//! The script surface for a map's **places** (`smudgy-cloudflare/docs/format-3.md`): the
//! map's own content (`"map"`), each of its Secrets the caller reads, and the caller's Private
//! additions (`"private"`). Each place keeps its own properties, tags and exits for the map's
//! rooms, and a Secret or Private additions keep rooms of their own besides.
//!
//! - **Where things live.** `area.places` lists a map's places (the map, its Secrets in layer
//!   order, Private additions last on a cloud map); `area.place` and `room.place` say where an
//!   area or room lives. An area id keeps one meaning: a Secret's own area reads under the
//!   Secret's id, and `area.mapId` names its map. Each place numbers its own rooms from 1, so
//!   `area.nextRoomNumber` and `createRoom` number within the area's place alone.
//! - **Views.** `room.in(place)` and `area.in(place)` read and write one place's data on a map
//!   room or map. Room views may name any readable source of that map; an area's
//!   own properties remain scoped to that area. `in()` takes
//!   `"map"`, `"private"`, a Secret or a Secret's id (an id cannot collide with a keyword). An
//!   unknown Secret and one the caller cannot read are the same "Secret not found".
//! - **Combined reads.** `room.combinedData(key?)` and `room.combinedTags()` list every
//!   readable place's values side by side, the room's own place first; nothing merges two
//!   places' values. `room.tags` is the union of every readable place's tags and
//!   `room.hasTag` asks any place; `room.data(key)` stays the room's own place.
//! - **Searches.** The tag and property searches and the nearest-room searches cover every
//!   readable place, one indexed lookup per place (`smudgy_cloud::mapper::places`), or one
//!   place with `{ in: place }`.
//! - **Secrets.** `area.secrets` lists, finds and creates a map's Secrets; a Secret's handle
//!   updates (one request for name and color) and deletes it. What the projection serves is
//!   all a handle shows: never owners, grants or audience.
//!
//! **Consent.** Installed packages reach places other than the map only with the manifest's
//! `smudgy.secrets` capability ([`SmudgyGrants`]' `secrets_read`, `secrets_write` and
//! `secrets_manage`; `write` and `manage` each imply `read`), on top of the mapper capability a
//! call already needs. It covers Secrets and Private additions alike, and `manage` covers
//! creating, updating and deleting Secrets. Trusted user scripts and trusted packages hold
//! everything.
//!
//! Without `secrets` read, Secrets and Private additions are invisible: nothing an isolate sees
//! differs from a map without them. Places, views, combined reads and searches include only the
//! map; `room.exits` leaves out their hidden doors ([`room_exits`]), and a write naming one by id
//! or a connection anchored on a Secret/Private room answers as an unknown id does.
//! Map-owned exits retain their content while unreadable destinations are redacted
//! ([`ensure_exit_write`], [`ensure_exit_shown`], [`connection_as_seen`]); routes and the
//! nearest-room searches walk as on a map without them ([`Sources::Hidden`]); their own areas answer every
//! lookup and write as an unknown id does ([`hides_area`], [`reach`]); a location in one of
//! their rooms reads as somewhere unmapped on their map ([`visible_location`]); and a click on
//! one of their rooms in a session map is never heard of ([`map_click_payload_for`]). Naming a Secret
//! or `"private"` throws the same capability error for every name before anything is looked up
//! ([`readable_place`]). Each place numbers its own rooms, so room numbers show nothing of the
//! others either.

use std::{borrow::Cow, cell::RefCell, rc::Rc, sync::Arc};

use deno_core::{OpState, op2};
use serde::Serialize;

use super::mapper_api::{
    JSArea, JSExit, JSRoom, JsRoomRef, MapperError, ensure_mapper, note_navigation, parse_id,
};
use super::ops::SmudgyGrants;
use super::script_uuid::ScriptUuid;
use smudgy_cloud::{
    AreaId, MapStorage, Mapper, RoomNumber, SourceBundle, SourceId, Uuid,
    clan_secrets::{NewSecret, NewSecretOwner},
    cloud_api::{SecretChange, SecretColorChange, SecretSummary},
    mapper::{
        RoomKey,
        area_cache::{AreaCache, SourceLayer},
        atlas_cache::AtlasCache,
        places::{Places, RoomQuery, Sources},
        room_cache::RoomCache,
    },
};

/// A wire word (`manage_access`) as scripts spell it (`manageAccess`).
fn camel_case(word: &str) -> String {
    let mut out = String::with_capacity(word.len());
    let mut upper = false;
    for c in word.chars() {
        if c == '_' {
            upper = true;
        } else if upper {
            out.push(c.to_ascii_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// A Secret as scripts see it: what the caller's projection serves about it, and nothing about
/// its owners or audience.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct JsSecret {
    id: ScriptUuid,
    /// The map the Secret belongs to.
    map_id: ScriptUuid,
    name: String,
    color: Option<String>,
    ownership: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    clan_id: Option<ScriptUuid>,
    actions: Vec<String>,
}

impl JsSecret {
    fn from_bundle(map: AreaId, id: Uuid, bundle: &SourceBundle) -> Self {
        Self {
            id: ScriptUuid(id),
            map_id: ScriptUuid(map.0),
            name: bundle.name.clone().unwrap_or_default(),
            color: bundle.color.clone(),
            ownership: bundle
                .ownership
                .clone()
                .unwrap_or_else(|| "owner".to_string()),
            clan_id: bundle.clan_id.map(ScriptUuid),
            actions: bundle
                .actions
                .iter()
                .map(|action| camel_case(action))
                .collect(),
        }
    }

    fn from_summary(map: AreaId, id: Uuid, summary: &SecretSummary) -> Self {
        Self {
            id: ScriptUuid(id),
            map_id: ScriptUuid(map.0),
            name: summary.name.clone(),
            color: summary.color.clone(),
            ownership: summary.ownership.clone(),
            clan_id: None,
            actions: summary
                .actions
                .iter()
                .map(|action| camel_case(action))
                .collect(),
        }
    }
}

/// A place as scripts receive it: `"map"`, `"private"`, or a Secret.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub(super) enum JsPlace {
    Keyword(&'static str),
    Secret(JsSecret),
}

/// One place's value for a property of a room (`room.combinedData`).
#[derive(Debug, Serialize)]
pub(super) struct JsPlaceData {
    /// `"map"`, `"private"`, or the Secret's id.
    place: String,
    key: String,
    data: String,
}

/// Parses a script's place: `"map"`, `"private"`, or a Secret's id.
fn parse_place(value: &str) -> Result<SourceId, MapperError> {
    value
        .parse::<SourceId>()
        .map_err(|_| MapperError::InvalidPlace(value.to_owned()))
}

/// Parses a place a script names to read it. Anything but `"map"` needs `smudgy.secrets`
/// read, checked before the name is parsed or looked up, so an isolate without it gets the
/// same refusal for every name and learns nothing about which exist.
fn readable_place(state: &OpState, key: &str) -> Result<SourceId, MapperError> {
    if key != SourceId::MAP && !secrets_readable(state) {
        return Err(MapperError::NotCapable("secrets-read"));
    }
    parse_place(key)
}

/// Parses a place a script names to write it, as [`readable_place`] for `smudgy.secrets`
/// write.
fn writable_place(state: &OpState, key: &str) -> Result<SourceId, MapperError> {
    if key != SourceId::MAP && !state.borrow::<SmudgyGrants>().secrets_write {
        return Err(MapperError::NotCapable("secrets-write"));
    }
    parse_place(key)
}

fn secrets_readable(state: &OpState) -> bool {
    state.borrow::<SmudgyGrants>().secrets_read
}

/// Refuses a read of a place other than the map to an isolate without `smudgy.secrets` read.
pub(super) fn ensure_place_read(state: &OpState, place: SourceId) -> Result<(), MapperError> {
    if place.is_map() || secrets_readable(state) {
        Ok(())
    } else {
        Err(MapperError::NotCapable("secrets-read"))
    }
}

/// Refuses a write to a place other than the map to an isolate without `smudgy.secrets` write.
fn ensure_place_write(state: &OpState, place: SourceId) -> Result<(), MapperError> {
    if place.is_map() || state.borrow::<SmudgyGrants>().secrets_write {
        Ok(())
    } else {
        Err(MapperError::NotCapable("secrets-write"))
    }
}

fn ensure_secrets_manage(state: &OpState) -> Result<(), MapperError> {
    if state.borrow::<SmudgyGrants>().secrets_manage {
        Ok(())
    } else {
        Err(MapperError::NotCapable("secrets-manage"))
    }
}

fn current_atlas(state: &OpState) -> Option<Arc<AtlasCache>> {
    state
        .try_borrow::<Mapper>()
        .map(smudgy_cloud::Mapper::get_current_atlas)
}

/// Whether this isolate sees a map's Secrets and Private additions at all. One without
/// `smudgy.secrets` read sees maps as they would be without them.
pub(super) fn sources_for(state: &OpState) -> Sources {
    if secrets_readable(state) {
        Sources::Shown
    } else {
        Sources::Hidden
    }
}

/// Whether `area_id` is a Secret's or Private additions' own area that this isolate cannot
/// see, now or since it left the atlas ([`AtlasCache::place_map`]). Such an area answers as an
/// unknown one does.
pub(super) fn hides_area(state: &OpState, area_id: AreaId) -> bool {
    !secrets_readable(state)
        && current_atlas(state).is_some_and(|atlas| atlas.place_map(&area_id).is_some())
}

/// `area_id`, when this isolate may reach it; otherwise the refusal the mapper gives an area
/// it does not hold, so a hidden area and an unknown one fail alike.
pub(super) fn reach(state: &OpState, area_id: AreaId) -> Result<AreaId, smudgy_cloud::CloudError> {
    if hides_area(state, area_id) {
        Err(smudgy_cloud::CloudError::AreaNotFound(area_id))
    } else {
        Ok(area_id)
    }
}

/// Refuses a write through `area_id` to an isolate that sees the area but lacks
/// `smudgy.secrets` write when the area is a Secret's or Private additions' own. An area the
/// isolate cannot see passes here, so [`reach`] refuses it as it refuses an unknown one.
pub(super) fn ensure_area_write(state: &OpState, area_id: AreaId) -> Result<(), MapperError> {
    if state.borrow::<SmudgyGrants>().secrets_write || hides_area(state, area_id) {
        return Ok(());
    }
    if current_atlas(state).is_some_and(|atlas| atlas.place_map(&area_id).is_some()) {
        Err(MapperError::NotCapable("secrets-write"))
    } else {
        Ok(())
    }
}

/// `area_id`, when this isolate may write through it: a hidden area fails through `failed` as
/// an unknown one does ([`reach`]), and a seen Secret's or Private additions' own area without
/// `smudgy.secrets` write fails as [`ensure_area_write`] does.
pub(super) fn reach_for_write(
    state: &OpState,
    area_id: AreaId,
    failed: impl FnOnce(smudgy_cloud::CloudError) -> MapperError,
) -> Result<AreaId, MapperError> {
    let area_id = reach(state, area_id).map_err(failed)?;
    ensure_area_write(state, area_id)?;
    Ok(area_id)
}

/// Naming an exit destination requires Read there. It does not write to that
/// destination's source; the exit owner's Write consent is checked separately.
/// Hidden destinations answer as unknown ones through `failed`.
pub(super) fn ensure_exit_target(
    state: &OpState,
    target: AreaId,
    failed: impl FnOnce(smudgy_cloud::CloudError) -> MapperError,
) -> Result<(), MapperError> {
    if hides_area(state, target) {
        return Err(failed(smudgy_cloud::CloudError::NotFoundOrNoAccess));
    }
    Ok(())
}

/// Refuses a write by id of exit `exit_id` through `area_id` that would reach a place this
/// isolate may not write. The mapper takes a write naming an exit that a Secret or the Private
/// additions keep on one of the map's rooms to that place, so the write is that place's:
///
/// - Without `smudgy.secrets` read, an exit owned by a Secret or Private source
///   answers as an exit that does not exist does: `failed` with
///   [`smudgy_cloud::CloudError::ExitNotFound`].
/// - With read but not write, a place's exit is refused for `secrets-write`.
///
/// `room`, when the write names one, is where the exit is looked for, as the mapper looks: a
/// room the area does not have passes here, so the mapper answers it as it answers any.
pub(super) fn ensure_exit_write(
    state: &OpState,
    area_id: AreaId,
    room: Option<RoomNumber>,
    exit_id: smudgy_cloud::ExitId,
    failed: impl FnOnce(smudgy_cloud::CloudError) -> MapperError,
) -> Result<(), MapperError> {
    let Some(atlas) = current_atlas(state) else {
        return Ok(());
    };
    let Some(area) = atlas.get_area(&area_id) else {
        return Ok(());
    };
    if room.is_some_and(|room| area.get_room(&room).is_none()) {
        return Ok(());
    }
    let anchored_at = |layer: &SourceLayer| match room {
        Some(room) => layer
            .anchored_exits(room)
            .iter()
            .any(|exit| exit.id == exit_id),
        None => layer
            .all_anchored_exits()
            .any(|(_, exits)| exits.iter().any(|exit| exit.id == exit_id)),
    };
    if let Some(layer) = area.source_layers().iter().find(|layer| anchored_at(layer)) {
        if !secrets_readable(state) {
            return Err(failed(smudgy_cloud::CloudError::ExitNotFound(exit_id)));
        }
        return ensure_place_write(state, layer.source());
    }
    Ok(())
}

/// Refuses to an isolate without `smudgy.secrets` read an unlink naming exit `exit_id` of
/// `area_id` that it is not shown: a door a Secret or the Private additions keep on a map room,
/// excluding exits owned by the ordinary map. To it the exit does not exist, so `failed`
/// gets the answer for an exit that does not exist.
pub(super) fn ensure_exit_shown(
    state: &OpState,
    area_id: AreaId,
    exit_id: smudgy_cloud::ExitId,
    failed: impl FnOnce(smudgy_cloud::CloudError) -> MapperError,
) -> Result<(), MapperError> {
    if secrets_readable(state) {
        return Ok(());
    }
    ensure_exit_write(state, area_id, None, exit_id, failed)
}

/// Protect a connection stored in an unreadable source. A map-owned connection
/// remains editable when its remote destination is unreadable.
pub(super) fn connection_as_seen(
    state: &OpState,
    area_id: AreaId,
    connection_id: smudgy_cloud::ConnectionId,
) -> smudgy_cloud::ConnectionId {
    if secrets_readable(state) {
        return connection_id;
    }
    let Some(atlas) = current_atlas(state) else {
        return connection_id;
    };
    let Some(area) = atlas.get_area(&area_id) else {
        return connection_id;
    };
    if area.get_connections().iter().any(|connection| {
        connection.id == connection_id && !connection_anchors_visible(state, connection)
    }) || area.source_layers().iter().any(|layer| {
        layer
            .content()
            .get_connections()
            .iter()
            .any(|connection| connection.id == connection_id)
    }) {
        smudgy_cloud::ConnectionId::new()
    } else {
        connection_id
    }
}

/// A local room anchor needs the same consent as the room. A remote exit
/// destination does not own the connection and is redacted independently.
pub(super) fn connection_anchors_visible(
    state: &OpState,
    connection: &smudgy_cloud::Connection,
) -> bool {
    secrets_readable(state)
        || [Some(connection.endpoint_a), connection.endpoint_b]
            .into_iter()
            .flatten()
            .all(|endpoint| endpoint.source.is_none_or(|source| source.is_map()))
}

/// Naming a Secret or Private room as an anchor reads that room; it does not
/// grant authority to edit the room or change the connection's owning source.
pub(super) fn ensure_connection_anchors_read(
    state: &OpState,
    endpoints: impl IntoIterator<Item = Option<smudgy_cloud::ConnectionEndpoint>>,
) -> Result<(), MapperError> {
    for endpoint in endpoints.into_iter().flatten() {
        if let Some(source) = endpoint.source {
            ensure_place_read(state, source)?;
        }
    }
    Ok(())
}

/// The hidden location this isolate set itself (with `setCurrentLocation`). Until anything else
/// writes the session's location, the isolate reads back what it set, as it would for any id it
/// made up. The next such write ends that, a write of this same location included, whether or
/// not the isolate hears of it (it need not listen to `map:room`). Any other location, the same
/// place's other rooms and a later return to this one included, reads as every hidden location
/// does.
#[derive(Default)]
pub(super) struct NamedHiddenLocation(std::cell::Cell<Option<NamedLocation>>);

/// A location an isolate set, and the number of the session's location write that set it.
#[derive(Clone, Copy)]
struct NamedLocation {
    area_id: AreaId,
    room: Option<i32>,
    write: u64,
}

/// Notes the location this isolate has just set as the session's latest location write: kept
/// when it names a hidden area, forgotten otherwise.
pub(super) fn note_named_area(state: &mut OpState, area_id: AreaId, room: Option<i32>) {
    let named = if hides_area(state, area_id) {
        current_location_write(state).map(|write| NamedLocation {
            area_id,
            room,
            write,
        })
    } else {
        None
    };
    if state.try_borrow::<NamedHiddenLocation>().is_none() {
        state.put(NamedHiddenLocation::default());
    }
    state.borrow::<NamedHiddenLocation>().0.set(named);
}

/// The number of the session's latest location write.
fn current_location_write(state: &OpState) -> Option<u64> {
    state
        .try_borrow::<crate::session::runtime::CurrentLocation>()
        .map(|current| current.write())
}

/// A location as this isolate sees it. Standing in a Secret's or Private additions' own room
/// reads, to an isolate that cannot see them, as standing somewhere unmapped on their map: the
/// map, with no room. That holds after the place leaves the atlas too (a revoked, deleted or
/// purged Secret), so nothing that stays behind shows its id. The location the isolate set
/// itself reads back as it set it until anything else writes the session's location or the
/// session is seen anywhere else ([`NamedHiddenLocation`]).
pub(super) fn visible_location(
    state: &OpState,
    area_id: AreaId,
    room: Option<i32>,
) -> (AreaId, Option<i32>) {
    if secrets_readable(state) {
        return (area_id, room);
    }
    if let Some(named) = state.try_borrow::<NamedHiddenLocation>()
        && let Some(location) = named.0.get()
    {
        if (location.area_id, location.room) == (area_id, room)
            && current_location_write(state) == Some(location.write)
        {
            return (area_id, room);
        }
        named.0.set(None);
    }
    match current_atlas(state).and_then(|atlas| atlas.place_map(&area_id)) {
        Some(map) => (map, None),
        None => (area_id, room),
    }
}

/// A `map:room` payload as this isolate receives it (see [`visible_location`]).
pub(super) fn map_room_payload_for<'p>(state: &OpState, payload: &'p str) -> Cow<'p, str> {
    if secrets_readable(state) {
        return Cow::Borrowed(payload);
    }
    let Ok(value) = serde_json::from_str::<serde_json::Value>(payload) else {
        return Cow::Borrowed(payload);
    };
    let Some(area_id) = value
        .get("areaId")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| Uuid::try_parse(id).ok())
        .map(AreaId)
    else {
        return Cow::Borrowed(payload);
    };
    let room = value
        .get("roomNumber")
        .and_then(serde_json::Value::as_i64)
        .and_then(|room| i32::try_from(room).ok());
    let (seen_area, seen_room) = visible_location(state, area_id, room);
    if (seen_area, seen_room) == (area_id, room) {
        return Cow::Borrowed(payload);
    }
    Cow::Owned(
        serde_json::json!({ "areaId": seen_area.to_string(), "roomNumber": seen_room }).to_string(),
    )
}

/// A `map:click` payload as this isolate receives it, or `None` when it receives nothing.
/// Without `smudgy.secrets` read, a click reaches the isolate only on a room of a map its own
/// atlas holds: a click on a Secret's or Private additions' own room ([`hides_area`]) is not
/// delivered at all, since to such an isolate the map has no room there and nothing is
/// clicked where the map has no room. Unlike `map:room`, no hidden form stands in for it (a
/// click on "somewhere unmapped" would say something is there), an area the isolate named
/// itself is no exception, and neither is a click in another session whose places this
/// isolate's atlas does not know. A payload that does not parse is not delivered.
pub(super) fn map_click_payload_for<'p>(state: &OpState, payload: &'p str) -> Option<Cow<'p, str>> {
    if secrets_readable(state) {
        return Some(Cow::Borrowed(payload));
    }
    let area_id = serde_json::from_str::<serde_json::Value>(payload)
        .ok()?
        .get("areaId")
        .and_then(serde_json::Value::as_str)
        .and_then(|id| Uuid::try_parse(id).ok())
        .map(AreaId)?;
    let a_map = current_atlas(state).is_some_and(|atlas| {
        atlas.get_area(&area_id).is_some() && atlas.place_map(&area_id).is_none()
    });
    (a_map && !hides_area(state, area_id)).then_some(Cow::Borrowed(payload))
}

/// A `map:merged` payload as this isolate receives it, or `None` when it receives nothing.
/// Without `smudgy.secrets` read, a move into a Secret or Private additions reads as the moved
/// rooms leaving the map and a move out of one as rooms appearing on it, as on a map without
/// them: every remap into or out of a place it cannot see ([`hides_area`]) goes, and a payload
/// left with none is not delivered.
pub(super) fn map_merged_payload_for<'p>(
    state: &OpState,
    payload: &'p str,
) -> Option<Cow<'p, str>> {
    if secrets_readable(state) {
        return Some(Cow::Borrowed(payload));
    }
    let mut value = serde_json::from_str::<serde_json::Value>(payload).ok()?;
    let hidden = |area: Option<&serde_json::Value>| {
        area.and_then(serde_json::Value::as_str)
            .and_then(|id| Uuid::try_parse(id).ok())
            .is_none_or(|id| hides_area(state, AreaId(id)))
    };
    if hidden(value.get("into")) {
        return None;
    }
    let rooms = value.get_mut("rooms")?.as_array_mut()?;
    let before = rooms.len();
    rooms.retain(|moved| !hidden(moved.get("from").and_then(|from| from.get("area"))));
    if rooms.is_empty() {
        None
    } else if rooms.len() == before {
        Some(Cow::Borrowed(payload))
    } else {
        Some(Cow::Owned(value.to_string()))
    }
}

/// Where an area's own content lives: its map, and its own place (the map for a map; the
/// Secret or Private additions for one of their own areas).
fn place_of(atlas: &AtlasCache, area_id: AreaId) -> (AreaId, SourceId) {
    atlas
        .source_of(&area_id)
        .unwrap_or((area_id, SourceId::Map))
}

/// The key a script receives for `place` and names it back by: `"map"`, `"private"`, or a
/// Secret's id.
fn place_key(place: SourceId) -> String {
    place.to_string()
}

/// The Secret `id` as scripts see it, when the caller reads it.
fn secret_on_map(atlas: &AtlasCache, id: Uuid) -> Option<JsSecret> {
    let (map, source) = atlas.source_of(&AreaId(id))?;
    if source != SourceId::Secret(id) {
        return None;
    }
    let area = atlas.get_area(&map)?;
    let bundle = area
        .meta()
        .sources
        .iter()
        .find(|bundle| bundle.source == source)?;
    Some(JsSecret::from_bundle(map, id, bundle))
}

/// `place` as scripts receive it.
fn js_place(atlas: Option<&AtlasCache>, place: SourceId) -> Result<JsPlace, MapperError> {
    match place {
        SourceId::Map => Ok(JsPlace::Keyword(SourceId::MAP)),
        SourceId::Private => Ok(JsPlace::Keyword(SourceId::PRIVATE)),
        SourceId::Secret(id) => atlas
            .and_then(|atlas| secret_on_map(atlas, id))
            .map(JsPlace::Secret)
            .ok_or(MapperError::SecretNotFound),
    }
}

/// Whether a map's places include Private additions: only cloud maps keep them.
fn keeps_private(state: &OpState, map: AreaId) -> bool {
    state
        .try_borrow::<Mapper>()
        .is_some_and(|mapper| mapper.area_storage(&map) == MapStorage::Cloud)
}

/// Checks `place` against the area `area_id`: one of its map's places on a map (the map,
/// Private additions on a cloud map, or one of its Secrets the caller reads), its own place on
/// a Secret's or Private additions' own area. Returns the map and the area's own place.
fn resolve_place(
    state: &OpState,
    atlas: &AtlasCache,
    area_id: AreaId,
    place: SourceId,
) -> Result<(AreaId, SourceId), MapperError> {
    // An area this isolate cannot see answers as an unknown one: a place of its own.
    if hides_area(state, area_id) {
        return Ok((area_id, SourceId::Map));
    }
    let (map, own) = place_of(atlas, area_id);
    if !own.is_map() {
        return if place == own {
            Ok((map, own))
        } else {
            Err(MapperError::PlaceMismatch)
        };
    }
    match place {
        SourceId::Map => Ok((map, own)),
        SourceId::Private if keeps_private(state, map) => Ok((map, own)),
        SourceId::Private => Err(MapperError::PrivateNeedsCloud),
        SourceId::Secret(_) => atlas
            .get_area(&map)
            .is_some_and(|area| {
                area.source_layers()
                    .iter()
                    .any(|layer| layer.source() == place)
            })
            .then_some((map, own))
            .ok_or(MapperError::SecretNotFound),
    }
}

/// Parses and checks a script's place for the area `area_id`, for a read.
fn read_place(
    state: &OpState,
    area_id: AreaId,
    key: &str,
) -> Result<(Arc<AtlasCache>, AreaId, SourceId, SourceId), MapperError> {
    let place = readable_place(state, key)?;
    let atlas = current_atlas(state).ok_or(MapperError::MapperNotEnabled)?;
    let (map, own) = resolve_place(state, &atlas, area_id, place)?;
    Ok((atlas, map, own, place))
}

/// A room may retain data owned by any readable source of its map.
fn read_room_place(
    state: &OpState,
    area_id: AreaId,
    key: &str,
) -> Result<(Arc<AtlasCache>, AreaId, SourceId, SourceId), MapperError> {
    let place = readable_place(state, key)?;
    let atlas = current_atlas(state).ok_or(MapperError::MapperNotEnabled)?;
    let (map, own) = place_of(&atlas, area_id);
    ensure_place_read(state, own)?;
    resolve_place(state, &atlas, map, place)?;
    Ok((atlas, map, own, place))
}

/// Resolves the place a write through `area_id` lands in, as the batch names it. An empty
/// `named` is the area's own place, as every write has always been; so is naming that place.
/// Any other place of a map is that place of the map; a Secret's or Private additions' own
/// area holds only its own.
pub(super) fn write_place(
    state: &OpState,
    area_id: AreaId,
    named: &str,
) -> Result<SourceId, MapperError> {
    if named.is_empty() {
        return Ok(SourceId::Map);
    }
    let place = writable_place(state, named)?;
    let atlas = current_atlas(state).ok_or(MapperError::MapperNotEnabled)?;
    let (_, own) = resolve_place(state, &atlas, area_id, place)?;
    if !own.is_map() {
        ensure_place_write(state, own)?;
        return Ok(SourceId::Map);
    }
    Ok(place)
}

/// Other readable sources' data for this qualified room, in place order.
fn place_rooms(
    state: &OpState,
    area_id: AreaId,
    number: RoomNumber,
) -> Vec<(SourceId, Arc<RoomCache>)> {
    if !secrets_readable(state) {
        return Vec::new();
    }
    let Some(atlas) = current_atlas(state) else {
        return Vec::new();
    };
    let (map, own) = place_of(&atlas, area_id);
    let Some(area) = atlas.get_area(&map) else {
        return Vec::new();
    };
    area.map_document_layer()
        .into_iter()
        .chain(area.source_layers())
        .filter(|layer| layer.source() != own)
        .filter_map(|layer| {
            layer
                .attachment(smudgy_cloud::RoomAddress::new(own, number))
                .map(|room| (layer.source(), room.clone()))
        })
        .collect()
}

/// The layer of `place` on the map `map`.
fn layer_of(area: &AreaCache, place: SourceId) -> Option<&SourceLayer> {
    area.source_layers()
        .iter()
        .find(|layer| layer.source() == place)
}

/// `room.place` (by key): `"map"` for a map's room, the Secret's id or `"private"` for one of
/// their own rooms. Scripts resolve a key through [`op_smudgy_mapper_resolve_place`].
#[op2]
#[string]
pub(super) fn op_smudgy_mapper_room_place(state: &OpState, #[cppgc] room: &JSRoom) -> String {
    current_atlas(state).map_or_else(
        || place_key(SourceId::Map),
        |atlas| place_key(place_of(&atlas, room.1).1),
    )
}

/// `area.place` (by key), as [`op_smudgy_mapper_room_place`].
#[op2]
#[string]
pub(super) fn op_smudgy_mapper_area_place(state: &OpState, #[cppgc] area: &JSArea) -> String {
    current_atlas(state).map_or_else(
        || place_key(SourceId::Map),
        |atlas| place_key(place_of(&atlas, *area.0.get_id()).1),
    )
}

/// A place key as scripts receive the place: the keyword, or the Secret. Anything but the map
/// needs `smudgy.secrets` read.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_resolve_place(
    state: &OpState,
    #[string] key: &str,
) -> Result<JsPlace, MapperError> {
    let place = readable_place(state, key)?;
    js_place(current_atlas(state).as_deref(), place)
}

/// `area.places`: the map first, then its Secrets in layer order, then Private additions on a
/// cloud map. A Secret's or Private additions' own area has its own place alone. An isolate
/// without `smudgy.secrets` read sees the map's place only.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_list_area_places(
    state: &OpState,
    #[cppgc] area: &JSArea,
) -> Result<Vec<JsPlace>, MapperError> {
    let area_id = *area.0.get_id();
    let Some(atlas) = current_atlas(state) else {
        return Ok(vec![JsPlace::Keyword(SourceId::MAP)]);
    };
    let (map, own) = place_of(&atlas, area_id);
    let readable = secrets_readable(state);
    if !own.is_map() {
        return if readable {
            Ok(vec![js_place(Some(&atlas), own)?])
        } else {
            Ok(Vec::new())
        };
    }
    let mut places = vec![JsPlace::Keyword(SourceId::MAP)];
    if !readable {
        return Ok(places);
    }
    if let Some(map_area) = atlas.get_area(&map) {
        for layer in map_area.source_layers() {
            if let SourceId::Secret(id) = layer.source()
                && let Some(secret) = secret_on_map(&atlas, id)
            {
                places.push(JsPlace::Secret(secret));
            }
        }
    }
    if keeps_private(state, map) {
        places.push(JsPlace::Keyword(SourceId::PRIVATE));
    }
    Ok(places)
}

/// Checks a place for `room.in(place)` and `area.in(place)` on the area `area_id`, so a bad
/// place fails where the view is made.
#[op2(fast)]
pub(super) fn op_smudgy_mapper_check_place(
    state: &OpState,
    #[string] area_id: &str,
    #[string] key: &str,
    room: bool,
) -> Result<(), MapperError> {
    let area = AreaId(parse_id(area_id)?);
    if room {
        read_room_place(state, area, key).map(|_| ())
    } else {
        read_place(state, area, key).map(|_| ())
    }
}

/// `room.in(place).data(key)`: the value `place` keeps for this room, or `undefined`.
#[op2]
#[string]
pub(super) fn op_smudgy_mapper_room_place_data(
    state: &OpState,
    #[cppgc] room: &JSRoom,
    #[string] key: &str,
    #[string] name: &str,
) -> Result<Option<String>, MapperError> {
    let (atlas, map, own, place) = read_room_place(state, room.1, key)?;
    if place == own {
        return Ok(room.0.get_property(name).map(str::to_owned));
    }
    let number = room.0.get_room_number();
    Ok(atlas.get_area(&map).and_then(|area| {
        let layer = area.document_layer(place)?;
        layer
            .attachment(smudgy_cloud::RoomAddress::new(own, number))?
            .get_property(name)
            .map(str::to_owned)
    }))
}

/// The tags `place` keeps for `room`, UPPERCASE and sorted.
fn tags_in(
    atlas: &AtlasCache,
    room: &JSRoom,
    map: AreaId,
    own: SourceId,
    place: SourceId,
) -> Vec<String> {
    if place == own {
        return room.0.tags().map(String::from).collect();
    }
    let number = room.0.get_room_number();
    let mut tags: Vec<String> = atlas
        .get_area(&map)
        .and_then(|area| {
            let layer = area.document_layer(place)?;
            layer
                .attachment(smudgy_cloud::RoomAddress::new(own, number))
                .map(|data| data.tags().map(str::to_uppercase).collect())
        })
        .unwrap_or_default();
    tags.sort();
    tags.dedup();
    tags
}

/// `room.in(place).tags`.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_room_place_tags(
    state: &OpState,
    #[cppgc] room: &JSRoom,
    #[string] key: &str,
) -> Result<Vec<String>, MapperError> {
    let (atlas, map, own, place) = read_room_place(state, room.1, key)?;
    Ok(tags_in(&atlas, room, map, own, place))
}

/// `room.in(place).hasTag(tag)`, case-insensitive.
#[op2(fast)]
pub(super) fn op_smudgy_mapper_room_place_has_tag(
    state: &OpState,
    #[cppgc] room: &JSRoom,
    #[string] key: &str,
    #[string] tag: &str,
) -> Result<bool, MapperError> {
    let (atlas, map, own, place) = read_room_place(state, room.1, key)?;
    let tag = smudgy_cloud::mapper::normalize_tag(tag);
    Ok(tags_in(&atlas, room, map, own, place).contains(&tag))
}

/// `room.in(place).exits`: the exits `place` keeps on this room.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_room_place_exits(
    state: &OpState,
    #[cppgc] room: &JSRoom,
    #[string] key: &str,
) -> Result<Vec<JSExit>, MapperError> {
    let (atlas, map, own, place) = read_room_place(state, room.1, key)?;
    let number = room.0.get_room_number();
    let sources = sources_for(state);
    if place == own {
        return Ok(room
            .0
            .get_exits()
            .iter()
            .map(|exit| {
                JSExit::of(
                    exit,
                    room.1,
                    number,
                    own,
                    atlas.can_follow_exit(sources, exit),
                )
            })
            .collect());
    }
    Ok(atlas
        .get_area(&map)
        .and_then(|area| {
            area.document_layer(place).map(|layer| {
                layer
                    .exits_on(&RoomKey::new(room.1, number))
                    .iter()
                    .map(|exit| {
                        JSExit::of(
                            exit,
                            room.1,
                            number,
                            place,
                            atlas.can_follow_exit(sources, exit),
                        )
                    })
                    .collect()
            })
        })
        .unwrap_or_default())
}

/// Read exits from authorized sources. Unreadable destinations are redacted
/// separately, so their absence never hides the owning source's exit content.
pub(super) fn room_exits(state: &OpState, room: &JSRoom) -> Vec<JSExit> {
    let number = room.0.get_room_number();
    let atlas = current_atlas(state);
    let sources = sources_for(state);
    let own = atlas
        .as_deref()
        .map_or(SourceId::Map, |atlas| place_of(atlas, room.1).1);
    let mut exits: Vec<JSExit> = room
        .0
        .get_exits()
        .iter()
        .map(|exit| {
            JSExit::of(
                exit,
                room.1,
                number,
                own,
                atlas
                    .as_deref()
                    .is_some_and(|atlas| atlas.can_follow_exit(sources, exit)),
            )
        })
        .collect();
    if sources == Sources::Shown
        && let Some(area) = atlas
            .as_deref()
            .and_then(|atlas| atlas.get_area(&place_of(atlas, room.1).0))
    {
        let atlas = atlas.as_deref().expect("the area came from it");
        for layer in area
            .map_document_layer()
            .into_iter()
            .chain(area.source_layers())
        {
            if layer.source() == own {
                continue;
            }
            exits.extend(
                layer
                    .exits_on(&RoomKey::new(room.1, number))
                    .iter()
                    .map(|exit| {
                        JSExit::of(
                            exit,
                            room.1,
                            number,
                            layer.source(),
                            atlas.can_follow_exit(sources, exit),
                        )
                    }),
            );
        }
    }
    exits
}

/// `area.in(place).data(key)`: the place's own property (keyed by no room), or `undefined`.
#[op2]
#[string]
pub(super) fn op_smudgy_mapper_area_place_data(
    state: &OpState,
    #[cppgc] area: &JSArea,
    #[string] key: &str,
    #[string] name: &str,
) -> Result<Option<String>, MapperError> {
    let (atlas, map, own, place) = read_place(state, *area.0.get_id(), key)?;
    if place == own {
        return Ok(area.0.get_property(name).map(str::to_owned));
    }
    Ok(atlas.get_area(&map).and_then(|map_area| {
        layer_of(&map_area, place)?
            .area()
            .get_property(name)
            .map(str::to_owned)
    }))
}

/// `room.tags`: the union of every readable place's tags for this room, UPPERCASE and sorted.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_get_room_tags(
    state: &OpState,
    #[cppgc] room: &JSRoom,
) -> Vec<String> {
    let mut tags: Vec<String> = room.0.tags().map(String::from).collect();
    let others = place_rooms(state, room.1, room.0.get_room_number());
    if others.is_empty() {
        return tags;
    }
    for (_, data) in others {
        tags.extend(data.tags().map(str::to_uppercase));
    }
    tags.sort();
    tags.dedup();
    tags
}

/// `room.hasTag(tag)`: whether any readable place tags this room, case-insensitive.
#[op2(fast)]
pub(super) fn op_smudgy_mapper_has_tag(
    state: &OpState,
    #[cppgc] room: &JSRoom,
    #[string] tag: &str,
) -> bool {
    room.0.has_tag(tag)
        || place_rooms(state, room.1, room.0.get_room_number())
            .iter()
            .any(|(_, data)| data.has_tag(tag))
}

/// The room's own place's data, readable to this isolate: the map's always, any other place's
/// with `smudgy.secrets` read.
fn own_place(state: &OpState, room: &JSRoom) -> Option<SourceId> {
    let own = current_atlas(state)
        .as_deref()
        .map_or(SourceId::Map, |atlas| place_of(atlas, room.1).1);
    (own.is_map() || secrets_readable(state)).then_some(own)
}

/// `room.combinedData(key?)`: every readable place's value for `name` on this room (every
/// property when `name` is absent), the room's own place first, then the map's other places in
/// place order. Nothing is merged or ranked.
#[op2]
#[serde]
#[allow(clippy::needless_pass_by_value)] // op2 hands optional strings over owned
pub(super) fn op_smudgy_mapper_room_combined_data(
    state: &OpState,
    #[cppgc] room: &JSRoom,
    #[string] name: Option<String>,
) -> Vec<JsPlaceData> {
    let wanted = |property: &str| name.as_deref().is_none_or(|name| name == property);
    let mut entries = Vec::new();
    let mut add = |place: SourceId, data: &RoomCache| {
        let mut properties: Vec<_> = data
            .properties()
            .filter(|(property, _)| wanted(property))
            .collect();
        properties.sort_unstable();
        entries.extend(properties.into_iter().map(|(property, value)| JsPlaceData {
            place: place_key(place),
            key: property.to_owned(),
            data: value.to_owned(),
        }));
    };
    if let Some(own) = own_place(state, room) {
        add(own, &room.0);
    }
    for (place, data) in place_rooms(state, room.1, room.0.get_room_number()) {
        add(place, &data);
    }
    entries
}

/// `room.combinedTags()`: each readable place's tags for this room, the room's own place first,
/// then the map's other places in place order; each place's tags UPPERCASE and sorted.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_room_combined_tags(
    state: &OpState,
    #[cppgc] room: &JSRoom,
) -> Vec<(String, String)> {
    let mut entries = Vec::new();
    let mut add = |place: SourceId, data: &RoomCache| {
        let mut tags: Vec<String> = data.tags().map(str::to_uppercase).collect();
        tags.sort();
        tags.dedup();
        entries.extend(tags.into_iter().map(|tag| (place_key(place), tag)));
    };
    if let Some(own) = own_place(state, room) {
        add(own, &room.0);
    }
    for (place, data) in place_rooms(state, room.1, room.0.get_room_number()) {
        add(place, &data);
    }
    entries
}

/// The places a search reads: every readable place for an empty `key` (the map's alone without
/// `smudgy.secrets` read), else the one `key` names. On an area the place is checked against
/// it; across the atlas a Secret must be one the caller reads.
fn search_places(
    state: &OpState,
    atlas: &AtlasCache,
    area_id: Option<AreaId>,
    key: &str,
) -> Result<Places, MapperError> {
    if key.is_empty() {
        return Ok(if secrets_readable(state) {
            Places::All
        } else {
            Places::Only(SourceId::Map)
        });
    }
    let place = readable_place(state, key)?;
    match area_id {
        Some(area_id) => {
            resolve_place(state, atlas, area_id, place)?;
        }
        None => {
            if let SourceId::Secret(id) = place
                && secret_on_map(atlas, id).is_none()
            {
                return Err(MapperError::SecretNotFound);
            }
        }
    }
    Ok(Places::Only(place))
}

fn refs(state: &OpState, keys: Vec<RoomKey>) -> Vec<JsRoomRef> {
    keys.into_iter()
        .filter(|key| !hides_area(state, key.area_id))
        .map(|key| (ScriptUuid(key.area_id.0), key.room_number.0))
        .collect()
}

/// An atlas-wide search: `mapper.findRooms*`.
fn find_in_atlas(
    state: &OpState,
    query: RoomQuery<'_>,
    key: &str,
) -> Result<Vec<JsRoomRef>, MapperError> {
    ensure_mapper(state, false)?;
    if !key.is_empty() {
        readable_place(state, key)?;
    }
    let Some(atlas) = current_atlas(state) else {
        return Ok(Vec::new());
    };
    let places = search_places(state, &atlas, None, key)?;
    Ok(refs(state, atlas.find_rooms_in(query, places)))
}

/// An area's search: `area.findRooms*`. The area answers for itself even when its map is
/// turned off: naming it is asking for it.
fn find_in_area(
    state: &OpState,
    area: &JSArea,
    query: RoomQuery<'_>,
    key: &str,
) -> Result<Vec<JsRoomRef>, MapperError> {
    let area_id = *area.0.get_id();
    let atlas = current_atlas(state).ok_or(MapperError::MapperNotEnabled)?;
    let places = search_places(state, &atlas, Some(area_id), key)?;
    // The area as it is now, or the handle's snapshot of one no longer loaded.
    let found = match atlas
        .source_of(&area_id)
        .and_then(|(map, _)| atlas.get_area(&map))
    {
        Some(map) => map
            .find_rooms_in(query, places)
            .into_iter()
            .filter(|key| key.area_id == area_id)
            .collect(),
        None => atlas.get_area(&area_id).map_or_else(
            || area.0.find_rooms_in(query, places),
            |current| current.find_rooms_in(query, places),
        ),
    };
    Ok(refs(state, found))
}

/// `mapper.findRoomsByProperty(name, value, { in })`.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_find_rooms_by_property(
    state: &OpState,
    #[string] name: &str,
    #[string] value: &str,
    #[string] key: &str,
) -> Result<Vec<JsRoomRef>, MapperError> {
    find_in_atlas(state, RoomQuery::Property(name, value), key)
}

/// `mapper.findRoomsWithProperty(name, { in })`.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_find_rooms_with_property(
    state: &OpState,
    #[string] name: &str,
    #[string] key: &str,
) -> Result<Vec<JsRoomRef>, MapperError> {
    find_in_atlas(state, RoomQuery::PropertyName(name), key)
}

/// `mapper.findRoomsWithTag(tag, { in })`.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_find_rooms_with_tag(
    state: &OpState,
    #[string] tag: &str,
    #[string] key: &str,
) -> Result<Vec<JsRoomRef>, MapperError> {
    find_in_atlas(state, RoomQuery::Tag(tag), key)
}

/// `area.findRoomsByProperty(name, value, { in })`.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_find_area_rooms_by_property(
    state: &OpState,
    #[cppgc] area: &JSArea,
    #[string] name: &str,
    #[string] value: &str,
    #[string] key: &str,
) -> Result<Vec<JsRoomRef>, MapperError> {
    find_in_area(state, area, RoomQuery::Property(name, value), key)
}

/// `area.findRoomsWithProperty(name, { in })`.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_find_area_rooms_with_property(
    state: &OpState,
    #[cppgc] area: &JSArea,
    #[string] name: &str,
    #[string] key: &str,
) -> Result<Vec<JsRoomRef>, MapperError> {
    find_in_area(state, area, RoomQuery::PropertyName(name), key)
}

/// `area.findRoomsWithTag(tag, { in })`.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_find_area_rooms_with_tag(
    state: &OpState,
    #[cppgc] area: &JSArea,
    #[string] tag: &str,
    #[string] key: &str,
) -> Result<Vec<JsRoomRef>, MapperError> {
    find_in_area(state, area, RoomQuery::Tag(tag), key)
}

/// The nearest reachable room whose tags in the searched places carry every tag in `required`
/// and none in `excluded` (case-insensitive), as a room ref or `null`. Backs
/// `findNearestRoomWithTag(s)`: the candidates come from one indexed lookup per place, and the
/// walk is the routing walk, which takes every hidden door the isolate sees.
#[op2]
#[serde]
#[allow(clippy::needless_pass_by_value)] // op2 hands serde arguments over owned
pub(super) fn op_smudgy_mapper_find_nearest_room_with_tags(
    state: &OpState,
    #[string] from_area_id: &str,
    from_room_number: i32,
    #[serde] required: Vec<String>,
    #[serde] excluded: Vec<String>,
    #[string] key: &str,
) -> Result<Option<JsRoomRef>, MapperError> {
    ensure_mapper(state, false)?;
    let atlas = current_atlas(state).ok_or(MapperError::MapperNotEnabled)?;
    let places = search_places(state, &atlas, None, key)?;
    let from = RoomKey {
        area_id: AreaId(parse_id(from_area_id)?),
        room_number: RoomNumber(from_room_number),
    };
    let nearest = atlas.find_nearest_room_matching_tags_in(
        &from,
        &required,
        &excluded,
        places,
        sources_for(state),
    );
    if let Some(room_key) = &nearest {
        note_navigation(state, room_key.area_id);
    }
    Ok(nearest.map(|room_key| (ScriptUuid(room_key.area_id.0), room_key.room_number.0)))
}

/// `area.secrets.list()`: the map's Secrets the caller reads, in place order. Empty on a
/// Secret's own area, on local and session maps, and for an isolate without `smudgy.secrets`
/// read.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_list_area_secrets(
    state: &OpState,
    #[cppgc] area: &JSArea,
) -> Vec<JsSecret> {
    if !secrets_readable(state) {
        return Vec::new();
    }
    let Some(atlas) = current_atlas(state) else {
        return Vec::new();
    };
    let Some(map) = atlas.get_area(area.0.get_id()) else {
        return Vec::new();
    };
    if map.map_id().is_some() {
        return Vec::new();
    }
    map.source_layers()
        .iter()
        .filter_map(|layer| match layer.source() {
            SourceId::Secret(id) => secret_on_map(&atlas, id),
            _ => None,
        })
        .collect()
}

/// The Secret `id` on the map `area`, or the same "not found" for one that does not exist, one
/// the caller cannot read, and one on another map.
fn area_secret(state: &OpState, area: &JSArea, id: &str) -> Result<JsSecret, MapperError> {
    ensure_place_read(state, SourceId::Secret(Uuid::nil()))?;
    let id = parse_id(id)?;
    let atlas = current_atlas(state).ok_or(MapperError::MapperNotEnabled)?;
    match atlas.source_of(&AreaId(id)) {
        Some((map, _)) if map == *area.0.get_id() => {
            secret_on_map(&atlas, id).ok_or(MapperError::SecretNotFound)
        }
        _ => Err(MapperError::SecretNotFound),
    }
}

/// `area.secrets.get(id)`.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_get_area_secret(
    state: &OpState,
    #[cppgc] area: &JSArea,
    #[string] id: &str,
) -> Result<JsSecret, MapperError> {
    area_secret(state, area, id)
}

/// `area.secrets.exists(id)`: whether `area.secrets.get(id)` would find it. Something that is
/// not an id finds nothing, and nothing is found without `smudgy.secrets` read.
#[op2(fast)]
pub(super) fn op_smudgy_mapper_area_secret_exists(
    state: &OpState,
    #[cppgc] area: &JSArea,
    #[string] id: &str,
) -> Result<bool, MapperError> {
    // Without `smudgy.secrets` read there are no Secrets to find, whatever the id.
    if !secrets_readable(state) {
        return Ok(false);
    }
    match area_secret(state, area, id) {
        Ok(_) => Ok(true),
        Err(MapperError::SecretNotFound | MapperError::InvalidId(_)) => Ok(false),
        Err(other) => Err(other),
    }
}

/// `mapper.getSecretById(id)`: the Secret, or the same "not found" for one that does not exist
/// and one the caller cannot read.
#[op2]
#[serde]
pub(super) fn op_smudgy_mapper_get_secret(
    state: &OpState,
    #[string] id: &str,
) -> Result<JsSecret, MapperError> {
    ensure_mapper(state, false)?;
    ensure_place_read(state, SourceId::Secret(Uuid::nil()))?;
    let id = parse_id(id)?;
    let atlas = current_atlas(state).ok_or(MapperError::MapperNotEnabled)?;
    secret_on_map(&atlas, id).ok_or(MapperError::SecretNotFound)
}

/// The mapper and the map Secret `id` sits on, for a lifecycle call.
fn secret_target(
    state: &Rc<RefCell<OpState>>,
    id: &str,
) -> Result<(Mapper, AreaId, Uuid), MapperError> {
    let state = state.borrow();
    ensure_mapper(&state, false)?;
    ensure_secrets_manage(&state)?;
    let id = parse_id(id)?;
    let mapper = state
        .try_borrow::<Mapper>()
        .cloned()
        .ok_or(MapperError::MapperNotEnabled)?;
    let atlas = mapper.get_current_atlas();
    secret_on_map(&atlas, id).ok_or(MapperError::SecretNotFound)?;
    let (map, _) = atlas
        .source_of(&AreaId(id))
        .ok_or(MapperError::SecretNotFound)?;
    Ok((mapper, map, id))
}

/// The Secret as the republished projection shows it, or as the server answered.
fn secret_after(mapper: &Mapper, map: AreaId, id: Uuid, summary: &SecretSummary) -> JsSecret {
    let atlas = mapper.get_current_atlas();
    secret_on_map(&atlas, id).unwrap_or_else(|| JsSecret::from_summary(map, id, summary))
}

fn secret_failed(operation: &'static str) -> impl Fn(smudgy_cloud::CloudError) -> MapperError {
    move |error| match error {
        smudgy_cloud::CloudError::NotFoundOrNoAccess => MapperError::SecretNotFound,
        other => MapperError::OperationFailed {
            operation,
            message: other.to_string(),
        },
    }
}

/// `area.secrets.create({ name, color, ownership, clanId })`: a new Secret on a cloud map, in
/// one request. Without `ownership` (or with `"owner"`) it is an owner Secret on a map the caller
/// owns; `"members"` and `"clan"` make a Clan Secret of `clan_id`, or of the map's own clan. A
/// Clan-owned Secret's creator starts as its Contributor.
#[op2(async(lazy))]
#[serde]
pub(super) async fn op_smudgy_mapper_create_secret(
    state: Rc<RefCell<OpState>>,
    #[string] area_id: String,
    #[string] name: String,
    #[string] color: Option<String>,
    #[string] ownership: Option<String>,
    #[string] clan_id: Option<String>,
) -> Result<JsSecret, MapperError> {
    let map = AreaId(parse_id(&area_id)?);
    let clan_id = clan_id.as_deref().map(parse_id).transpose()?;
    let mapper = {
        let state = state.borrow();
        ensure_mapper(&state, false)?;
        ensure_secrets_manage(&state)?;
        let mapper = state
            .try_borrow::<Mapper>()
            .cloned()
            .ok_or(MapperError::MapperNotEnabled)?;
        if mapper.get_current_atlas().source_of(&map).is_some() {
            return Err(MapperError::PlaceMismatch);
        }
        mapper
    };
    let named_clan = || {
        clan_id
            .or_else(|| {
                mapper
                    .get_current_atlas()
                    .get_area(&map)
                    .and_then(|area| area.meta().clan_id)
            })
            .ok_or_else(|| MapperError::OperationFailed {
                operation: "create secret",
                message: "a Clan Secret on a user's map names its clan (clanId)".to_string(),
            })
    };
    let owner = match ownership.as_deref() {
        None | Some("owner") => NewSecretOwner::Me,
        Some("members") => NewSecretOwner::Members {
            clan_id: named_clan()?,
        },
        Some("clan") => NewSecretOwner::Clan {
            clan_id: named_clan()?,
        },
        Some(other) => {
            return Err(MapperError::OperationFailed {
                operation: "create secret",
                message: format!(
                    "{other:?} is not an ownership (\"owner\", \"members\" or \"clan\")"
                ),
            });
        }
    };
    let secret = NewSecret { name, color, owner };
    let summary = mapper
        .create_secret_as(map, &secret)
        .await
        .map_err(secret_failed("create secret"))?;
    let SourceId::Secret(id) = summary.source else {
        return Err(MapperError::OperationFailed {
            operation: "create secret",
            message: format!("the server answered with source {}", summary.source),
        });
    };
    Ok(secret_after(&mapper, map, id, &summary))
}

/// `secret.update({ name, color })`: one request carrying both. `set_color` says whether the
/// color changes; a `None` color then hands it back to the palette.
#[op2(async(lazy))]
#[serde]
pub(super) async fn op_smudgy_mapper_update_secret(
    state: Rc<RefCell<OpState>>,
    #[string] secret_id: String,
    #[string] name: Option<String>,
    set_color: bool,
    #[string] color: Option<String>,
) -> Result<JsSecret, MapperError> {
    let (mapper, map, id) = secret_target(&state, &secret_id)?;
    let change = SecretChange {
        name,
        color: match (set_color, color) {
            (false, _) => SecretColorChange::Keep,
            (true, None) => SecretColorChange::Clear,
            (true, Some(color)) => SecretColorChange::Set(color),
        },
    };
    let summary = mapper
        .update_secret(map, &SourceId::Secret(id), &change)
        .await
        .map_err(secret_failed("update secret"))?;
    Ok(secret_after(&mapper, map, id, &summary))
}

/// `secret.delete()`: deletes it and everything in it.
#[op2(async(lazy), fast)]
pub(super) async fn op_smudgy_mapper_delete_secret(
    state: Rc<RefCell<OpState>>,
    #[string] secret_id: String,
) -> Result<(), MapperError> {
    let (mapper, map, id) = secret_target(&state, &secret_id)?;
    mapper
        .delete_secret(map, &SourceId::Secret(id))
        .await
        .map_err(secret_failed("delete secret"))
}

#[cfg(test)]
mod tests {
    use super::camel_case;

    #[test]
    fn actions_reach_scripts_in_camel_case() {
        for (wire, script) in [
            ("read", "read"),
            ("manage_access", "manageAccess"),
            ("manage_ownership", "manageOwnership"),
        ] {
            assert_eq!(camel_case(wire), script);
        }
    }
}
