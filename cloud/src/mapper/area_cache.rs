use std::{
    collections::{BTreeMap, HashMap, HashSet},
    sync::Arc,
};

use chrono::{DateTime, Utc};
use log::warn;

use crate::RoomAddress;
use crate::backends::source_document::{SourceDocument, room_data_address};
// The cache-editing helpers only tests use.
#[cfg(test)]
use crate::RoomUpdates;
use crate::{
    AREA_FORMAT_VERSION, Area, AreaAccess, AreaId, AreaWithDetails, AtlasId, Connection,
    ConnectionKind, ExitDirection, ExitId, Label, LabelId, MapPoint, RoomNumber, Shape, ShapeId,
    connection_geometry,
    connection_lifecycle::{self, ExitTopology, RoomSite},
    mapper::{
        exit_cache::ExitCache,
        room_cache::{PropertyEntry, RoomCache},
        room_connection::{RoomConnection, RoomConnectionEnd},
    },
    parse_css_color,
};
use crate::{SourceId, Uuid};
use rstar::{AABB, RTree, RTreeObject};

/// The rendered fallback when a Connection's stored color fails to parse —
/// the same gray as [`crate::DEFAULT_CONNECTION_COLOR`].
const DEFAULT_CONNECTION_ICED_COLOR: iced::Color = iced::Color::from_rgb8(164, 164, 164);

/// Cloud metadata for an area beyond its geometry: the viewer's access block,
/// owner attribution, atlas membership, and clone provenance.
#[derive(Debug, Clone, Default)]
pub struct AreaMeta {
    pub room_data: Arc<[crate::RoomData]>,
    pub access: Option<AreaAccess>,
    /// The area owner's user id (`Area.user_id`). Used to group shared rows
    /// by sharer/owner identity rather than by the display handle, which may
    /// be absent.
    pub owner_id: Option<uuid::Uuid>,
    pub owner_nickname: Option<String>,
    pub atlas_id: Option<AtlasId>,
    /// The atlas's display name, denormalized onto the area (§4.1 un-redaction
    /// delivers it to every viewer who can see the area). `None` when the source
    /// row carried no name — a caller that needs a label falls back to the area
    /// name or a generic phrase. Purely descriptive; confers no capability.
    pub atlas_name: Option<String>,
    /// The clan that owns the map, on a clan's own maps.
    pub clan_id: Option<uuid::Uuid>,
    /// The caller's clan map actions where clan authority reaches the map
    /// ([`crate::Area::actions`]).
    pub actions: Option<std::collections::BTreeSet<String>>,
    /// Whose a clan's map is ([`crate::Area::clan_ownership`]).
    pub clan_ownership: crate::clan_maps::ClanOwnership,
    pub copied_from_area_id: Option<AreaId>,
    pub copied_from_rev: Option<i64>,
    pub copied_at: Option<DateTime<Utc>>,
    /// The cloud's token over the caller's projection this copy came from.
    pub projection_token: Option<String>,
    /// The map's other readable sources, as served; shared, never merged.
    pub sources: Arc<[crate::SourceBundle]>,
}

/// Content owned by one source, with independently addressed room anchors.
/// `content` preserves real room addresses; attachments borrow readable anchor
/// geometry without joining the source's owned room list. `area` is the legacy
/// area-addressed view used by routing and scripts.
#[derive(Debug, Clone)]
pub struct SourceLayer {
    viewer: Option<Uuid>,
    source: SourceId,
    name: Option<String>,
    color: Option<[u8; 3]>,
    content: Arc<AreaCache>,
    area: Arc<AreaCache>,
    anchored: Arc<HashMap<crate::mapper::RoomKey, Vec<ExitCache>>>,
}

/// The area id a source of map `map_id` reads under: a Secret's own id, or,
/// for the caller's Private additions, one derived from the map and the
/// viewer, stable across sessions and different for every viewer. `None`
/// for the map itself.
#[must_use]
pub fn source_area_id(map_id: AreaId, source: SourceId, viewer: Option<Uuid>) -> Option<AreaId> {
    match source {
        SourceId::Map => None,
        SourceId::Secret(id) => Some(AreaId(id)),
        SourceId::Private => Some(AreaId(Uuid::new_v5(
            &map_id.0,
            viewer.unwrap_or_default().as_bytes(),
        ))),
    }
}

impl SourceLayer {
    #[must_use]
    pub fn source(&self) -> SourceId {
        self.source
    }

    /// The source as an area of its own (see the type's documentation).
    #[must_use]
    pub fn area(&self) -> &Arc<AreaCache> {
        &self.area
    }

    /// The area id the source reads under.
    #[must_use]
    pub fn area_id(&self) -> AreaId {
        self.area.id
    }

    /// The exits the source keeps on map room `map_room`, in the source
    /// area's addressing: into its own rooms by its area id, into map
    /// rooms by the map's.
    #[must_use]
    pub fn anchored_exits(&self, map_room: RoomNumber) -> &[ExitCache] {
        self.exits_on(&crate::mapper::RoomKey::new(self.content.id, map_room))
    }

    #[must_use]
    pub fn exits_on(&self, room: &crate::mapper::RoomKey) -> &[ExitCache] {
        self.anchored.get(room).map_or(&[], Vec::as_slice)
    }

    /// Every map room the source keeps exits on, with those exits.
    pub fn all_anchored_exits(
        &self,
    ) -> impl Iterator<Item = (&crate::mapper::RoomKey, &[ExitCache])> {
        self.anchored
            .iter()
            .map(|(room, exits)| (room, exits.as_slice()))
    }

    /// The anchored exits as shared, so a holder can tell an unchanged
    /// rebuild by pointer.
    pub(super) fn anchored_shared(&self) -> &Arc<HashMap<crate::mapper::RoomKey, Vec<ExitCache>>> {
        &self.anchored
    }

    /// A Secret's name; `None` for Private additions.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// A Secret's chosen color; `None` when the palette picks.
    #[must_use]
    pub fn color(&self) -> Option<[u8; 3]> {
        self.color
    }

    /// The source's content, laid out like a map.
    #[must_use]
    pub fn content(&self) -> &AreaCache {
        &self.content
    }

    /// The attachment/reference at a qualified room, if its anchor is readable.
    #[must_use]
    pub fn attachment(&self, address: RoomAddress) -> Option<&Arc<RoomCache>> {
        self.content.references.get(&address)
    }

    /// Translate to the legacy area-addressed API only at its boundary.
    #[must_use]
    pub fn room_key(&self, address: RoomAddress) -> crate::mapper::RoomKey {
        crate::mapper::RoomKey::new(
            source_area_id(self.content.id, address.source, self.viewer).unwrap_or(self.content.id),
            address.number,
        )
    }

    #[must_use]
    pub fn map_room_data(&self, number: RoomNumber) -> Option<&Arc<RoomCache>> {
        self.attachment(RoomAddress::map(number))
    }

    #[must_use]
    pub fn own_room(&self, number: RoomNumber) -> Option<&Arc<RoomCache>> {
        self.content.get_room(&number)
    }

    /// The source of a layer [`AreaCache::find_label`] and its siblings
    /// return: the map when there is none.
    #[must_use]
    pub fn source_of(layer: Option<&Self>) -> SourceId {
        layer.map_or(SourceId::Map, Self::source)
    }
}

#[derive(Debug, Clone)]
pub struct AreaCache {
    id: AreaId,
    name: String,
    rev: i64,
    meta: AreaMeta,
    rooms_by_number: HashMap<RoomNumber, Arc<RoomCache>>,
    rooms: Vec<Arc<RoomCache>>,
    /// Readable anchors with this document's attachment content. Never owned rooms.
    references: BTreeMap<RoomAddress, Arc<RoomCache>>,
    document_source: SourceId,
    /// The stored Connection rows, as projected (or locally maintained by
    /// the optimistic edit paths); [`Self::room_connections`] is resolved
    /// from these.
    connections: Vec<Connection>,
    room_connections: Vec<RoomConnection>,
    properties: HashMap<String, PropertyEntry>,
    labels: Vec<Label>,
    shapes: Vec<Shape>,
    max_room_number: RoomNumber,
    rooms_index: RTree<RoomSpatialEntry>,
    room_connections_index: RTree<ConnectionSpatialEntry>,
    /// Reverse lookups over this area's rooms by property and by tag.
    room_lookups: RoomLookups,
    /// The map's other readable sources, Secrets first by lowercased name,
    /// then the caller's Private additions.
    layers: Arc<[SourceLayer]>,
    /// Map-owned attachments can anchor on another source's rooms. Keep their
    /// drawing numbers out of the map's public room indexes.
    map_document: Option<Arc<SourceLayer>>,
    /// For a Secret's or Private additions' own area, the map it belongs
    /// to and the source it reads; `None` for a map.
    map: Option<(AreaId, SourceId)>,
}

/// The empty result an unmatched lookup borrows, so a miss allocates nothing
/// and every accessor can return a slice.
static EMPTY_ROOMS: Vec<Arc<RoomCache>> = Vec::new();

/// One area's reverse lookups over its rooms' properties and tags. Rebuilt
/// wholesale with the rest of the room state, next to the spatial R-trees and
/// at the same cost class, so every room-mutating path maintains them by
/// construction rather than by remembering to.
///
/// Properties are indexed twice — by name alone and by name with value —
/// mirroring `AtlasCache`'s `rooms_by_title` / `rooms_by_title_and_description`
/// pair, so "which rooms carry this at all" and "which rooms carry this exact
/// value" are both one probe. The second table costs a bucket per distinct
/// (name, value); a property holding a per-room identity spends one bucket per
/// room, which is precisely the case that wants the O(1) lookup.
///
/// Tags are keyed in their normalized (uppercase) spelling, so lookup is
/// case-insensitive on the same terms as [`RoomCache::has_tag`].
#[derive(Debug, Clone, Default)]
struct RoomLookups {
    by_property_name: HashMap<String, Vec<Arc<RoomCache>>>,
    by_property_name_and_value: HashMap<(String, String), Vec<Arc<RoomCache>>>,
    by_tag: HashMap<String, Vec<Arc<RoomCache>>>,
}

#[derive(Debug, Clone)]
struct RoomSpatialEntry {
    bounds: AABB<[f32; 2]>,
    room: Arc<RoomCache>,
}

impl RoomSpatialEntry {
    fn new(room: Arc<RoomCache>) -> Self {
        let bounds = AABB::from_corners([room.get_x(), room.get_y()], [room.get_x(), room.get_y()]);
        Self { bounds, room }
    }
}

impl RTreeObject for RoomSpatialEntry {
    type Envelope = AABB<[f32; 2]>;

    fn envelope(&self) -> Self::Envelope {
        self.bounds
    }
}

#[derive(Debug, Clone)]
struct ConnectionSpatialEntry {
    bounds: AABB<[f32; 2]>,
    index: usize,
}

impl ConnectionSpatialEntry {
    fn new(index: usize, connection: &RoomConnection) -> Self {
        let bounds = &connection.geometry.bounds;
        Self {
            bounds: AABB::from_corners([bounds.min_x, bounds.min_y], [bounds.max_x, bounds.max_y]),
            index,
        }
    }
}

impl RTreeObject for ConnectionSpatialEntry {
    type Envelope = AABB<[f32; 2]>;

    fn envelope(&self) -> Self::Envelope {
        self.bounds
    }
}

impl AreaCache {
    /// Builds the property and tag lookups from the area's rooms. Bucket order
    /// follows `rooms`, which preserves the area's original room ordering, so
    /// results are stable across rebuilds that do not reorder rooms.
    fn build_room_lookups(rooms: &[Arc<RoomCache>]) -> RoomLookups {
        let mut lookups = RoomLookups::default();
        for room in rooms {
            for (name, value) in room.properties() {
                lookups
                    .by_property_name
                    .entry(name.to_owned())
                    .or_default()
                    .push(room.clone());
                lookups
                    .by_property_name_and_value
                    .entry((name.to_owned(), value.to_owned()))
                    .or_default()
                    .push(room.clone());
            }
            for tag in room.tags() {
                lookups
                    .by_tag
                    .entry(tag.to_owned())
                    .or_default()
                    .push(room.clone());
            }
        }
        lookups
    }

    fn build_rooms_index(rooms: &[Arc<RoomCache>]) -> RTree<RoomSpatialEntry> {
        let entries: Vec<_> = rooms.iter().cloned().map(RoomSpatialEntry::new).collect();
        RTree::bulk_load(entries)
    }

    fn build_room_connections_index(
        room_connections: &[RoomConnection],
    ) -> RTree<ConnectionSpatialEntry> {
        let entries: Vec<_> = room_connections
            .iter()
            .enumerate()
            // An empty bounds is the inverted-infinity sentinel; it must
            // never reach the spatial index.
            .filter(|(_, connection)| !connection.geometry.bounds.is_empty())
            .map(|(index, connection)| ConnectionSpatialEntry::new(index, connection))
            .collect();
        RTree::bulk_load(entries)
    }

    fn rebuild_room_state(
        &self,
        rooms_by_number: HashMap<RoomNumber, Arc<RoomCache>>,
        rooms: Vec<Arc<RoomCache>>,
        max_room_number: RoomNumber,
        connections: Vec<Connection>,
    ) -> Self {
        if let Some(layer) = self.map_document_layer() {
            let mut details = self.to_details();
            details.rooms = rooms.iter().map(|room| room.to_details()).collect();
            details.connections = connections;
            details.area.rev += 1;
            return Self::for_viewer(details, layer.viewer, Some(self));
        }
        let room_connections =
            Self::build_room_connections(&self.id, &connections, &rooms_by_number);
        let rooms_index = Self::build_rooms_index(&rooms);
        let room_connections_index = Self::build_room_connections_index(&room_connections);
        let room_lookups = Self::build_room_lookups(&rooms);

        Self {
            rev: self.rev + 1,
            room_lookups,
            rooms_by_number,
            rooms,
            max_room_number,
            connections,
            room_connections,
            rooms_index,
            room_connections_index,
            ..self.clone()
        }
    }

    /// The cache of `area`, with its Private additions (if any) read under
    /// no viewer's id; see [`Self::for_viewer`].
    #[cfg(test)]
    pub(super) fn new_with_area(area: AreaWithDetails) -> Self {
        Self::for_viewer(area, None, None)
    }

    /// The cache of `area` as `viewer` reads it: the viewer's Private
    /// additions take the area id derived from the map and the viewer (see
    /// [`source_area_id`]). Each source whose bundle matches `previous`'s
    /// keeps `previous`'s own area for it, so an edit of the map does not
    /// rebuild (or re-index) every Secret.
    pub(super) fn for_viewer(
        area: AreaWithDetails,
        viewer: Option<Uuid>,
        previous: Option<&Self>,
    ) -> Self {
        let layers = Self::build_source_layers(&area, viewer, previous);
        let map_document = Self::build_map_document(&area, viewer);
        let mut cache = Self::from_content(area, viewer, SourceId::Map);
        cache.layers = layers;
        cache.map_document = map_document;
        cache
    }

    fn from_content(area: AreaWithDetails, viewer: Option<Uuid>, source: SourceId) -> Self {
        let max_room_number = area
            .rooms
            .iter()
            .map(|r| r.room_number)
            .max()
            .unwrap_or(RoomNumber(0));

        let rooms: Vec<Arc<RoomCache>> = area
            .rooms
            .into_iter()
            .map(|room| {
                Arc::new(
                    RoomCache::from(room)
                        .with_viewer(viewer)
                        .with_source(source),
                )
            })
            .collect();
        let rooms_by_number = rooms
            .iter()
            .map(|r| (r.get_room_number(), r.clone()))
            .collect();
        let properties = area
            .properties
            .into_iter()
            .map(|p| (p.name, PropertyEntry { value: p.value }))
            .collect();

        let room_connections =
            Self::build_room_connections(&area.area.id, &area.connections, &rooms_by_number);
        let rooms_index = Self::build_rooms_index(&rooms);
        let room_connections_index = Self::build_room_connections_index(&room_connections);
        let room_lookups = Self::build_room_lookups(&rooms);

        Self {
            id: area.area.id,
            name: area.area.name,
            rev: area.area.rev,
            meta: AreaMeta {
                room_data: area.room_data.into(),
                access: area.area.access,
                owner_id: area.area.user_id,
                owner_nickname: area.area.owner_nickname,
                atlas_id: area.area.atlas_id,
                atlas_name: area.area.atlas_name,
                clan_id: area.area.clan_id,
                actions: area.area.actions,
                clan_ownership: area.area.clan_ownership,
                copied_from_area_id: area.area.copied_from_area_id,
                copied_from_rev: area.area.copied_from_rev,
                copied_at: area.area.copied_at,
                projection_token: area.area.projection_token,
                sources: area.sources.into(),
            },
            rooms,
            rooms_by_number,
            max_room_number,
            properties,
            labels: area.labels,
            shapes: area.shapes,
            connections: area.connections,
            room_connections,
            rooms_index,
            room_connections_index,
            room_lookups,
            layers: Arc::from([]),
            map_document: None,
            map: None,
            references: BTreeMap::new(),
            document_source: source,
        }
    }

    fn reference_addresses<'a>(
        room_data: &'a [crate::RoomData],
        connections: &'a [Connection],
        source: SourceId,
    ) -> impl Iterator<Item = RoomAddress> + 'a {
        room_data
            .iter()
            .map(room_data_address)
            .chain(connections.iter().flat_map(|connection| {
                std::iter::once(connection.endpoint_a)
                    .chain(connection.endpoint_b)
                    .map(crate::ConnectionEndpoint::address)
            }))
            .filter(move |address| address.source != source)
    }

    fn for_source(
        details: &AreaWithDetails,
        source: SourceId,
        viewer: Option<Uuid>,
    ) -> Option<Self> {
        let mut document = SourceDocument::open(details, source).ok()?;
        let addresses: HashSet<_> = Self::reference_addresses(
            &document.content.room_data,
            &document.content.connections,
            source,
        )
        .collect();
        let room_data = std::mem::take(&mut document.content.room_data);
        let room_data_by_address: HashMap<_, _> = room_data
            .iter()
            .map(|data| (room_data_address(data), data))
            .collect();
        let connections = std::mem::take(&mut document.content.connections);
        let mut cache = Self::from_content(document.content, viewer, source);
        cache.references = addresses
            .into_iter()
            .filter_map(|address| {
                let site = document.context.anchors.get(&address)?;
                let data = room_data_by_address.get(&address);
                let room = crate::RoomWithDetails {
                    room_number: address.number,
                    x: site.x,
                    y: site.y,
                    level: site.level,
                    properties: data.map_or_else(Vec::new, |data| data.properties.clone()),
                    tags: data.map_or_else(Default::default, |data| data.tags.clone()),
                    exits: data.map_or_else(Vec::new, |data| data.exits.clone()),
                    title: String::new(),
                    description: String::new(),
                    color: String::new(),
                    external_id: None,
                };
                Some((
                    address,
                    Arc::new(
                        RoomCache::from(room)
                            .with_viewer(viewer)
                            .with_source(address.source),
                    ),
                ))
            })
            .collect();
        cache.meta.room_data = room_data.into();
        cache.connections = connections;
        let rooms = cache
            .document_rooms()
            .map(|room| (room.address(), room.clone()))
            .collect();
        cache.room_connections =
            Self::build_qualified_connections(&cache.id, &cache.connections, &rooms);
        cache.room_connections_index = Self::build_room_connections_index(&cache.room_connections);
        cache.room_lookups =
            Self::build_room_lookups(&cache.document_rooms().cloned().collect::<Vec<_>>());
        Some(cache)
    }

    #[must_use]
    pub fn get_room_at(&self, address: RoomAddress) -> Option<&Arc<RoomCache>> {
        if address.source == self.document_source {
            self.get_room(&address.number)
        } else {
            self.references.get(&address).or_else(|| {
                self.map_document
                    .as_ref()
                    .and_then(|layer| layer.content.get_room_at(address))
            })
        }
    }

    /// The source owning the content in this cache.
    #[must_use]
    pub fn document_source(&self) -> SourceId {
        self.document_source
    }

    /// Owned rooms and readable attachment anchors for content queries.
    /// Geometry editing and room selection must use `get_rooms` instead.
    pub fn document_rooms(&self) -> impl Iterator<Item = &Arc<RoomCache>> {
        self.rooms.iter().chain(self.references.values())
    }

    fn build_map_document(
        area: &AreaWithDetails,
        viewer: Option<Uuid>,
    ) -> Option<Arc<SourceLayer>> {
        let qualified = |endpoint: &crate::ConnectionEndpoint| {
            endpoint.source.is_some_and(|source| !source.is_map())
        };
        if area.room_data.is_empty()
            && !area.connections.iter().any(|connection| {
                qualified(&connection.endpoint_a)
                    || connection.endpoint_b.as_ref().is_some_and(qualified)
            })
        {
            return None;
        }
        let content = Arc::new(Self::for_source(area, SourceId::Map, viewer)?);
        let map = area.area.id;
        let anchored = area
            .room_data
            .iter()
            .map(|data| {
                let key = crate::mapper::RoomKey::new(
                    source_area_id(map, data.room_source.unwrap_or(SourceId::Map), viewer)
                        .unwrap_or(map),
                    data.room_number,
                );
                let exits = data
                    .exits
                    .iter()
                    .cloned()
                    .map(|exit| ExitCache::from(exit).with_viewer(viewer))
                    .collect();
                (key, exits)
            })
            .collect();
        Some(Arc::new(SourceLayer {
            viewer,
            source: SourceId::Map,
            name: None,
            color: None,
            area: content.clone(),
            content,
            anchored: Arc::new(anchored),
        }))
    }

    /// Map content with separately stored, source-qualified attachment anchors.
    #[must_use]
    pub fn map_document_layer(&self) -> Option<&SourceLayer> {
        self.map_document.as_deref()
    }

    /// The content document for a source. Plain Map content uses the map cache.
    #[must_use]
    pub fn document_layer(&self, source: SourceId) -> Option<&SourceLayer> {
        if source.is_map() {
            self.map_document_layer()
        } else {
            self.layers.iter().find(|layer| layer.source() == source)
        }
    }

    /// Each of the map's other sources as a layer, in color order: Secrets
    /// by lowercased name (ties by name, then id), then Private additions.
    fn build_source_layers(
        area: &AreaWithDetails,
        viewer: Option<Uuid>,
        previous: Option<&Self>,
    ) -> Arc<[SourceLayer]> {
        if area.sources.is_empty() {
            return Arc::from([]);
        }
        let sites = previous.map_or_else(HashMap::new, |_| {
            crate::backends::source_document::room_sites(area)
        });
        let mut layers: Vec<SourceLayer> = area
            .sources
            .iter()
            .filter_map(|bundle| {
                let area_id = source_area_id(area.area.id, bundle.source, viewer)?;
                let reused = previous
                    .and_then(|previous| previous.reusable_layer(area, bundle, area_id, &sites));
                let (own_area, anchored) = match reused {
                    // Nothing it draws over moved either.
                    Some((layer, true)) => return Some(layer.clone()),
                    Some((layer, false)) => (layer.area.clone(), layer.anchored.clone()),
                    None => {
                        let (own_area, anchored) = Self::source_area(area, bundle, area_id, viewer);
                        (Arc::new(own_area), Arc::new(anchored))
                    }
                };
                let content = Arc::new(Self::for_source(area, bundle.source, viewer)?);
                Some(SourceLayer {
                    viewer,
                    source: bundle.source,
                    name: bundle.name.clone(),
                    color: bundle.rgb(),
                    content,
                    area: own_area,
                    anchored,
                })
            })
            .collect();
        layers.sort_by(|a, b| {
            let key = |layer: &SourceLayer| {
                (
                    !layer.source.is_secret(),
                    layer.name.as_deref().map(str::to_lowercase),
                    layer.name.clone(),
                    layer.source,
                )
            };
            key(a).cmp(&key(b))
        });
        layers.into()
    }

    /// The map's other readable sources as layers: Secrets first by
    /// lowercased name, then the caller's Private additions.
    #[must_use]
    pub fn source_layers(&self) -> &[SourceLayer] {
        &self.layers
    }

    /// The layer of the source reading under `area_id`, if this map has it.
    #[must_use]
    pub fn source_layer(&self, area_id: &AreaId) -> Option<&SourceLayer> {
        self.layers.iter().find(|layer| layer.area.id == *area_id)
    }

    /// For a Secret's or Private additions' own area, the map it belongs
    /// to; `None` for a map.
    #[must_use]
    pub fn map_id(&self) -> Option<AreaId> {
        self.map.map(|(map, _)| map)
    }

    /// The place this area's own content is: the map for a map, or the
    /// Secret or Private additions whose own area this is.
    #[must_use]
    pub fn place(&self) -> SourceId {
        self.map.map_or(SourceId::Map, |(_, source)| source)
    }

    /// This map's layer for `bundle`, when `details` changes nothing it was
    /// built from: the same bundle under the same area id, on a map with the
    /// same owner, access and folder. The flag says whether its drawing
    /// holds too: every referenced anchor has the same availability and position.
    fn reusable_layer(
        &self,
        details: &AreaWithDetails,
        bundle: &crate::SourceBundle,
        area_id: AreaId,
        sites: &HashMap<RoomAddress, RoomSite>,
    ) -> Option<(&SourceLayer, bool)> {
        let layer = self
            .layers
            .iter()
            .find(|layer| layer.source == bundle.source && layer.area.id == area_id)?;
        let served = self
            .meta
            .sources
            .iter()
            .find(|served| served.source == bundle.source)?;
        let same_map = self.meta.access == details.area.access
            && self.meta.owner_id == details.area.user_id
            && self.meta.owner_nickname == details.area.owner_nickname
            && self.meta.atlas_id == details.area.atlas_id
            && self.meta.atlas_name == details.area.atlas_name
            && self.meta.clan_id == details.area.clan_id
            && self.meta.actions == details.area.actions
            && self.meta.clan_ownership == details.area.clan_ownership;
        if served != bundle || !same_map {
            return None;
        }
        let drawing_holds =
            Self::reference_addresses(&bundle.room_data, &bundle.connections, bundle.source).all(
                |address| {
                    let site = sites.get(&address).map(|site| (site.x, site.y, site.level));
                    let old = layer
                        .content
                        .references
                        .get(&address)
                        .map(|room| (room.get_x(), room.get_y(), room.get_level()));
                    old == site
                },
            );
        Some((layer, drawing_holds))
    }

    /// `bundle`, a source of map `map`, as an area of its own under
    /// `area_id`: its own rooms, properties, labels and shapes, and the
    /// connections among its own rooms. Exits into its own rooms name
    /// `area_id`; exits into map rooms keep naming the map. The exits it
    /// keeps on map rooms come back beside the area, by map room.
    fn source_area(
        map: &AreaWithDetails,
        bundle: &crate::SourceBundle,
        area_id: AreaId,
        viewer: Option<Uuid>,
    ) -> (Self, HashMap<crate::mapper::RoomKey, Vec<ExitCache>>) {
        let map_id = map.area.id;
        // Only an exit into another map's Secret room keeps its `to_source`.
        let readdress = |mut exit: crate::Exit| {
            if exit.to_area_id == Some(map_id) {
                exit.to_area_id = Some(
                    source_area_id(map_id, exit.to_source.unwrap_or(SourceId::Map), viewer)
                        .unwrap_or(map_id),
                );
                exit.to_source = None;
            }
            exit
        };
        let rooms = bundle
            .rooms
            .iter()
            .cloned()
            .map(|mut room| {
                room.exits = room.exits.into_iter().map(readdress).collect();
                room
            })
            .collect();
        let anchored = bundle
            .room_data
            .iter()
            .filter(|data| !data.exits.is_empty())
            .map(|data| {
                let exits = data
                    .exits
                    .iter()
                    .cloned()
                    .map(|exit| ExitCache::from(readdress(exit)).with_viewer(viewer))
                    .collect();
                (
                    crate::mapper::RoomKey::new(
                        source_area_id(map_id, data.room_source.unwrap_or(SourceId::Map), viewer)
                            .unwrap_or(map_id),
                        data.room_number,
                    ),
                    exits,
                )
            })
            .collect();
        let own = |endpoint: &crate::ConnectionEndpoint| endpoint.source == Some(bundle.source);
        let connections = bundle
            .connections
            .iter()
            .filter(|connection| {
                own(&connection.endpoint_a) && connection.endpoint_b.as_ref().is_none_or(own)
            })
            .cloned()
            .map(|mut connection| {
                connection.endpoint_a.source = None;
                if let Some(endpoint) = &mut connection.endpoint_b {
                    endpoint.source = None;
                }
                connection
            })
            .collect();
        let writable = ["add", "edit", "remove"]
            .iter()
            .any(|action| bundle.can(action));
        let access = AreaAccess {
            can_edit: writable,
            can_reshare: false,
            can_copy: false,
            ..map.area.effective_access()
        };
        let details = AreaWithDetails {
            room_data: Vec::new(),
            area: Area {
                id: area_id,
                user_id: map.area.user_id,
                atlas_id: map.area.atlas_id,
                atlas_name: map.area.atlas_name.clone(),
                name: bundle.name.clone().unwrap_or_else(|| "Private".to_string()),
                created_at: map.area.created_at,
                rev: bundle.rev,
                projection_token: None,
                access: Some(access),
                owner_nickname: map.area.owner_nickname.clone(),
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
                clan_id: None,
                clan_name: None,
                actions: None,
                clan_ownership: crate::clan_maps::ClanOwnership::default(),
            },
            format_version: map.format_version,
            properties: bundle.properties.clone(),
            rooms,
            labels: bundle.labels.clone(),
            shapes: bundle.shapes.clone(),
            connections,
            linked_areas: Vec::new(),
            sources: Vec::new(),
        };
        let mut area = Self::for_viewer(details, viewer, None);
        area.map = Some((map_id, bundle.source));
        (area, anchored)
    }

    /// This area's text labels.
    #[must_use]
    pub fn labels(&self) -> &[Label] {
        &self.labels
    }

    /// This area's graphical shapes.
    #[must_use]
    pub fn shapes(&self) -> &[Shape] {
        &self.shapes
    }

    /// Resolves the stored `connections` array into render views: for each
    /// Connection, look up its endpoint rooms and member exits, resolve the
    /// geometry exactly once, and derive the special-kind end facts from
    /// visible topology. A Connection whose projection is corrupt (missing
    /// endpoint room, no member exit) is skipped with a warning — never a
    /// panic. Cross-level Connections emit two halves, one per endpoint
    /// level, sharing one geometry [`Arc`].
    fn build_room_connections(
        area_id: &AreaId,
        connections: &[Connection],
        rooms_by_number: &HashMap<RoomNumber, Arc<RoomCache>>,
    ) -> Vec<RoomConnection> {
        let rooms = rooms_by_number
            .values()
            .map(|room| (room.address(), room.clone()))
            .collect();
        Self::build_qualified_connections(area_id, connections, &rooms)
    }

    #[allow(clippy::too_many_lines)]
    fn build_qualified_connections(
        area_id: &AreaId,
        connections: &[Connection],
        rooms_by_number: &HashMap<RoomAddress, Arc<RoomCache>>,
    ) -> Vec<RoomConnection> {
        // One walk of the rooms builds the connection_id → members map.
        let mut members_by_connection: HashMap<
            crate::ConnectionId,
            Vec<(&Arc<RoomCache>, &ExitCache)>,
        > = HashMap::new();
        for room in rooms_by_number.values() {
            for exit in room.get_exits() {
                members_by_connection
                    .entry(exit.connection_id)
                    .or_default()
                    .push((room, exit));
            }
        }

        let mut room_connections = Vec::with_capacity(connections.len());
        for connection in connections {
            let Some(room_a) = rooms_by_number.get(&connection.endpoint_a.address()) else {
                warn!(
                    "area {area_id}: connection {} endpoint room {} missing from the projection; skipping",
                    connection.id, connection.endpoint_a.room_number.0
                );
                continue;
            };
            let room_b = if let Some(endpoint) = &connection.endpoint_b {
                let Some(room) = rooms_by_number.get(&endpoint.address()) else {
                    warn!(
                        "area {area_id}: connection {} endpoint room {} missing from the projection; skipping",
                        connection.id, endpoint.room_number.0
                    );
                    continue;
                };
                Some(room)
            } else {
                None
            };
            let members = members_by_connection
                .get(&connection.id)
                .map_or(&[][..], Vec::as_slice);
            let Some(&(member_room, member_exit)) = members.first() else {
                warn!(
                    "area {area_id}: connection {} has no member exit; skipping",
                    connection.id
                );
                continue;
            };

            let direction_a = Self::direction_at(
                members,
                connection.endpoint_a.address(),
                Some(connection.endpoint_a.side),
            );
            let stub_a = connection_geometry::StubAxis::for_direction(direction_a);
            let direction_b = connection.endpoint_b.as_ref().map(|endpoint| {
                Self::direction_at(members, endpoint.address(), Some(endpoint.side))
            });
            let stub_b = direction_b.map_or(
                connection_geometry::StubAxis::Normal,
                connection_geometry::StubAxis::for_direction,
            );
            let geometry = Arc::new(connection_geometry::resolve(
                &connection_geometry::GeometryInput {
                    kind: connection.kind,
                    routing: connection.routing,
                    corner: connection.corner,
                    endpoint_a: connection_geometry::EndpointGeometry {
                        room_center: MapPoint::new(room_a.get_x(), room_a.get_y()),
                        side: connection.endpoint_a.side,
                        port_offset: connection.endpoint_a.port_offset,
                        stub: stub_a,
                    },
                    endpoint_b: connection.endpoint_b.as_ref().zip(room_b).map(
                        |(endpoint, room)| connection_geometry::EndpointGeometry {
                            room_center: MapPoint::new(room.get_x(), room.get_y()),
                            side: endpoint.side,
                            port_offset: endpoint.port_offset,
                            stub: stub_b,
                        },
                    ),
                    route_points: &connection.route_points,
                    thickness: connection.thickness,
                },
            ));

            let is_bidirectional = members.len() == 2;
            let arrow_toward_b = if is_bidirectional || connection.kind == ConnectionKind::SelfLoop
            {
                None
            } else {
                Some(member_room.address() == connection.endpoint_a.address())
            };
            let door = members
                .iter()
                .filter_map(|(_, exit)| exit.door.as_ref().map(|door| door.state))
                .max();
            let base = RoomConnection {
                connection_id: connection.id,
                from_level: room_a.get_level(),
                geometry: geometry.clone(),
                kind: connection.kind,
                routing: connection.routing,
                dash: connection.dash,
                corner: connection.corner,
                thickness: connection.thickness,
                stub_a,
                stub_b,
                direction_a,
                direction_b,
                color: parse_css_color(&connection.color).unwrap_or(DEFAULT_CONNECTION_ICED_COLOR),
                is_bidirectional,
                arrow_toward_b,
                door,
                to: RoomConnectionEnd::None,
                room: (*room_a).clone(),
                source: connection.clone(),
                endpoint_a_room: (*room_a).clone(),
                endpoint_b_room: room_b.cloned(),
                layout_spacing: 1.0,
            };

            match connection.kind {
                ConnectionKind::SelfLoop => {
                    room_connections.push(RoomConnection {
                        to: RoomConnectionEnd::SelfLoop,
                        ..base
                    });
                }
                ConnectionKind::Internal => {
                    let (Some(room_b), Some(_)) = (room_b, connection.endpoint_b) else {
                        warn!(
                            "area {area_id}: internal connection {} without endpoint B; skipping",
                            connection.id
                        );
                        continue;
                    };
                    room_connections.push(RoomConnection {
                        to: RoomConnectionEnd::Normal {
                            direction: direction_b.unwrap_or(direction_a),
                            x: room_b.get_x(),
                            y: room_b.get_y(),
                            room: (*room_b).clone(),
                        },
                        ..base
                    });
                }
                ConnectionKind::CrossLevel => {
                    let (Some(room_b), Some(_)) = (room_b, connection.endpoint_b) else {
                        warn!(
                            "area {area_id}: cross-level connection {} without endpoint B; skipping",
                            connection.id
                        );
                        continue;
                    };
                    // Two halves — one per endpoint room's level — sharing
                    // the one resolved geometry.
                    room_connections.push(RoomConnection {
                        to: RoomConnectionEnd::ToLevel {
                            level: room_b.get_level(),
                            direction: direction_a,
                            x: room_b.get_x(),
                            y: room_b.get_y(),
                            room: (*room_b).clone(),
                        },
                        ..base.clone()
                    });
                    room_connections.push(RoomConnection {
                        from_level: room_b.get_level(),
                        room: (*room_b).clone(),
                        to: RoomConnectionEnd::ToLevel {
                            level: room_a.get_level(),
                            direction: direction_b.unwrap_or(direction_a),
                            x: room_a.get_x(),
                            y: room_a.get_y(),
                            room: (*room_a).clone(),
                        },
                        ..base
                    });
                }
                ConnectionKind::Dangling => {
                    let to = if member_exit.to_unknown {
                        // Redacted destination: must render as "Unknown map",
                        // never as a plain dangling exit.
                        RoomConnectionEnd::Unknown {
                            token: member_exit.to_area_token.clone().unwrap_or_default(),
                        }
                    } else {
                        RoomConnectionEnd::None
                    };
                    room_connections.push(RoomConnection { to, ..base });
                }
                ConnectionKind::External => {
                    let to = if member_exit.to_unknown {
                        RoomConnectionEnd::Unknown {
                            token: member_exit.to_area_token.clone().unwrap_or_default(),
                        }
                    } else if let Some(to_area_id) = member_exit.to_area_id {
                        RoomConnectionEnd::External {
                            area_id: to_area_id,
                        }
                    } else {
                        // The member lost its destination without the kind
                        // catching up (mid-edit projection); degrade to a
                        // bare stub rather than invent a destination.
                        RoomConnectionEnd::None
                    };
                    room_connections.push(RoomConnection { to, ..base });
                }
            }
        }

        room_connections
    }

    /// The compass direction a Connection anchors on at `room`: the member
    /// exit originating there knows it directly (`from_direction`); a member
    /// arriving there knows it as its `to_direction` (or the opposite of its
    /// origin direction); with neither, the endpoint's wall side stands in.
    fn direction_at(
        members: &[(&Arc<RoomCache>, &ExitCache)],
        room: RoomAddress,
        side: Option<crate::RoomSide>,
    ) -> ExitDirection {
        for (member_room, exit) in members {
            if member_room.address() == room {
                return exit.from_direction;
            }
        }
        for (_, exit) in members {
            if exit
                .destination_address()
                .is_some_and(|destination| destination.room == room)
            {
                return exit
                    .to_direction
                    .unwrap_or_else(|| exit.from_direction.opposite());
            }
        }
        match side {
            Some(crate::RoomSide::North) => ExitDirection::North,
            Some(crate::RoomSide::East) => ExitDirection::East,
            Some(crate::RoomSide::South) => ExitDirection::South,
            Some(crate::RoomSide::West) | None => ExitDirection::West,
        }
    }

    #[must_use]
    pub fn get_id(&self) -> &AreaId {
        &self.id
    }

    #[must_use]
    pub fn get_room(&self, room_number: &RoomNumber) -> Option<&Arc<RoomCache>> {
        self.rooms_by_number.get(room_number)
    }

    /// This area's rooms carrying a property named `name`, whatever its value.
    /// One hash probe; the slice is the index bucket itself, never a copy.
    /// Order is unspecified but stable for an unchanged area.
    #[must_use]
    pub fn get_rooms_with_property(&self, name: &str) -> &[Arc<RoomCache>] {
        self.room_lookups
            .by_property_name
            .get(name)
            .unwrap_or(&EMPTY_ROOMS)
    }

    /// This area's rooms whose `name` property holds exactly `value`. Name and
    /// value both match case-sensitively, on the same terms as
    /// [`RoomCache::get_property`]. One hash probe.
    #[must_use]
    pub fn get_rooms_by_property(&self, name: &str, value: &str) -> &[Arc<RoomCache>] {
        // The tuple key is owned, so probing allocates the pair — the same
        // trade `AtlasCache`'s title-and-description lookup already makes, and
        // negligible beside the work a caller does with the result.
        self.room_lookups
            .by_property_name_and_value
            .get(&(name.to_owned(), value.to_owned()))
            .unwrap_or(&EMPTY_ROOMS)
    }

    /// This area's rooms carrying `tag`, matched case-insensitively like
    /// [`RoomCache::has_tag`]. One hash probe.
    #[must_use]
    pub fn get_rooms_with_tag(&self, tag: &str) -> &[Arc<RoomCache>] {
        self.room_lookups
            .by_tag
            .get(&crate::mapper::normalize_tag(tag))
            .unwrap_or(&EMPTY_ROOMS)
    }

    /// This area's own properties, for callers that need to compare two
    /// snapshots' property sets rather than read one.
    #[must_use]
    pub(super) fn property_entries(&self) -> &HashMap<String, PropertyEntry> {
        &self.properties
    }

    #[must_use]
    pub fn get_rooms(&self) -> &[Arc<RoomCache>] {
        &self.rooms
    }

    #[must_use]
    pub fn get_name(&self) -> &str {
        self.name.as_str()
    }

    #[must_use]
    pub(super) fn rename(&self, name: &str) -> Self {
        Self {
            name: name.to_string(),
            rev: self.rev + 1,
            ..self.clone()
        }
    }

    pub(super) fn with_local_metadata(&self, area: &crate::Area) -> Self {
        let mut updated = self.clone();
        updated.name.clone_from(&area.name);
        updated.meta.atlas_id = area.atlas_id;
        updated.meta.atlas_name.clone_from(&area.atlas_name);
        updated.rev = (self.rev + 1).max(area.rev);
        updated
    }

    pub(super) fn advance_revision(&mut self, revision: i64) {
        self.rev = self.rev.max(revision + 1);
    }

    /// Returns a copy filed into `atlas_id` (`Some`) or pulled loose
    /// (`None`). Bumps `rev` like other local edits so an open editor on the
    /// area notices; the folder regrouping itself is read fresh from
    /// `meta().atlas_id` by the area list.
    #[must_use]
    pub(super) fn with_atlas(&self, atlas_id: Option<AtlasId>) -> Self {
        let mut meta = self.meta.clone();
        meta.atlas_id = atlas_id;
        Self {
            meta,
            rev: self.rev + 1,
            ..self.clone()
        }
    }

    #[must_use]
    pub fn get_property(&self, name: &str) -> Option<&str> {
        self.properties.get(name).map(|p| p.value.as_str())
    }

    /// Iterates all properties in unspecified order; sort in the caller when
    /// stable ordering matters.
    pub fn properties(&self) -> impl Iterator<Item = (&str, &str)> {
        self.properties
            .iter()
            .map(|(k, v)| (k.as_str(), v.value.as_str()))
    }

    #[must_use]
    pub fn get_rev(&self) -> i64 {
        self.rev
    }

    /// Cloud metadata: access block, owner handle, provenance, atlas id.
    #[must_use]
    pub fn meta(&self) -> &AreaMeta {
        &self.meta
    }

    /// The clan a Clan Secret on a map filed in that clan by link belongs
    /// to; `None` for every other source, a clan's Secret on one of the
    /// clan's own maps included. Such a Secret holds only its own content:
    /// no data on the map's rooms, no exits into them, and no moves to or
    /// from the map's own sources.
    #[must_use]
    pub fn linked_clan_of(&self, source: SourceId) -> Option<uuid::Uuid> {
        if !source.is_secret() {
            return None;
        }
        self.meta
            .sources
            .iter()
            .find(|bundle| bundle.source == source)
            .and_then(|bundle| bundle.clan_id)
            .filter(|clan| self.meta.clan_id != Some(*clan))
    }

    /// Whether `source` is a clan's Secret on a map filed in the clan by
    /// link (see [`Self::linked_clan_of`]).
    #[must_use]
    pub fn keeps_only_own_rooms(&self, source: SourceId) -> bool {
        self.linked_clan_of(source).is_some()
    }

    /// The viewer's capabilities on this area; see [`AreaAccess::effective`].
    #[must_use]
    pub fn effective_access(&self) -> AreaAccess {
        AreaAccess::effective(
            self.meta.access,
            self.meta.clan_id,
            self.meta.actions.as_ref(),
        )
    }

    /// Whether the viewer owns this area (shared areas and clans' maps
    /// return false).
    #[must_use]
    pub fn is_owned(&self) -> bool {
        self.effective_access().is_owner
    }

    #[must_use]
    pub fn room_count(&self) -> usize {
        self.rooms.len()
    }

    /// The next unused room number in this area.
    #[must_use]
    pub fn next_room_number(&self) -> RoomNumber {
        RoomNumber(self.max_room_number.0 + 1)
    }

    /// Keeps an exhausted allocation cursor representable for merge planning
    /// and reservations, without wrapping into an occupied room number.
    pub(crate) fn room_number_floor(&self) -> i64 {
        i64::from(self.max_room_number.0) + 1
    }

    #[must_use]
    pub fn get_labels(&self) -> &[Label] {
        &self.labels
    }

    #[must_use]
    pub fn get_shapes(&self) -> &[Shape] {
        &self.shapes
    }

    #[must_use]
    pub fn get_label(&self, label_id: &LabelId) -> Option<&Label> {
        self.labels.iter().find(|l| &l.id == label_id)
    }

    #[must_use]
    pub fn get_shape(&self, shape_id: &ShapeId) -> Option<&Shape> {
        self.shapes.iter().find(|s| &s.id == shape_id)
    }

    /// A label in whichever source holds it, with the layer holding it
    /// (`None` for the map's own). Label ids are unique across a map's
    /// sources.
    #[must_use]
    pub fn find_label(&self, label_id: &LabelId) -> Option<(Option<&SourceLayer>, &Label)> {
        self.get_label(label_id)
            .map(|label| (None, label))
            .or_else(|| {
                self.layers.iter().find_map(|layer| {
                    layer
                        .content
                        .get_label(label_id)
                        .map(|label| (Some(layer), label))
                })
            })
    }

    /// [`Self::find_label`] for shapes.
    #[must_use]
    pub fn find_shape(&self, shape_id: &ShapeId) -> Option<(Option<&SourceLayer>, &Shape)> {
        self.get_shape(shape_id)
            .map(|shape| (None, shape))
            .or_else(|| {
                self.layers.iter().find_map(|layer| {
                    layer
                        .content
                        .get_shape(shape_id)
                        .map(|shape| (Some(layer), shape))
                })
            })
    }

    /// The document holding connection `id`, with the layer it belongs to
    /// (`None` for the map's own): this area, or that layer's content. Its
    /// rooms retain their source-qualified addresses.
    #[must_use]
    pub fn connection_document(
        &self,
        id: crate::ConnectionId,
    ) -> Option<(&Self, Option<&SourceLayer>)> {
        let (layer, _) = self.find_connection(id)?;
        Some((layer.map_or(self, SourceLayer::content), layer))
    }

    /// [`Self::find_label`] for connections; a layer's connection names its
    /// source-qualified room addresses.
    #[must_use]
    pub fn find_connection(
        &self,
        id: crate::ConnectionId,
    ) -> Option<(Option<&SourceLayer>, &Connection)> {
        self.map_document_layer()
            .map_or_else(
                || self.get_connection(id).map(|connection| (None, connection)),
                |layer| {
                    layer
                        .content()
                        .get_connection(id)
                        .map(|connection| (Some(layer), connection))
                },
            )
            .or_else(|| {
                self.layers.iter().find_map(|layer| {
                    layer
                        .content
                        .get_connection(id)
                        .map(|connection| (Some(layer), connection))
                })
            })
    }

    #[cfg(test)]
    pub(super) fn set_property(&self, name: String, value: String) -> Self {
        let mut new_properties = self.properties.clone();
        new_properties.insert(name, PropertyEntry { value });

        Self {
            properties: new_properties,
            rev: self.rev + 1,
            ..self.clone()
        }
    }

    #[cfg(test)]
    pub(super) fn upsert_room(&self, room_number: RoomNumber, updates: RoomUpdates) -> Self {
        let room = if let Some(room) = self.rooms_by_number.get(&room_number) {
            Arc::new(room.apply_updates(updates))
        } else {
            Arc::new(RoomCache::new(room_number).apply_updates(updates))
        };

        self.upsert_room_cache(room_number, room)
    }

    #[cfg(test)]
    fn upsert_room_cache(&self, room_number: RoomNumber, room: Arc<RoomCache>) -> Self {
        self.upsert_room_cache_with_connections(room_number, room, self.connections.clone())
    }

    #[cfg(test)]
    /// [`Self::upsert_room_cache`] for the edit paths that also changed the
    /// stored Connection rows (exit lifecycle).
    fn upsert_room_cache_with_connections(
        &self,
        room_number: RoomNumber,
        room: Arc<RoomCache>,
        connections: Vec<Connection>,
    ) -> Self {
        let mut new_rooms_by_number = self.rooms_by_number.clone();
        let mut new_rooms = self.rooms.clone();

        new_rooms_by_number.insert(room_number, room.clone());
        new_rooms.retain(|r| r.get_room_number() != room_number);
        new_rooms.push(room);
        let max_room_number = RoomNumber(room_number.0.max(self.max_room_number.0));

        self.rebuild_room_state(new_rooms_by_number, new_rooms, max_room_number, connections)
    }

    #[cfg(test)]
    pub(super) fn delete_room(&self, room_number: RoomNumber) -> Self {
        let mut new_rooms_by_number = self.rooms_by_number.clone();
        new_rooms_by_number.remove(&room_number);
        let mut new_rooms = self.rooms.clone();
        new_rooms.retain(|r| r.get_room_number() != room_number);

        let max_room_number = if self.max_room_number == room_number {
            new_rooms
                .iter()
                .map(|r| r.get_room_number())
                .max()
                .unwrap_or(RoomNumber(0))
        } else {
            self.max_room_number
        };

        // §3.3: Connections orphaned by the room's outgoing exits are
        // deleted; those kept alive by a surviving inbound member become
        // dangling. (The inbound destinations themselves are nulled by the
        // caller's follow-up `null_inbound_exits` cascade.)
        let mut connections = self.connections.clone();
        let survivors = Self::topologies_of(&self.id, &new_rooms_by_number, None);
        connection_lifecycle::repair_after_room_delete(
            room_number.into(),
            &survivors,
            &mut connections,
        );

        self.rebuild_room_state(new_rooms_by_number, new_rooms, max_room_number, connections)
    }

    /// Resets every exit in this area that leads to one of `numbers` in
    /// `target_area` to no destination, returning the rebuilt area with its
    /// revision advanced once, or `None` when nothing here led there. Mirrors
    /// the server's inbound-exit cascade on room deletion (see
    /// [`RoomCache::null_exits_into`]) for links between cloud areas, which
    /// the server clears in the deletion's own transaction. Links from any
    /// other area are cleared by an ordinary exit update queued for that
    /// area, which leaves the same document.
    pub(super) fn null_inbound_exits(
        &self,
        target_area: AreaId,
        numbers: &HashSet<RoomNumber>,
    ) -> Option<Self> {
        // Every area is asked on every deletion; most answer no without
        // copying anything.
        let leads_in = self.rooms.iter().any(|room| {
            room.get_exits()
                .iter()
                .any(|exit| RoomCache::exit_leads_into(exit, target_area, numbers))
        });
        if !leads_in {
            return None;
        }
        let mut new_rooms_by_number = self.rooms_by_number.clone();
        let mut touched = HashSet::new();
        // The topology of every affected exit before its destination was
        // cleared, for Connection repair.
        let mut cleared: Vec<ExitTopology> = Vec::new();
        for (room_number, room) in &self.rooms_by_number {
            if let Some(updated) = room.null_exits_into(target_area, numbers) {
                cleared.extend(
                    room.get_exits()
                        .iter()
                        .filter(|exit| RoomCache::exit_leads_into(exit, target_area, numbers))
                        .map(|exit| Self::exit_topology(&self.id, *room_number, exit)),
                );
                new_rooms_by_number.insert(*room_number, Arc::new(updated));
                touched.insert(*room_number);
            }
        }

        if touched.is_empty() {
            return None;
        }

        // Each exit that lost its destination drags its Connection along:
        // the far side is gone, so the row becomes dangling (idempotent when
        // a same-area room deletion already repaired it).
        let mut connections = self.connections.clone();
        let site = |number: crate::RoomAddress| {
            new_rooms_by_number
                .get(&number.number)
                .map(|room| RoomSite {
                    x: room.get_x(),
                    y: room.get_y(),
                    level: room.get_level(),
                })
        };
        let exits = Self::topologies_of(&self.id, &new_rooms_by_number, None);
        connection_lifecycle::repair_after_destinations_cleared(
            &cleared,
            &exits,
            &mut connections,
            site,
        );

        // Rooms keep their places, as they do in the stored document.
        let new_rooms = self
            .rooms
            .iter()
            .map(|room| {
                let number = room.get_room_number();
                if touched.contains(&number) {
                    new_rooms_by_number[&number].clone()
                } else {
                    room.clone()
                }
            })
            .collect();

        Some(self.rebuild_room_state(
            new_rooms_by_number,
            new_rooms,
            self.max_room_number,
            connections,
        ))
    }

    /// Projects one cached exit into its connection-relevant topology.
    fn exit_topology(area_id: &AreaId, from_room: RoomNumber, exit: &ExitCache) -> ExitTopology {
        let same_area = exit.to_area_id.as_ref() == Some(area_id);
        ExitTopology {
            id: exit.id,
            connection_id: exit.connection_id,
            from_room: from_room.into(),
            from_direction: exit.from_direction,
            to_room_in_area: if same_area {
                exit.to_room_number.map(crate::RoomAddress::map)
            } else {
                None
            },
            to_direction: exit.to_direction,
            leaves_area: exit.to_unknown || (!same_area && exit.to_area_id.is_some()),
        }
    }

    /// Every exit's topology, optionally excluding one (the exit being
    /// edited or deleted).
    fn topologies_of(
        area_id: &AreaId,
        rooms_by_number: &HashMap<RoomNumber, Arc<RoomCache>>,
        exclude: Option<ExitId>,
    ) -> Vec<ExitTopology> {
        rooms_by_number
            .iter()
            .flat_map(|(room_number, room)| {
                room.get_exits()
                    .iter()
                    .filter(|exit| Some(exit.id) != exclude)
                    .map(|exit| Self::exit_topology(area_id, *room_number, exit))
                    .collect::<Vec<_>>()
            })
            .collect()
    }

    #[must_use]
    pub fn get_max_room_number(&self) -> RoomNumber {
        self.max_room_number
    }

    #[must_use]
    pub fn get_room_connections(&self) -> &[RoomConnection] {
        if let Some(layer) = self.map_document_layer() {
            return layer.content().get_room_connections();
        }
        &self.room_connections
    }

    /// Stored Connection rows (as opposed to their resolved render views).
    #[must_use]
    pub fn get_connections(&self) -> &[Connection] {
        &self.connections
    }

    #[must_use]
    pub fn get_connection(&self, id: crate::ConnectionId) -> Option<&Connection> {
        self.connections
            .iter()
            .find(|connection| connection.id == id)
    }

    /// Rebuilds the full editable document used by the shared mutation
    /// applier. Cache-only metadata that the editor never mutates is retained;
    /// list-only family tokens and linked-area presentation rows are omitted.
    #[must_use]
    pub(super) fn to_details(&self) -> AreaWithDetails {
        let mut properties: Vec<_> = self
            .properties
            .iter()
            .map(|(name, entry)| crate::Property {
                name: name.clone(),
                value: entry.value.clone(),
            })
            .collect();
        properties.sort_by(|a, b| a.name.cmp(&b.name));
        AreaWithDetails {
            room_data: self.meta.room_data.to_vec(),
            sources: self.meta.sources.to_vec(),
            area: Area {
                projection_token: self.meta.projection_token.clone(),
                id: self.id,
                user_id: self.meta.owner_id,
                atlas_id: self.meta.atlas_id,
                atlas_name: self.meta.atlas_name.clone(),
                name: self.name.clone(),
                created_at: Utc::now(),
                rev: self.rev,
                access: self.meta.access,
                owner_nickname: self.meta.owner_nickname.clone(),
                copied_from_area_id: self.meta.copied_from_area_id,
                copied_from_rev: self.meta.copied_from_rev,
                copied_at: self.meta.copied_at,
                family_token: None,
                clan_id: self.meta.clan_id,
                clan_name: None,
                actions: self.meta.actions.clone(),
                clan_ownership: self.meta.clan_ownership,
            },
            format_version: AREA_FORMAT_VERSION,
            properties,
            rooms: self.rooms.iter().map(|room| room.to_details()).collect(),
            labels: self.labels.clone(),
            shapes: self.shapes.clone(),
            connections: self.connections.clone(),
            linked_areas: Vec::new(),
        }
    }

    pub fn with_rooms_in<F>(&self, min_x: f32, min_y: f32, max_x: f32, max_y: f32, mut fun: F)
    where
        F: FnMut(&Arc<RoomCache>),
    {
        let envelope = bounds_to_envelope(min_x, min_y, max_x, max_y);
        for entry in self.rooms_index.locate_in_envelope_intersecting(&envelope) {
            fun(&entry.room);
        }
    }

    pub fn with_room_connections_in<F>(
        &self,
        min_x: f32,
        min_y: f32,
        max_x: f32,
        max_y: f32,
        mut fun: F,
    ) where
        F: FnMut(&RoomConnection),
    {
        if let Some(layer) = self.map_document_layer() {
            layer
                .content()
                .with_room_connections_in(min_x, min_y, max_x, max_y, fun);
            return;
        }
        let envelope = bounds_to_envelope(min_x, min_y, max_x, max_y);
        for entry in self
            .room_connections_index
            .locate_in_envelope_intersecting(&envelope)
        {
            if let Some(connection) = self.room_connections.get(entry.index) {
                fun(connection);
            }
        }
    }
}

fn bounds_to_envelope(min_x: f32, min_y: f32, max_x: f32, max_y: f32) -> AABB<[f32; 2]> {
    let (min_x, max_x) = if min_x <= max_x {
        (min_x, max_x)
    } else {
        (max_x, min_x)
    };
    let (min_y, max_y) = if min_y <= max_y {
        (min_y, max_y)
    } else {
        (max_y, min_y)
    };

    AABB::from_corners([min_x, min_y], [max_x, max_y])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mutation::AreaMutation;
    use crate::{
        ConnectionDash, ConnectionEndpoint, ConnectionId, ConnectionRouting, CornerStyle,
        ExitDirection, PortMode, RoomSide, RoomUpdates, SegmentShape, SourceId,
    };
    use uuid::Uuid;

    /// A visible exit leaving via `from_direction` toward `(to_area,
    /// to_room)` (arriving from `to_direction`), as a member of `connection`.
    fn member_exit(
        id: u128,
        connection: ConnectionId,
        from_direction: ExitDirection,
        to_area: Option<AreaId>,
        to_room: Option<RoomNumber>,
        to_direction: Option<ExitDirection>,
    ) -> ExitCache {
        ExitCache {
            id: ExitId(Uuid::from_u128(id)),
            from_direction,
            to_area_id: to_area,
            to_room_number: to_room,
            to_direction,
            to_secret_map: None,
            to_private_map: None,
            path: None,
            is_hidden: false,
            door: None,
            weight: 1.0,
            command: None,
            connection_id: connection,
            to_unknown: false,
            to_area_token: None,
        }
    }

    fn endpoint(room: RoomNumber, side: RoomSide) -> ConnectionEndpoint {
        ConnectionEndpoint {
            source: None,
            room_number: room,
            side,
            port_offset: 0.5,
            port_mode: PortMode::AutoPinned,
        }
    }

    fn stored_connection(
        id: ConnectionId,
        kind: ConnectionKind,
        endpoint_a: ConnectionEndpoint,
        endpoint_b: Option<ConnectionEndpoint>,
    ) -> Connection {
        Connection {
            id,
            endpoint_a,
            endpoint_b,
            kind,
            routing: ConnectionRouting::Simple,
            segment_shape: SegmentShape::Direct,
            corner: CornerStyle::Sharp,
            route_points: Vec::new(),
            dash: ConnectionDash::Solid,
            color: crate::DEFAULT_CONNECTION_COLOR.to_string(),
            thickness: 1.0,
        }
    }

    fn placed_room(number: RoomNumber, x: f32, y: f32, level: i32) -> RoomCache {
        RoomCache::new(number).apply_updates(RoomUpdates {
            x: Some(x),
            y: Some(y),
            level: Some(level),
            ..RoomUpdates::default()
        })
    }

    fn rooms_by_number(list: Vec<RoomCache>) -> HashMap<RoomNumber, Arc<RoomCache>> {
        list.into_iter()
            .map(|room| (room.get_room_number(), Arc::new(room)))
            .collect()
    }

    #[test]
    fn self_loop_connection_resolves_a_self_loop_end() {
        let area = AreaId(Uuid::from_u128(1));
        let n = RoomNumber(1);
        let connection_id = ConnectionId::new();
        // North leaves and returns to the same room (arriving from the south).
        let room = placed_room(n, 0.0, 0.0, 0).upsert_exit(member_exit(
            10,
            connection_id,
            ExitDirection::North,
            Some(area),
            Some(n),
            Some(ExitDirection::South),
        ));
        let connections = vec![stored_connection(
            connection_id,
            ConnectionKind::SelfLoop,
            endpoint(n, RoomSide::North),
            Some(endpoint(n, RoomSide::South)),
        )];

        let conns =
            AreaCache::build_room_connections(&area, &connections, &rooms_by_number(vec![room]));

        assert_eq!(conns.len(), 1);
        assert!(matches!(conns[0].to, RoomConnectionEnd::SelfLoop));
        assert_eq!(conns[0].connection_id, connection_id);
        // Self-loops carry no arrow.
        assert!(conns[0].arrow_toward_b.is_none());
        assert!(!conns[0].geometry.circles.is_empty(), "loop arc resolved");
    }

    #[test]
    fn one_member_internal_connection_resolves_normal_with_an_arrow() {
        let area = AreaId(Uuid::from_u128(1));
        let (a, b) = (RoomNumber(1), RoomNumber(2));
        let connection_id = ConnectionId::new();
        let room_a = placed_room(a, 0.0, 0.0, 0).upsert_exit(member_exit(
            10,
            connection_id,
            ExitDirection::East,
            Some(area),
            Some(b),
            Some(ExitDirection::West),
        ));
        let room_b = placed_room(b, 4.0, 0.0, 0);
        let connections = vec![stored_connection(
            connection_id,
            ConnectionKind::Internal,
            endpoint(a, RoomSide::East),
            Some(endpoint(b, RoomSide::West)),
        )];

        let conns = AreaCache::build_room_connections(
            &area,
            &connections,
            &rooms_by_number(vec![room_a, room_b]),
        );

        assert_eq!(conns.len(), 1);
        let conn = &conns[0];
        assert!(matches!(
            conn.to,
            RoomConnectionEnd::Normal {
                direction: ExitDirection::West,
                ..
            }
        ));
        assert!(!conn.is_bidirectional);
        // The single member runs A→B: arrow at B.
        assert_eq!(conn.arrow_toward_b, Some(true));
        assert!(!conn.geometry.centerline.is_empty(), "stroke resolved");
        assert!(!conn.geometry.bounds.is_empty());
    }

    #[test]
    fn two_member_connection_is_bidirectional_without_an_arrow() {
        let area = AreaId(Uuid::from_u128(1));
        let (a, b) = (RoomNumber(1), RoomNumber(2));
        let connection_id = ConnectionId::new();
        let room_a = placed_room(a, 0.0, 0.0, 0).upsert_exit(member_exit(
            10,
            connection_id,
            ExitDirection::East,
            Some(area),
            Some(b),
            Some(ExitDirection::West),
        ));
        let room_b = placed_room(b, 4.0, 0.0, 0).upsert_exit(member_exit(
            11,
            connection_id,
            ExitDirection::West,
            Some(area),
            Some(a),
            Some(ExitDirection::East),
        ));
        let connections = vec![stored_connection(
            connection_id,
            ConnectionKind::Internal,
            endpoint(a, RoomSide::East),
            Some(endpoint(b, RoomSide::West)),
        )];

        let conns = AreaCache::build_room_connections(
            &area,
            &connections,
            &rooms_by_number(vec![room_a, room_b]),
        );

        assert_eq!(conns.len(), 1);
        assert!(conns[0].is_bidirectional);
        assert!(conns[0].arrow_toward_b.is_none());
    }

    #[test]
    fn door_state_folds_both_connection_members() {
        let area = AreaId(Uuid::from_u128(1));
        let (a, b) = (RoomNumber(1), RoomNumber(2));
        let connection_id = ConnectionId::new();
        let mut from_a = member_exit(
            10,
            connection_id,
            ExitDirection::East,
            Some(area),
            Some(b),
            Some(ExitDirection::West),
        );
        from_a.door = Some(crate::Door::new(crate::DoorState::Closed));
        let mut from_b = member_exit(
            11,
            connection_id,
            ExitDirection::West,
            Some(area),
            Some(a),
            Some(ExitDirection::East),
        );
        from_b.door = Some(crate::Door::new(crate::DoorState::Locked));
        let room_a = placed_room(a, 0.0, 0.0, 0).upsert_exit(from_a);
        let room_b = placed_room(b, 4.0, 0.0, 0).upsert_exit(from_b);
        let connections = vec![stored_connection(
            connection_id,
            ConnectionKind::Internal,
            endpoint(a, RoomSide::East),
            Some(endpoint(b, RoomSide::West)),
        )];

        let conns = AreaCache::build_room_connections(
            &area,
            &connections,
            &rooms_by_number(vec![room_a, room_b]),
        );
        assert_eq!(conns[0].door, Some(crate::DoorState::Locked));
    }

    #[test]
    fn cross_level_connection_emits_two_halves_sharing_geometry() {
        let area = AreaId(Uuid::from_u128(1));
        let (a, b) = (RoomNumber(1), RoomNumber(2));
        let connection_id = ConnectionId::new();
        let room_a = placed_room(a, 0.0, 0.0, 0).upsert_exit(member_exit(
            10,
            connection_id,
            ExitDirection::Up,
            Some(area),
            Some(b),
            Some(ExitDirection::Down),
        ));
        let room_b = placed_room(b, 1.0, 1.0, 1);
        let connections = vec![stored_connection(
            connection_id,
            ConnectionKind::CrossLevel,
            endpoint(a, RoomSide::East),
            Some(endpoint(b, RoomSide::West)),
        )];

        let conns = AreaCache::build_room_connections(
            &area,
            &connections,
            &rooms_by_number(vec![room_a, room_b]),
        );

        assert_eq!(conns.len(), 2);
        let by_level = |level: i32| {
            conns
                .iter()
                .find(|c| c.from_level == level)
                .expect("one half per level")
        };
        let (half_a, half_b) = (by_level(0), by_level(1));
        assert!(matches!(
            half_a.to,
            RoomConnectionEnd::ToLevel { level: 1, .. }
        ));
        assert!(matches!(
            half_b.to,
            RoomConnectionEnd::ToLevel { level: 0, .. }
        ));
        assert!(
            Arc::ptr_eq(&half_a.geometry, &half_b.geometry),
            "both halves share one resolved geometry"
        );
    }

    #[test]
    fn dangling_external_and_unknown_members_resolve_their_ends() {
        let area = AreaId(Uuid::from_u128(1));
        let other_area = AreaId(Uuid::from_u128(2));
        let n = RoomNumber(1);
        let dangling_id = ConnectionId::new();
        let external_id = ConnectionId::new();
        let unknown_id = ConnectionId::new();
        let unknown_exit = ExitCache {
            to_unknown: true,
            to_area_token: Some("tok".to_string()),
            ..member_exit(12, unknown_id, ExitDirection::South, None, None, None)
        };
        let room = placed_room(n, 0.0, 0.0, 0)
            .upsert_exit(member_exit(
                10,
                dangling_id,
                ExitDirection::North,
                None,
                None,
                None,
            ))
            .upsert_exit(member_exit(
                11,
                external_id,
                ExitDirection::East,
                Some(other_area),
                Some(RoomNumber(7)),
                None,
            ))
            .upsert_exit(unknown_exit);
        let connections = vec![
            stored_connection(
                dangling_id,
                ConnectionKind::Dangling,
                endpoint(n, RoomSide::North),
                None,
            ),
            stored_connection(
                external_id,
                ConnectionKind::External,
                endpoint(n, RoomSide::East),
                None,
            ),
            stored_connection(
                unknown_id,
                ConnectionKind::External,
                endpoint(n, RoomSide::South),
                None,
            ),
        ];

        let conns =
            AreaCache::build_room_connections(&area, &connections, &rooms_by_number(vec![room]));

        assert_eq!(conns.len(), 3);
        let end_of = |id: ConnectionId| &conns.iter().find(|c| c.connection_id == id).unwrap().to;
        assert!(matches!(end_of(dangling_id), RoomConnectionEnd::None));
        assert!(matches!(
            end_of(external_id),
            RoomConnectionEnd::External { area_id } if *area_id == other_area
        ));
        assert!(matches!(
            end_of(unknown_id),
            RoomConnectionEnd::Unknown { token } if token == "tok"
        ));
    }

    #[test]
    fn corrupt_connections_are_skipped_not_fatal() {
        let area = AreaId(Uuid::from_u128(1));
        let n = RoomNumber(1);
        let memberless = ConnectionId::new();
        let missing_room = ConnectionId::new();
        let orphan_member = ConnectionId::new();
        // An exit whose connection row is missing entirely: it simply does
        // not render.
        let room = placed_room(n, 0.0, 0.0, 0).upsert_exit(member_exit(
            10,
            orphan_member,
            ExitDirection::North,
            None,
            None,
            None,
        ));
        let connections = vec![
            stored_connection(
                memberless,
                ConnectionKind::Dangling,
                endpoint(n, RoomSide::North),
                None,
            ),
            stored_connection(
                missing_room,
                ConnectionKind::Dangling,
                endpoint(RoomNumber(99), RoomSide::North),
                None,
            ),
        ];

        let conns =
            AreaCache::build_room_connections(&area, &connections, &rooms_by_number(vec![room]));
        assert!(conns.is_empty());
    }

    #[test]
    fn sources_keep_attachments_separate_from_owned_rooms() {
        let room = |number: i32, x: f32| {
            serde_json::json!({
                "room_number": number, "title": "", "description": "", "color": "",
                "level": 0, "x": x, "y": 0.0, "properties": [], "exits": [], "tags": []
            })
        };
        let secret = |id: &str, name: &str| {
            serde_json::json!({
                "source": id, "name": name, "ownership": "owner", "rev": 1, "actions": ["read"],
                "rooms": [room(1, 9.0)],
                "room_data": [{ "room_number": 2, "properties": [{ "name": "notes", "value": "x" }] }]
            })
        };
        let details: AreaWithDetails = serde_json::from_value(serde_json::json!({
            "id": "123e4567-e89b-12d3-a456-426614174000",
            "user_id": null,
            "atlas_id": null,
            "name": "Midgaard",
            "created_at": "2026-10-04T00:00:00Z",
            "format_version": AREA_FORMAT_VERSION,
            "properties": [],
            "rooms": [room(1, 0.0), room(2, 4.0)],
            "labels": [],
            "shapes": [],
            "sources": [
                { "source": "private", "rev": 1, "actions": ["read"] },
                secret("4f1c2a6e-9d7b-4c3e-8a51-2b6d0e9f7a13", "sewer grate"),
                secret("0a1c2a6e-9d7b-4c3e-8a51-2b6d0e9f7a13", "Behind The Bookcase"),
            ],
        }))
        .expect("area parses");
        let cache = AreaCache::new_with_area(details);

        let layers = cache.source_layers();
        let names: Vec<_> = layers.iter().map(SourceLayer::name).collect();
        assert_eq!(
            names,
            [Some("Behind The Bookcase"), Some("sewer grate"), None]
        );
        assert_eq!(layers[2].source(), SourceId::Private);

        let content = layers[0].content();
        let own = content
            .get_room(&RoomNumber(1))
            .expect("the Secret's own room 1");
        assert!((own.get_x() - 9.0).abs() < f32::EPSILON);
        assert_eq!(content.get_rooms().len(), 1, "anchors are not owned rooms");
        let attachment = layers[0]
            .attachment(RoomAddress::map(RoomNumber(2)))
            .expect("map room attachment");
        assert!(
            (attachment.get_x() - 4.0).abs() < f32::EPSILON,
            "where the map has room 2"
        );
        assert!(
            cache
                .get_room(&RoomNumber(1))
                .is_some_and(|room| room.get_x().abs() < f32::EPSILON)
        );
    }

    const DOOR_MAP: &str = "123e4567-e89b-12d3-a456-426614174000";
    const DOOR_SECRET: &str = "6f1c2a9e-0b7d-4e1a-9c3f-2d8e5b4a7c10";

    fn door_secret() -> SourceId {
        DOOR_SECRET.parse().expect("a Secret id")
    }

    /// A served Secret whose own room 2 sits behind a hidden door on map
    /// room 1: an exit east from map room 1 into the Secret's room and one
    /// west back, paired into one connection, plus a note on map room 1.
    fn door_bundle() -> serde_json::Value {
        serde_json::json!({
            "source": DOOR_SECRET, "name": "Bookcase", "ownership": "owner", "rev": 4,
            "actions": ["read", "add", "edit", "remove"],
            "properties": [],
            "rooms": [{
                "room_number": 2, "title": "Vault", "description": "", "color": "",
                "level": 0, "x": 2.0, "y": 0.0, "properties": [], "tags": [],
                "exits": [{
                    "id": "00000000-0000-4000-8000-000000000002", "from_direction": "West",
                    "to_area_id": DOOR_MAP, "to_room_number": 1, "to_direction": "East",
                    "to_unknown": false, "path": "", "command": "", "weight": 1.0,
                    "connection_id": "00000000-0000-4000-8000-0000000000c1",
                    "is_hidden": false, "door": null
                }]
            }],
            "room_data": [{
                "room_number": 1,
                "properties": [{ "name": "notes", "value": "behind the shelf" }],
                "tags": ["DOOR"],
                "exits": [{
                    "id": "00000000-0000-4000-8000-000000000001", "from_direction": "East",
                    "to_area_id": DOOR_MAP, "to_room_number": 2, "to_source": DOOR_SECRET,
                    "to_direction": "West", "to_unknown": false, "path": "", "command": "",
                    "weight": 1.0, "connection_id": "00000000-0000-4000-8000-0000000000c1",
                    "is_hidden": true, "door": null
                }]
            }],
            "labels": [], "shapes": [],
            "connections": [{
                "id": "00000000-0000-4000-8000-0000000000c1",
                "endpoint_a": {
                    "room_number": 1, "side": "East", "port_offset": 0.5, "port_mode": "AutoPinned"
                },
                "endpoint_b": {
                    "room_number": 2, "source": DOOR_SECRET, "side": "West",
                    "port_offset": 0.5, "port_mode": "AutoPinned"
                },
                "kind": "Internal", "routing": "Simple", "segment_shape": "Direct",
                "corner": "Sharp", "route_points": [], "dash": "Solid",
                "color": "#A4A4A4", "thickness": 1.0
            }]
        })
    }

    /// Map rooms 1 and 2 beside `secret`.
    fn door_map(secret: &serde_json::Value) -> AreaWithDetails {
        let room = |number: i32, x: f32| {
            serde_json::json!({
                "room_number": number, "title": format!("room {number}"), "description": "",
                "color": "", "level": 0, "x": x, "y": 0.0, "properties": [], "exits": [],
                "tags": []
            })
        };
        let projection: crate::format3::AreaProjection =
            serde_json::from_value(serde_json::json!({
                "format_version": 3,
                "id": DOOR_MAP,
                "user_id": "123e4567-e89b-12d3-a456-426614174001",
                "atlas_id": null,
                "name": "Library",
                "created_at": "2026-10-04T00:00:00Z",
                "access": {
                    "is_owner": true, "can_edit": true, "can_reshare": true,
                    "can_copy": true, "can_admin": true, "include_secrets": true
                },
                "projection_token": "p_abc",
                "linked_areas": [],
                "sources": [
                    {
                        "source": "map", "rev": 3, "actions": ["read", "add", "edit", "remove"],
                        "properties": [], "rooms": [room(1, 0.0), room(2, 1.0)],
                        "labels": [], "shapes": [], "connections": []
                    },
                    secret,
                ],
            }))
            .expect("projection parses");
        AreaWithDetails::try_from(projection).expect("converts")
    }

    fn served_bundle(details: &AreaWithDetails) -> serde_json::Value {
        serde_json::to_value(
            details
                .sources
                .iter()
                .find(|bundle| bundle.source == door_secret())
                .expect("the Secret's bundle"),
        )
        .expect("serializes")
    }

    /// Operations that build `layer`'s content from nothing, in the
    /// qualified content addresses.
    #[test]
    fn map_attachment_geometry_uses_the_secret_room_not_its_ordinary_namesake() {
        let mut details = door_map(&door_bundle());
        let secret = &mut details.sources[0];
        details.connections = std::mem::take(&mut secret.connections);
        details.rooms[0].exits = std::mem::take(&mut secret.room_data[0].exits);
        details.room_data.push(crate::RoomData {
            room_source: Some(door_secret()),
            room_number: RoomNumber(2),
            properties: Vec::new(),
            tags: Default::default(),
            exits: std::mem::take(&mut secret.rooms[0].exits),
        });
        let before = serde_json::to_value(&details).unwrap();
        let cache = AreaCache::new_with_area(details.clone());
        let layer = cache.map_document_layer().unwrap();
        let attachment = layer
            .attachment(RoomAddress::new(door_secret(), RoomNumber(2)))
            .unwrap();
        assert_eq!(attachment.address().source, door_secret());
        assert_eq!(cache.room_count(), 2);
        let connection = &cache.get_room_connections()[0];
        assert!((connection.endpoint_b_room.as_ref().unwrap().get_x() - 2.0).abs() < f32::EPSILON);
        assert!((cache.get_room(&RoomNumber(2)).unwrap().get_x() - 1.0).abs() < f32::EPSILON);
        let (_, connection) = cache.find_connection(details.connections[0].id).unwrap();
        let wire = connection.endpoint_b.unwrap();
        assert_eq!(
            (wire.source, wire.room_number),
            (Some(door_secret()), RoomNumber(2))
        );
        let mut round_trip = cache.to_details();
        // The cache does not retain creation timestamps.
        round_trip.area.created_at = details.area.created_at;
        assert_eq!(serde_json::to_value(round_trip).unwrap(), before);

        details.sources.clear();
        let hidden = AreaCache::new_with_area(details);
        assert!(
            hidden.get_room_connections().is_empty(),
            "a missing Secret anchor never falls back to the ordinary room"
        );
    }

    fn rebuild_ops(layer: &SourceLayer) -> Vec<AreaMutation> {
        let content = layer.content();
        let mut rooms: Vec<_> = content.document_rooms().cloned().collect::<Vec<_>>();
        rooms.sort_by_key(|room| room.get_room_number());
        let mut ops = Vec::new();
        for room in &rooms {
            let number = room.get_room_number();
            if room.address().source == layer.source() {
                ops.push(AreaMutation::CreateRoom {
                    room_number: number,
                    room_source: room.address().wire_source(),
                    body: RoomUpdates {
                        title: Some(room.get_title().to_string()),
                        x: Some(room.get_x()),
                        y: Some(room.get_y()),
                        level: Some(room.get_level()),
                        ..RoomUpdates::default()
                    },
                });
            }
            for (name, value) in room.properties() {
                ops.push(AreaMutation::UpsertRoomProperty {
                    room_number: number,
                    room_source: room.address().wire_source(),
                    name: name.to_string(),
                    value: value.to_string(),
                });
            }
            for tag in room.tags() {
                ops.push(AreaMutation::AddRoomTag {
                    room_number: number,
                    room_source: room.address().wire_source(),
                    tag: tag.to_string(),
                });
            }
        }
        for connection in content.get_connections() {
            ops.push(AreaMutation::CreateConnection {
                body: crate::ConnectionArgs::from(connection),
            });
        }
        for room in &rooms {
            for exit in room.get_exits() {
                ops.push(AreaMutation::CreateExit {
                    room_number: room.get_room_number(),
                    room_source: room.address().wire_source(),
                    body: crate::ExitArgs {
                        id: Some(exit.id),
                        connection_id: Some(exit.connection_id),
                        from_direction: exit.from_direction,
                        to_area_id: exit.wire_to_area_id(),
                        to_source: exit.to_exit().to_source,
                        to_room_number: exit.to_room_number,
                        to_direction: exit.to_direction,
                        is_hidden: exit.is_hidden,
                        weight: exit.weight,
                        ..crate::ExitArgs::default()
                    },
                });
            }
        }
        ops
    }

    #[test]
    fn qualified_edits_round_trip_a_secret_with_a_hidden_door() {
        let served = door_map(&door_bundle());
        let cache = AreaCache::new_with_area(served.clone());
        let layer = &cache.source_layers()[0];
        let secret = door_secret();

        let wire = rebuild_ops(layer);

        let exit_from = |from: i32, room_source: Option<SourceId>| {
            wire.iter().find_map(|op| match op {
                AreaMutation::CreateExit {
                    room_number,
                    room_source: named,
                    body,
                } if *room_number == RoomNumber(from) && *named == room_source => Some(body),
                _ => None,
            })
        };
        let door = exit_from(1, None).expect("the door leaves map room 1");
        assert_eq!(
            (door.to_room_number, door.to_source),
            (Some(RoomNumber(2)), Some(secret)),
            "into the Secret's own room 2"
        );
        let back = exit_from(2, Some(secret)).expect("the way back leaves the Secret's room");
        assert_eq!(
            (back.to_room_number, back.to_source),
            (Some(RoomNumber(1)), None),
            "to map room 1"
        );
        assert!(wire.iter().any(|op| matches!(
            op,
            AreaMutation::UpsertRoomProperty {
                room_number: RoomNumber(1),
                room_source: None,
                ..
            }
        )));

        let mut rebuilt = door_map(&serde_json::json!({
            "source": DOOR_SECRET, "name": "Bookcase", "ownership": "owner", "rev": 4,
            "actions": ["read", "add", "edit", "remove"]
        }));
        crate::backends::source_document::apply_source_ops(&mut rebuilt, secret, &wire)
            .expect("the wire operations apply to the Secret");
        assert_eq!(served_bundle(&rebuilt), served_bundle(&served));
    }
    #[test]
    fn retained_attachments_disappear_and_reappear_with_their_qualified_anchor() {
        let mut details = door_map(&door_bundle());
        let mut private = crate::backends::source_document::empty_bundle(SourceId::Private);
        private.room_data.push(crate::RoomData {
            room_source: Some(door_secret()),
            room_number: RoomNumber(2),
            properties: vec![crate::Property {
                name: "note".into(),
                value: "retained".into(),
            }],
            tags: Default::default(),
            exits: Vec::new(),
        });
        details.sources.push(private);
        let address = RoomAddress::new(door_secret(), RoomNumber(2));
        let visible = AreaCache::for_viewer(details.clone(), None, None);
        assert!(
            visible
                .document_layer(SourceId::Private)
                .unwrap()
                .attachment(address)
                .is_some()
        );
        let secret = details.sources.remove(0);
        let hidden = AreaCache::for_viewer(details.clone(), None, Some(&visible));
        let private = hidden.document_layer(SourceId::Private).unwrap();
        assert!(private.attachment(address).is_none());
        assert!(private.content().get_rooms_with_property("note").is_empty());
        assert_eq!(
            hidden.meta().sources[0].room_data[0].properties[0].value,
            "retained"
        );
        details.sources.insert(0, secret);
        let visible = AreaCache::for_viewer(details, None, Some(&hidden));
        let attachment = visible
            .document_layer(SourceId::Private)
            .unwrap()
            .attachment(address)
            .unwrap();
        assert_eq!(attachment.get_property("note"), Some("retained"));
        assert_eq!(attachment.address(), address);
        assert_eq!(attachment.get_x(), 2.0);
    }
}
