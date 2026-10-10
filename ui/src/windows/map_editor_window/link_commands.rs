//! The link editor's edits as undoable commands. Each is one command with
//! one write per place it touches, usually one: removing a link, making it
//! one-way or two-way, swapping a one-way link's ends, changing the
//! direction an end leaves in, moving an end to another room, and editing
//! the fields of one or both of its exits. A link's place is decided by its
//! ends ([`links::place_for`]); two places are never linked.
//!
//! New links and rewritten ones are built in wire form, naming map rooms by
//! their numbers and a place's own rooms with the place, so an end may be a
//! map room the place keeps nothing for yet. Deletes and restores read the
//! document holding the link, whose operations its place translates.

use std::sync::Arc;

use smudgy_cloud::link_edits;
use smudgy_cloud::mapper::AtlasCache;
use smudgy_cloud::mapper::exit_cache::ExitCache;
use smudgy_cloud::mutation::AreaMutation;
use smudgy_cloud::{
    AreaId, Connection, ConnectionArgs, ConnectionDash, ConnectionEndpoint, ConnectionId,
    ConnectionRouting, CornerStyle, DEFAULT_CONNECTION_COLOR, DEFAULT_CONNECTION_THICKNESS,
    ExitArgs, ExitDirection, ExitId, ExitUpdates, PortMode, RoomAddress, RoomNumber, SegmentShape,
    SourceId, default_anchor_for_direction,
};

use super::commands::{self, CoalesceKey, Command, EntityRef, FieldId, Mutation};
use super::document::Document;
use super::links::{self, LinkView, RoomId};

/// One end of a link, in travel order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum End {
    From,
    To,
}

/// Why a link edit can't be made, as the i18n key of the notice to show.
pub type Refusal = &'static str;

/// One write to `place` of map `map`, in wire form.
fn place_batch(
    map: AreaId,
    place: SourceId,
    operations: Vec<AreaMutation>,
    what: &str,
) -> Mutation {
    if place.is_map() {
        Mutation::AreaBatch {
            area_id: map,
            operations,
            description: what.to_string(),
        }
    } else {
        Mutation::SourceBatch {
            area_id: map,
            source: place,
            operations,
            description: what.to_string(),
            split_paired_exit: false,
        }
    }
}

/// Room `room` as a write to `place` of map `map` names it: a map room by
/// its number, one of `place`'s own rooms with `place` named. `None` for
/// another place's room or another map's.
fn wire_room(map: AreaId, place: SourceId, room: RoomId) -> Option<(RoomNumber, Option<SourceId>)> {
    if room.map != map {
        return None;
    }
    if room.place.is_map() {
        Some((room.number, None))
    } else if room.place == place {
        Some((room.number, Some(place)))
    } else {
        None
    }
}

/// Where an exit written to `place` of map `map` leads when it leads into
/// `room`: a room of this map as [`wire_room`] names it, another map's room,
/// or a room of another map's Secret, named by the Secret's own area (the
/// mapper writes it as the wire names it). Another map's Private additions
/// are no destination.
fn wire_destination(
    map: AreaId,
    place: SourceId,
    room: RoomId,
) -> Option<(AreaId, RoomNumber, Option<SourceId>)> {
    if room.map == map {
        let (number, source) = wire_room(map, place, room)?;
        return Some((map, number, source));
    }
    match room.place {
        SourceId::Map => Some((room.map, room.number, None)),
        SourceId::Secret(secret) => Some((AreaId(secret), room.number, None)),
        SourceId::Private => None,
    }
}

/// The exit `cached` as a create recreates it, under its own id, in
/// `connection`, leading where the wire names its destination.
fn exit_args(cached: &ExitCache, connection: ConnectionId) -> ExitArgs {
    ExitArgs {
        to_source: cached.to_exit().to_source,
        id: Some(cached.id),
        connection_id: Some(connection),
        new_connection_id: None,
        from_direction: cached.from_direction,
        to_area_id: cached.wire_to_area_id(),
        to_room_number: cached.to_room_number,
        to_direction: cached.to_direction,
        path: cached.path.clone(),
        is_hidden: cached.is_hidden,
        door: cached.door.clone(),
        weight: cached.weight,
        command: cached.command.clone(),
    }
}

/// Points `args` at `destination` (wire form).
fn lead_to(args: &mut ExitArgs, destination: Option<(AreaId, RoomNumber, Option<SourceId>)>) {
    args.to_area_id = destination.map(|(area, _, _)| area);
    args.to_room_number = destination.map(|(_, number, _)| number);
    args.to_source = destination.and_then(|(_, _, source)| source);
}

/// A link's visual settings, kept when it is rewritten.
#[derive(Debug, Clone)]
struct Style {
    routing: ConnectionRouting,
    segment_shape: SegmentShape,
    corner: CornerStyle,
    route_points: Vec<smudgy_cloud::MapPoint>,
    dash: ConnectionDash,
    color: String,
    thickness: f32,
}

impl Default for Style {
    fn default() -> Self {
        Self {
            routing: ConnectionRouting::Simple,
            segment_shape: SegmentShape::Direct,
            corner: CornerStyle::Sharp,
            route_points: Vec::new(),
            dash: ConnectionDash::Solid,
            color: DEFAULT_CONNECTION_COLOR.to_string(),
            thickness: DEFAULT_CONNECTION_THICKNESS,
        }
    }
}

impl Style {
    fn of(connection: &Connection) -> Self {
        Self {
            routing: connection.routing,
            segment_shape: connection.segment_shape,
            corner: connection.corner,
            route_points: connection.route_points.clone(),
            dash: connection.dash,
            color: connection.color.clone(),
            thickness: connection.thickness,
        }
    }

    /// The style once the link's rooms changed: its colour, line, corners
    /// and segments stay; a route drawn between the old rooms goes.
    fn rerouted(mut self) -> Self {
        if matches!(
            self.routing,
            ConnectionRouting::Manual | ConnectionRouting::Automatic
        ) {
            self.routing = ConnectionRouting::Simple;
        }
        self.route_points.clear();
        self
    }
}

/// A link to write: its id and style, its two ends' rooms (the far one on
/// another map, or nowhere), and its exits.
struct NewLink {
    connection: ConnectionId,
    style: Style,
    from: RoomId,
    from_anchor: Option<ConnectionEndpoint>,
    to: Option<RoomId>,
    to_anchor: Option<ConnectionEndpoint>,
    forward: ExitArgs,
    back: Option<ExitArgs>,
}

/// `link`'s operations, written to `place` of map `map`: the connection,
/// then its exits. `None` when an end can't be named in `place`.
fn link_operations(map: AreaId, place: SourceId, link: NewLink) -> Option<Vec<AreaMutation>> {
    let from = wire_room(map, place, link.from)?;
    let to_room = match link.to {
        Some(to) if to.map == map => Some(wire_room(map, place, to)?),
        _ => None,
    };
    let endpoint = |(number, source): (RoomNumber, Option<SourceId>),
                    kept: Option<ConnectionEndpoint>,
                    direction: ExitDirection| {
        let (side, port_offset, port_mode) = kept.map_or_else(
            || {
                let (side, offset) = default_anchor_for_direction(direction, None);
                (side, offset, PortMode::AutoPinned)
            },
            |kept| (kept.side, kept.port_offset, kept.port_mode),
        );
        ConnectionEndpoint {
            room_number: number,
            source,
            side,
            port_offset,
            port_mode,
        }
    };
    let mut endpoint_a = endpoint(from, link.from_anchor, link.forward.from_direction);
    let arrival = link
        .back
        .as_ref()
        .map(|back| back.from_direction)
        .or(link.forward.to_direction)
        .unwrap_or_else(|| link.forward.from_direction.opposite());
    let mut endpoint_b = to_room.map(|to| endpoint(to, link.to_anchor, arrival));
    if endpoint_b.is_some_and(|b| {
        endpoint_a.address().connection_order_key() > b.address().connection_order_key()
    }) {
        std::mem::swap(&mut endpoint_a, endpoint_b.as_mut().expect("checked"));
    }
    let mut forward = link.forward;
    forward.connection_id = Some(link.connection);
    forward.new_connection_id = None;
    let destination = match link.to {
        Some(to) => Some(wire_destination(map, place, to)?),
        None => None,
    };
    lead_to(&mut forward, destination);
    let mut operations = vec![
        AreaMutation::CreateConnection {
            body: ConnectionArgs {
                id: link.connection,
                endpoint_a,
                endpoint_b,
                routing: link.style.routing,
                segment_shape: link.style.segment_shape,
                corner: link.style.corner,
                route_points: link.style.route_points,
                dash: link.style.dash,
                color: link.style.color,
                thickness: link.style.thickness,
            },
        },
        AreaMutation::CreateExit {
            room_source: from.1,
            room_number: from.0,
            body: forward,
        },
    ];
    if let (Some(mut back), Some(to)) = (link.back, to_room) {
        back.connection_id = Some(link.connection);
        back.new_connection_id = None;
        lead_to(&mut back, Some((map, from.0, from.1)));
        operations.push(AreaMutation::CreateExit {
            room_source: to.1,
            room_number: to.0,
            body: back,
        });
    }
    Some(operations)
}

/// The default exit back along a new link leaving by `forward`: it leaves
/// in the direction the forward exit arrives from (else the opposite one),
/// arriving where the forward exit leaves. It shares the passage's door and
/// hidden flag (the two sides start alike) and its weight; its command and
/// path start empty. For a link already made, see [`way_back`].
fn reverse_of(forward: &ExitArgs) -> ExitArgs {
    ExitArgs {
        id: Some(ExitId::new()),
        from_direction: forward
            .to_direction
            .unwrap_or_else(|| forward.from_direction.opposite()),
        to_direction: Some(forward.from_direction),
        is_hidden: forward.is_hidden,
        door: forward.door.clone(),
        weight: forward.weight,
        ..ExitArgs::default()
    }
}

/// A new link from `from` leaving in `direction` to `to` (a room of this
/// map, of one of its places, or of another map): two-way within the map,
/// with the return leaving in the opposite direction; one-way into another
/// map. It goes into the place at either end, else `fallback` (where new
/// content goes). Returns the command and the new link's id.
///
/// # Errors
/// The notice to show when the ends are two places' rooms, or `to` is in
/// another map's Private additions.
pub fn create(
    map: AreaId,
    from: RoomId,
    direction: ExitDirection,
    to: RoomId,
    fallback: SourceId,
) -> Result<(Command, ConnectionId), Refusal> {
    let place = links::place_for(map, &[from, to], fallback).ok_or("mapper-link-two-secrets")?;
    if to.map != map && to.place == SourceId::Private {
        return Err("cloud-error-secret-link-into-other-map");
    }
    let connection = ConnectionId::new();
    let forward = ExitArgs {
        id: Some(ExitId::new()),
        from_direction: direction,
        to_direction: Some(direction.opposite()),
        weight: 1.0,
        ..ExitArgs::default()
    };
    let back = (to.map == map).then(|| reverse_of(&forward));
    let operations = link_operations(
        map,
        place,
        NewLink {
            connection,
            style: Style::default(),
            from,
            from_anchor: None,
            to: Some(to),
            to_anchor: None,
            forward,
            back,
        },
    )
    .ok_or("mapper-link-two-secrets")?;
    let command = Command::new(
        vec![place_batch(map, place, operations, "Create link")],
        vec![place_batch(
            map,
            place,
            vec![AreaMutation::DeleteLink {
                connection_id: connection,
            }],
            "Undo link creation",
        )],
    );
    Ok((command, connection))
}

/// A new exit from `from` leaving in `direction` with no destination yet
/// (an unexplored direction): a one-way link with no far end, kept in the
/// place of `from` when it is a place's own room, else in `fallback`.
/// "Change ▾" on its To end gives it a destination later ([`retarget`]).
/// Returns the command and the new link's id.
///
/// # Errors
/// The notice to show when `from` can't be named in its place.
pub fn create_dangling(
    map: AreaId,
    from: RoomId,
    direction: ExitDirection,
    fallback: SourceId,
) -> Result<(Command, ConnectionId), Refusal> {
    let place = links::place_for(map, &[from], fallback).ok_or("mapper-link-not-changed")?;
    let connection = ConnectionId::new();
    let forward = ExitArgs {
        id: Some(ExitId::new()),
        from_direction: direction,
        weight: 1.0,
        ..ExitArgs::default()
    };
    let operations = link_operations(
        map,
        place,
        NewLink {
            connection,
            style: Style::default(),
            from,
            from_anchor: None,
            to: None,
            to_anchor: None,
            forward,
            back: None,
        },
    )
    .ok_or("mapper-link-not-changed")?;
    let command = Command::new(
        vec![place_batch(map, place, operations, "Create exit")],
        vec![place_batch(
            map,
            place,
            vec![AreaMutation::DeleteLink {
                connection_id: connection,
            }],
            "Undo exit creation",
        )],
    );
    Ok((command, connection))
}

/// The exit back along `forward`, which leaves room `from_room` of the
/// link's document for another of its rooms ([`link_edits::reverse_of`]),
/// with the passage's door: a link made two-way, or swapped, starts with
/// the same door on both sides.
fn way_back(map: AreaId, from_room: RoomAddress, forward: &ExitCache) -> Option<ExitArgs> {
    let mut back = link_edits::reverse_of(map, from_room, forward)?;
    back.door.clone_from(&forward.door);
    Some(back)
}

/// The document holding `view`'s link, and the link.
fn holding<'a>(
    area: &'a smudgy_cloud::mapper::area_cache::AreaCache,
    view: &LinkView,
) -> Option<(Document<'a>, &'a Connection)> {
    let document = Document::of_connection(area, view.connection)?;
    let connection = document.content().get_connection(view.connection)?;
    Some((document, connection))
}

/// Removes, in another map, the link its exit `back` belongs to; and its
/// restore. `None` where the viewer may not remove it: that map keeps its
/// way back, as it keeps any exit of its own.
fn remove_elsewhere(atlas: &AtlasCache, back: links::ExitAt) -> Option<(Mutation, Mutation)> {
    let area = atlas.get_area(&back.map)?;
    if !super::secrets::can_remove(&area, back.place) {
        return None;
    }
    let document = Document::of(&area, back.place)?;
    let content = document.content();
    let exit = content
        .get_room_at(back.room)?
        .get_exits()
        .iter()
        .find(|exit| exit.id == back.exit)?;
    let connection = content.get_connection(exit.connection_id)?;
    Some((
        document.batch(
            vec![AreaMutation::DeleteLink {
                connection_id: connection.id,
            }],
            "Delete the way back",
        ),
        document.batch(
            commands::restore_link(content, connection, false),
            "Restore the way back",
        ),
    ))
}

/// Removes `view`'s link whole: its exits, and the way back another map
/// keeps where the viewer may remove it (else that way back stays, as the
/// other map's own exit), once this map's delete is acknowledged. Undo
/// brings it back under its ids with its route and style, and the way back
/// once that is acknowledged. Refused while an exit leads into a map that
/// wasn't shared with the viewer: undo couldn't restore where it leads.
#[must_use]
pub fn remove(atlas: &Arc<AtlasCache>, map: AreaId, view: &LinkView) -> Option<Command> {
    if [&view.from.exit, &view.to.exit]
        .into_iter()
        .flatten()
        .any(|exit| exit.to_unknown)
    {
        return None;
    }
    let command = commands::delete_connection(atlas, map, view.connection)?;
    Some(
        match view
            .return_elsewhere
            .and_then(|back| remove_elsewhere(atlas, back))
        {
            Some((delete, restore)) => command.then(delete, restore),
            None => command,
        },
    )
}

/// Makes `view`'s two-way link one-way: the To end's exit goes (or the way
/// back another map keeps). The link keeps its id, route and style.
#[must_use]
pub fn one_way(atlas: &Arc<AtlasCache>, map: AreaId, view: &LinkView) -> Option<Command> {
    if let (Some(exit), Some(room)) = (&view.to.exit, view.to.doc_room) {
        let area = atlas.get_area(&map)?;
        let (document, _) = holding(&area, view)?;
        return Some(Command::new(
            vec![document.batch(link_edits::make_one_way(exit.id), "Make link one-way")],
            vec![document.batch(
                vec![AreaMutation::CreateExit {
                    room_source: room.wire_source(),
                    room_number: room.number,
                    body: commands::restore_exit_args(exit, view.connection),
                }],
                "Make link two-way again",
            )],
        ));
    }
    let (delete, restore) = remove_elsewhere(atlas, view.return_elsewhere?)?;
    Some(Command::new(vec![delete], vec![restore]))
}

/// The far room of `view`'s link into another map when its way back would
/// show to more people than the link does, so the link stays one-way: a
/// link a Secret or Private additions keep, unless its near end is that
/// Secret's own room. The way back is the other map's to keep, and every
/// reader of the place holding it would see it; only an exit into a
/// Secret's room is hidden from those who don't read that Secret. `None`
/// for a link the map keeps, or one within this map.
#[must_use]
pub fn way_back_shows_more(view: &LinkView) -> Option<RoomId> {
    let here = view.from.room.room()?;
    let far = view.to.room.room()?;
    if far.map == here.map {
        return None;
    }
    match view.place {
        SourceId::Map => None,
        SourceId::Secret(_) if here.place == view.place => None,
        SourceId::Secret(_) | SourceId::Private => Some(far),
    }
}

/// Makes `view`'s one-way link two-way: an exit back joins it from the far
/// room, leaving in the direction the link arrives from (else the
/// opposite), with the passage's door. Into another map the way back is
/// that map's, a link of its own, and is made only where it stays as hidden
/// as the link ([`way_back_shows_more`]): a Secret's link from its own room
/// comes back by the other map's exit into that room. A reciprocal one-way
/// link already running back is paired with it instead.
#[must_use]
pub fn two_way(atlas: &Arc<AtlasCache>, map: AreaId, view: &LinkView) -> Option<Command> {
    let area = atlas.get_area(&map)?;
    let (document, _) = holding(&area, view)?;
    let forward = view.from.exit.as_ref()?;
    let from_room = view.from.doc_room?;
    if view.two_way() || forward.to_unknown || way_back_shows_more(view).is_some() {
        return None;
    }
    if let Some(to_room) = view.to.doc_room {
        if let Some(reciprocal) = super::reciprocal_pair_candidate(
            document.content(),
            map,
            from_room,
            to_room,
            forward.from_direction,
            forward
                .to_direction
                .unwrap_or_else(|| forward.from_direction.opposite()),
        ) {
            return commands::pair_connections(atlas, map, view.connection, reciprocal);
        }
        let back = way_back(map, from_room, forward)?;
        let id = back.id.expect("a new exit has an id");
        return Some(Command::new(
            vec![document.batch(
                link_edits::make_two_way(view.connection, to_room, back),
                "Make link two-way",
            )],
            vec![document.batch(
                vec![AreaMutation::DeleteExit { exit_id: id }],
                "Make link one-way again",
            )],
        ));
    }
    // Into another map: the way back is that map's to keep, leading into
    // this map's room or, for a Secret's own room, into that Secret (never
    // Private additions).
    let far = view.to.room.room()?;
    let here = view.from.room.room()?;
    let back_to = match here.place {
        SourceId::Map => (map, here.number, None),
        SourceId::Secret(secret) => (AreaId(secret), here.number, None),
        SourceId::Private => return None,
    };
    let other = atlas.get_area(&far.map)?;
    let far_document = Document::of(&other, far.place)?;
    let connection = ConnectionId::new();
    let mut back = reverse_of(&exit_args(forward, view.connection));
    back.new_connection_id = Some(connection);
    lead_to(&mut back, Some(back_to));
    Some(Command::new(
        vec![far_document.batch(
            vec![AreaMutation::CreateExit {
                room_source: (!far.place.is_map()).then_some(far.place),
                room_number: far.number,
                body: back,
            }],
            "Make link two-way",
        )],
        vec![far_document.batch(
            vec![AreaMutation::DeleteLink {
                connection_id: connection,
            }],
            "Make link one-way again",
        )],
    ))
}

/// Swaps `view`'s one-way link's ends: an exit back from the far room
/// joins the link first, then the link's exit goes (the other order would
/// take the link down with its last exit). Undo swaps them back the same
/// way. Only for a link within one document.
#[must_use]
pub fn swap(atlas: &Arc<AtlasCache>, map: AreaId, view: &LinkView) -> Option<Command> {
    let area = atlas.get_area(&map)?;
    let (document, _) = holding(&area, view)?;
    let forward = view.from.exit.as_ref()?;
    let (from_room, to_room) = (view.from.doc_room?, view.to.doc_room?);
    if view.two_way() || forward.to_unknown {
        return None;
    }
    let back = way_back(map, from_room, forward)?;
    let back_id = back.id.expect("a new exit has an id");
    Some(Command::new(
        vec![document.batch(
            link_edits::swap(forward.id, view.connection, to_room, back),
            "Swap link ends",
        )],
        vec![document.batch(
            vec![
                AreaMutation::CreateExit {
                    room_source: from_room.wire_source(),
                    room_number: from_room.number,
                    body: commands::restore_exit_args(forward, view.connection),
                },
                AreaMutation::DeleteExit { exit_id: back_id },
            ],
            "Swap link ends back",
        )],
    ))
}

/// The exit leaving `end` of `view`, and its room in the link's document.
fn end_exit(view: &LinkView, end: End) -> Option<(&ExitCache, RoomAddress)> {
    let side = match end {
        End::From => &view.from,
        End::To => &view.to,
    };
    Some((side.exit.as_ref()?, side.doc_room?))
}

/// Rewrites `view`'s link in place under its own ids: `change` edits its
/// two ends (rooms, exits) and its style before the link is written again,
/// as one write, after which undo writes it back as it was. For edits a
/// two-way link can't take exit by exit (an end's direction, its rooms).
fn rewrite(
    atlas: &Arc<AtlasCache>,
    map: AreaId,
    view: &LinkView,
    what: &str,
    change: impl FnOnce(&mut NewLink),
) -> Option<Command> {
    let area = atlas.get_area(&map)?;
    let (document, connection) = holding(&area, view)?;
    let place = document.source();
    let forward = view.from.exit.as_ref()?;
    // Endpoints keep their anchors where their rooms stay.
    let anchor = |room: Option<RoomAddress>| {
        let room = room?;
        [Some(connection.endpoint_a), connection.endpoint_b]
            .into_iter()
            .flatten()
            .find(|end| end.address() == room)
            .map(|end| ConnectionEndpoint {
                source: None,
                ..end
            })
    };
    let mut link = NewLink {
        connection: view.connection,
        style: Style::of(connection),
        from: view.from.room.room()?,
        from_anchor: anchor(view.from.doc_room),
        to: view.to.room.room(),
        to_anchor: anchor(view.to.doc_room),
        forward: exit_args(forward, view.connection),
        back: view
            .to
            .exit
            .as_ref()
            .map(|exit| exit_args(exit, view.connection)),
    };
    change(&mut link);
    let mut operations = vec![AreaMutation::DeleteLink {
        connection_id: view.connection,
    }];
    operations.extend(link_operations(map, place, link)?);
    Some(Command::new(
        vec![document.batch(operations, what)],
        vec![document.batch(
            commands::restore_link(document.content(), connection, true),
            format!("Undo {what}"),
        )],
    ))
}

/// Changes the direction `end` of `view`'s two-way link leaves in; the
/// other end's exit arrives from it, and the end's port moves to the new
/// direction's home. A one-way link's exit changes by itself (see the
/// window's direction edit).
#[must_use]
pub fn set_leaves(
    atlas: &Arc<AtlasCache>,
    map: AreaId,
    view: &LinkView,
    end: End,
    direction: ExitDirection,
) -> Option<Command> {
    end_exit(view, End::To)?;
    rewrite(atlas, map, view, "Change link direction", |link| {
        let back = link.back.as_mut().expect("a two-way link");
        match end {
            End::From => {
                link.forward.from_direction = direction;
                back.to_direction = Some(direction);
                link.from_anchor = None;
            }
            End::To => {
                back.from_direction = direction;
                link.forward.to_direction = Some(direction);
                link.to_anchor = None;
            }
        }
    })
}

/// Whether the viewer may change the way back `back` another map keeps.
#[must_use]
pub fn way_back_writable(atlas: &AtlasCache, back: links::ExitAt) -> bool {
    atlas
        .get_area(&back.map)
        .is_some_and(|area| super::secrets::can_write(&area, back.place))
}

/// The other exit's arrival once `end` of `view`'s link into another map,
/// whose way back that map keeps, turns to leave in `direction`: turning the
/// To end (the way back), the link's exit arrives from `direction`; turning
/// the From end, the way back does, where the viewer may change it (else
/// it keeps its arrival, as the other map's own exit). The write, and its
/// undo. `None` when the link has no such way back, or it stays as it is.
#[must_use]
pub fn arrival_follows(
    atlas: &Arc<AtlasCache>,
    map: AreaId,
    view: &LinkView,
    end: End,
    direction: ExitDirection,
) -> Option<(Mutation, Mutation)> {
    let back = view.return_elsewhere?;
    let arrives = |document: &Document<'_>, exit: &ExitCache| {
        let prior = commands::exit_updates_from_cache(exit);
        let mut updates = prior.clone();
        updates.to_direction = Some(direction);
        (
            document.batch(
                vec![AreaMutation::UpdateExit {
                    exit_id: exit.id,
                    body: updates,
                }],
                "Change link direction",
            ),
            document.batch(
                vec![AreaMutation::UpdateExit {
                    exit_id: exit.id,
                    body: prior,
                }],
                "Undo change link direction",
            ),
        )
    };
    match end {
        End::To => {
            let area = atlas.get_area(&map)?;
            let (document, _) = holding(&area, view)?;
            Some(arrives(&document, view.from.exit.as_ref()?))
        }
        End::From => {
            if !way_back_writable(atlas, back) {
                return None;
            }
            let other = atlas.get_area(&back.map)?;
            let far = Document::of(&other, back.place)?;
            let exit = far
                .content()
                .get_room_at(back.room)?
                .get_exits()
                .iter()
                .find(|exit| exit.id == back.exit)?;
            Some(arrives(&far, exit))
        }
    }
}

/// Moves `end` of `view`'s link to room `to`, never leaving it nowhere on
/// the way. Where the link's place stays the same it is rewritten under its
/// own ids as one write, keeping its exits' settings, colour and line (a
/// drawn route goes). Otherwise it is made again where its new ends put it,
/// under new ids, and only then does its old self go; undo likewise brings
/// the old link back before the new one goes. A way back another map kept
/// goes last, where the viewer may remove it, once this map's writes are
/// acknowledged (and comes back once their undo is): a refused write
/// discarded never takes the way back with it. Into another map a link runs
/// one way; its From end stays on this map. An exit with no destination
/// that gets one becomes what a new link there would be: two-way within the
/// map, arriving from the opposite direction, its way back sharing its door.
///
/// # Errors
/// The notice to show when the new ends are two places' rooms, the From
/// end would leave the map, or `to` is in another map's Private additions.
pub fn retarget(
    atlas: &Arc<AtlasCache>,
    map: AreaId,
    view: &LinkView,
    end: End,
    to: RoomId,
) -> Result<(Command, ConnectionId), Refusal> {
    let refused = "mapper-link-not-changed";
    let from = view.from.room.room().ok_or(refused)?;
    let (new_from, new_to) = match end {
        End::From => (to, view.to.room.room()),
        End::To => (from, Some(to)),
    };
    if new_from.map != map {
        return Err(refused);
    }
    if new_to.is_some_and(|to| to.map != map && to.place == SourceId::Private) {
        return Err("cloud-error-secret-link-into-other-map");
    }
    let ends: Vec<RoomId> = std::iter::once(new_from).chain(new_to).collect();
    let place = links::place_for(map, &ends, view.place).ok_or("mapper-link-two-secrets")?;
    let leaves_map = new_to.is_some_and(|to| to.map != map);
    // An exit with no destination given one is a new link made at last:
    // two-way within the map, as the panel makes new links.
    let explored = end == End::To && view.to.room == links::Destination::Nowhere;
    let reroute = |link: &mut NewLink| {
        link.style = std::mem::take(&mut link.style).rerouted();
        link.from = new_from;
        link.to = new_to;
        match end {
            End::From => link.from_anchor = None,
            End::To => link.to_anchor = None,
        }
        if explored {
            let arrives = link.forward.from_direction.opposite();
            link.forward.to_direction.get_or_insert(arrives);
        }
        if leaves_map {
            link.back = None;
        } else if explored && link.back.is_none() {
            link.back = Some(reverse_of(&link.forward));
        }
    };
    // The way back another map keeps no longer pairs with the moved link.
    let elsewhere = view
        .return_elsewhere
        .and_then(|back| remove_elsewhere(atlas, back));
    let follow = |command: Command| match elsewhere {
        Some((delete, restore)) => command.then(delete, restore),
        None => command,
    };
    if place == view.place {
        let command = rewrite(atlas, map, view, "Change link end", reroute).ok_or(refused)?;
        return Ok((follow(command), view.connection));
    }

    // Made again where it now goes, under new ids, before the old one goes.
    let area = atlas.get_area(&map).ok_or(refused)?;
    let (document, connection) = holding(&area, view).ok_or(refused)?;
    let forward = view.from.exit.as_ref().ok_or(refused)?;
    let id = ConnectionId::new();
    let fresh = |mut args: ExitArgs| {
        args.id = Some(ExitId::new());
        args
    };
    let mut link = NewLink {
        connection: id,
        style: Style::of(connection),
        from,
        from_anchor: None,
        to: view.to.room.room(),
        to_anchor: None,
        forward: fresh(exit_args(forward, id)),
        back: view.to.exit.as_ref().map(|exit| fresh(exit_args(exit, id))),
    };
    reroute(&mut link);
    let operations = link_operations(map, place, link).ok_or("mapper-link-two-secrets")?;
    let redo = vec![
        place_batch(map, place, operations, "Change link end"),
        document.batch(
            vec![AreaMutation::DeleteLink {
                connection_id: view.connection,
            }],
            "Change link end",
        ),
    ];
    let undo = vec![
        document.batch(
            commands::restore_link(document.content(), connection, false),
            "Undo change link end",
        ),
        place_batch(
            map,
            place,
            vec![AreaMutation::DeleteLink { connection_id: id }],
            "Undo change link end",
        ),
    ];
    Ok((follow(Command::new(redo, undo)), id))
}

/// Which exits of a link an edit changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Sides {
    One(End),
    Both,
}

/// Edits `field` of the exits `sides` of `view`'s link (the way back
/// another map keeps included) with `change`, as one command; consecutive
/// edits of the same field of the same sides make one undo step.
#[must_use]
pub fn edit_exits(
    atlas: &Arc<AtlasCache>,
    map: AreaId,
    view: &LinkView,
    sides: Sides,
    field: FieldId,
    change: impl Fn(&mut ExitUpdates),
) -> Option<Command> {
    let area = atlas.get_area(&map)?;
    let (document, _) = holding(&area, view)?;
    let ends: &[End] = match sides {
        Sides::One(End::From) => &[End::From],
        Sides::One(End::To) => &[End::To],
        Sides::Both => &[End::From, End::To],
    };
    let mut redo = Vec::new();
    let mut undo = Vec::new();
    let mut here_redo = Vec::new();
    let mut here_undo = Vec::new();
    let update = |exit: &ExitCache| {
        let prior = commands::exit_updates_from_cache(exit);
        let mut updates = prior.clone();
        change(&mut updates);
        (
            AreaMutation::UpdateExit {
                exit_id: exit.id,
                body: updates,
            },
            AreaMutation::UpdateExit {
                exit_id: exit.id,
                body: prior,
            },
        )
    };
    for end in ends {
        if let Some((exit, _)) = end_exit(view, *end) {
            let (forward, back) = update(exit);
            here_redo.push(forward);
            here_undo.push(back);
        } else if *end == End::To
            && let Some(back) = view.return_elsewhere
        {
            let other = atlas.get_area(&back.map)?;
            let far = Document::of(&other, back.place)?;
            let exit = far
                .content()
                .get_room_at(back.room)?
                .get_exits()
                .iter()
                .find(|exit| exit.id == back.exit)?;
            let (forward, back) = update(exit);
            redo.push(far.batch(vec![forward], "Update exit"));
            undo.push(far.batch(vec![back], "Undo update exit"));
        }
    }
    if !here_redo.is_empty() {
        redo.insert(0, document.batch(here_redo, "Update exit"));
        undo.insert(0, document.batch(here_undo, "Undo update exit"));
    }
    if redo.is_empty() {
        return None;
    }
    let detail = match sides {
        Sides::One(End::From) => "from",
        Sides::One(End::To) => "to",
        Sides::Both => "both",
    };
    Some(
        Command::new(redo, undo).coalescing(CoalesceKey::with_detail(
            EntityRef::Connection(map, view.connection),
            field,
            detail,
        )),
    )
}

#[cfg(test)]
mod tests {
    use super::super::commands::CommandStack;
    use super::super::links::fixture::*;
    use super::super::links::{Destination, Way, link_view, room_links};
    use super::*;
    use smudgy_cloud::Mapper;
    use smudgy_map_widget::map_editor::PlacedRoom;

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

    /// The exits of map `map`'s room `number`.
    fn exits(mapper: &Mapper, map: &str, number: i32) -> Vec<ExitCache> {
        mapper
            .get_current_atlas()
            .get_area(&area(map))
            .and_then(|area| area.get_room(&RoomNumber(number)).cloned())
            .map(|room| room.get_exits().to_vec())
            .unwrap_or_default()
    }

    fn view(mapper: &Mapper, id: &str, anchor: Option<i32>) -> LinkView {
        let atlas = mapper.get_current_atlas();
        let keep = atlas.get_area(&area(KEEP)).expect("loaded");
        link_view(
            &atlas,
            &keep,
            link(id),
            anchor.map(|number| smudgy_cloud::RoomAddress::map(RoomNumber(number))),
        )
        .expect("the link")
    }

    fn apply(mapper: &Mapper, command: Command) -> CommandStack {
        let mut stack = CommandStack::default();
        let _ = stack.push_and_apply(mapper, command);
        assert_eq!(stack.take_last_error(), None);
        assert!(stack.can_undo(), "the command applied");
        stack
    }

    fn undo(mapper: &Mapper, stack: &mut CommandStack) {
        let _ = stack.undo(mapper);
        assert_eq!(stack.take_last_error(), None);
        assert!(stack.can_redo(), "the undo applied");
    }

    fn rows(mapper: &Mapper, room: PlacedRoom) -> Vec<super::super::links::LinkRow> {
        let atlas = mapper.get_current_atlas();
        let keep = atlas.get_area(&area(KEEP)).expect("loaded");
        room_links(&atlas, &keep, room)
    }

    /// A link made from the panel runs both ways within the map, goes into
    /// the place at either end, and runs one way into another map; one
    /// joining two places is refused.
    #[tokio::test]
    async fn creating_a_link_from_the_panel() {
        let mapper = maps().await;
        let garden = PlacedRoom::map(RoomNumber(3));

        let (command, id) = create(
            area(KEEP),
            keep_room(3),
            ExitDirection::North,
            keep_room(5),
            SourceId::Map,
        )
        .expect("a link");
        let mut stack = apply(&mapper, command);
        let row = rows(&mapper, garden)
            .into_iter()
            .find(|row| row.connection == id)
            .expect("the new link");
        assert_eq!(
            (row.way, row.place, row.direction, row.back, row.far),
            (
                Way::Both,
                SourceId::Map,
                Some(ExitDirection::North),
                Some(ExitDirection::South),
                Destination::Room(keep_room(5))
            )
        );
        undo(&mapper, &mut stack);
        assert!(rows(&mapper, garden).iter().all(|row| row.connection != id));

        let (command, id) = create(
            area(KEEP),
            keep_room(3),
            ExitDirection::West,
            library(),
            SourceId::Map,
        )
        .expect("a link into the Secret");
        assert!(matches!(
            command.redo_mutations(),
            [Mutation::SourceBatch { source, .. }] if *source == secret()
        ));
        let _stack = apply(&mapper, command);
        let row = rows(&mapper, garden)
            .into_iter()
            .find(|row| row.connection == id)
            .expect("the Secret's link");
        assert_eq!(
            (row.place, row.far),
            (secret(), Destination::Room(library()))
        );

        let (command, id) = create(
            area(KEEP),
            keep_room(3),
            ExitDirection::Down,
            ossuary(),
            secret(),
        )
        .expect("a link into the Catacombs");
        let _stack = apply(&mapper, command);
        let row = rows(&mapper, garden)
            .into_iter()
            .find(|row| row.connection == id)
            .expect("the way down");
        assert_eq!(
            (row.way, row.place, row.far),
            (Way::Out, secret(), Destination::Room(ossuary())),
            "kept where new content goes, one way"
        );

        let private = RoomId {
            place: SourceId::Private,
            ..library()
        };
        assert_eq!(
            create(
                area(KEEP),
                library(),
                ExitDirection::North,
                private,
                SourceId::Map
            )
            .err(),
            Some("mapper-link-two-secrets")
        );
        assert_eq!(
            create(
                area(KEEP),
                keep_room(3),
                ExitDirection::Down,
                RoomId {
                    place: SourceId::Private,
                    ..ossuary()
                },
                SourceId::Map
            )
            .err(),
            Some("cloud-error-secret-link-into-other-map")
        );
    }

    /// A link may lead into a room of another map's Secret, named by its
    /// map and Secret; two-way, that Secret keeps the way back.
    #[tokio::test]
    async fn a_link_into_another_maps_secret_room() {
        let mapper = maps().await;
        let atlas = || mapper.get_current_atlas();
        let altar = RoomId {
            map: area(CATACOMBS),
            place: crypt(),
            number: RoomNumber(1),
        };
        let (command, id) = create(
            area(KEEP),
            keep_room(3),
            ExitDirection::Down,
            altar,
            SourceId::Map,
        )
        .expect("a link into the Crypt");
        let _stack = apply(&mapper, command);
        let row = rows(&mapper, PlacedRoom::map(RoomNumber(3)))
            .into_iter()
            .find(|row| row.connection == id)
            .expect("the way down");
        assert_eq!((row.way, row.far), (Way::Out, Destination::Room(altar)));
        let name = super::super::links::name(&atlas(), area(KEEP), altar);
        assert_eq!(
            (
                name.map_name.as_deref(),
                name.place.as_ref().map(|(place, _)| place.as_str()),
                name.title.as_str()
            ),
            (Some("Catacombs"), Some("Crypt"), "Bone Altar")
        );

        let keep = atlas().get_area(&area(KEEP)).expect("loaded");
        let down = link_view(&atlas(), &keep, id, None).expect("the way down");
        let _stack = apply(
            &mapper,
            two_way(&atlas(), area(KEEP), &down).expect("two way"),
        );
        let keep = atlas().get_area(&area(KEEP)).expect("loaded");
        let down = link_view(&atlas(), &keep, id, None).expect("the way down");
        assert!(down.two_way(), "the Crypt keeps the way back up");
        assert_eq!(
            down.return_elsewhere.map(|back| (back.map, back.place)),
            Some((area(CATACOMBS), crypt()))
        );
    }

    /// Removing a link takes both its exits, and the way back another map
    /// keeps once the link's delete is acknowledged; undo brings them back
    /// under their ids.
    #[tokio::test]
    async fn removing_a_link_takes_it_whole() {
        let mapper = maps().await;
        let gate = view(&mapper, C_GATE, None);
        let mut stack = apply(
            &mapper,
            remove(&mapper.get_current_atlas(), area(KEEP), &gate).expect("a remove"),
        );
        assert!(
            exits(&mapper, KEEP, 1)
                .iter()
                .all(|exit| exit.id != exit_id(E_GATE_N))
        );
        assert!(exits(&mapper, KEEP, 2).is_empty());
        undo(&mapper, &mut stack);
        assert!(
            exits(&mapper, KEEP, 1)
                .iter()
                .any(|exit| exit.id == exit_id(E_GATE_N))
        );
        assert_eq!(
            exits(&mapper, KEEP, 2)
                .iter()
                .map(|exit| (exit.id, exit.connection_id))
                .collect::<Vec<_>>(),
            vec![(exit_id(E_GATE_S), link(C_GATE))]
        );

        let down = view(&mapper, C_OSSUARY, None);
        let mut stack = apply(
            &mapper,
            remove(&mapper.get_current_atlas(), area(KEEP), &down).expect("a remove"),
        );
        assert!(
            exits(&mapper, KEEP, 1)
                .iter()
                .all(|exit| exit.id != exit_id(E_OSSUARY))
        );
        stack.settle(&mapper).await;
        assert!(
            exits(&mapper, CATACOMBS, 7).is_empty(),
            "the way back goes too"
        );
        undo(&mapper, &mut stack);
        stack.settle(&mapper).await;
        assert_eq!(exits(&mapper, CATACOMBS, 7).len(), 1);
        assert!(
            exits(&mapper, KEEP, 1)
                .iter()
                .any(|exit| exit.id == exit_id(E_OSSUARY))
        );
    }

    /// One-way takes the To end's exit (or the other map's way back);
    /// two-way adds a way back that arrives where the link leaves.
    #[tokio::test]
    async fn one_way_and_two_way() {
        let mapper = maps().await;
        let atlas = || mapper.get_current_atlas();
        let gate = view(&mapper, C_GATE, Some(1));
        let mut stack = apply(
            &mapper,
            one_way(&atlas(), area(KEEP), &gate).expect("one way"),
        );
        assert!(exits(&mapper, KEEP, 2).is_empty());
        assert!(!view(&mapper, C_GATE, Some(1)).two_way());
        undo(&mapper, &mut stack);
        assert!(view(&mapper, C_GATE, Some(1)).two_way());

        let down = view(&mapper, C_OSSUARY, None);
        let mut stack = apply(
            &mapper,
            one_way(&atlas(), area(KEEP), &down).expect("one way"),
        );
        assert!(exits(&mapper, CATACOMBS, 7).is_empty());
        undo(&mapper, &mut stack);
        assert_eq!(exits(&mapper, CATACOMBS, 7).len(), 1);

        let garden = view(&mapper, C_GARDEN, None);
        let mut stack = apply(
            &mapper,
            two_way(&atlas(), area(KEEP), &garden).expect("two way"),
        );
        let back = exits(&mapper, KEEP, 3);
        assert_eq!(
            back.iter()
                .map(|exit| (
                    exit.from_direction,
                    exit.to_room_number,
                    exit.to_direction,
                    exit.connection_id
                ))
                .collect::<Vec<_>>(),
            vec![(
                ExitDirection::West,
                Some(RoomNumber(1)),
                Some(ExitDirection::East),
                link(C_GARDEN)
            )]
        );
        assert!(view(&mapper, C_GARDEN, None).two_way());
        undo(&mapper, &mut stack);
        assert!(exits(&mapper, KEEP, 3).is_empty());

        // Into another map, the way back is that map's.
        let (command, id) = create(
            area(KEEP),
            keep_room(3),
            ExitDirection::Down,
            ossuary(),
            SourceId::Map,
        )
        .expect("a link");
        let _created = apply(&mapper, command);
        let keep = atlas().get_area(&area(KEEP)).expect("loaded");
        let down = link_view(&atlas(), &keep, id, None).expect("the way down");
        assert!(!down.two_way());
        let _stack = apply(
            &mapper,
            two_way(&atlas(), area(KEEP), &down).expect("two way"),
        );
        assert!(
            exits(&mapper, CATACOMBS, 7)
                .iter()
                .any(|exit| exit.to_area_id == Some(area(KEEP))
                    && exit.to_room_number == Some(RoomNumber(3))),
            "the Catacombs keep a way back up to the Garden"
        );
        let keep = atlas().get_area(&area(KEEP)).expect("loaded");
        assert!(
            link_view(&atlas(), &keep, id, None)
                .expect("the way down")
                .two_way()
        );
    }

    /// Swapping a one-way link makes its exit back first, then takes the
    /// old one; undo does the same the other way.
    #[tokio::test]
    async fn swapping_a_one_way_link_creates_before_it_deletes() {
        let mapper = maps().await;
        let garden = view(&mapper, C_GARDEN, None);
        let command = swap(&mapper.get_current_atlas(), area(KEEP), &garden).expect("a swap");
        assert!(matches!(
            command.redo_mutations(),
            [Mutation::AreaBatch { operations, .. }]
                if matches!(operations.as_slice(), [AreaMutation::CreateExit { .. }, AreaMutation::DeleteExit { .. }])
        ));
        assert!(matches!(
            command.undo_mutations(),
            [Mutation::AreaBatch { operations, .. }]
                if matches!(operations.as_slice(), [AreaMutation::CreateExit { .. }, AreaMutation::DeleteExit { .. }])
        ));
        let mut stack = apply(&mapper, command);
        assert!(
            exits(&mapper, KEEP, 1)
                .iter()
                .all(|exit| exit.id != exit_id(E_GARDEN))
        );
        let swapped = view(&mapper, C_GARDEN, None);
        assert_eq!(swapped.from.room, Destination::Room(keep_room(3)));
        assert_eq!(swapped.to.room, Destination::Room(keep_room(1)));
        assert_eq!(
            swapped.from.exit.as_ref().map(|exit| exit.from_direction),
            Some(ExitDirection::West)
        );
        undo(&mapper, &mut stack);
        let back = view(&mapper, C_GARDEN, None);
        assert_eq!(
            back.from.exit.as_ref().map(|exit| exit.id),
            Some(exit_id(E_GARDEN))
        );
        assert!(exits(&mapper, KEEP, 3).is_empty());
    }

    /// A two-way link's end turns to another direction and stays one
    /// link: the far exit arrives from the new direction.
    #[tokio::test]
    async fn changing_a_two_way_links_direction_keeps_it_one_link() {
        let mapper = maps().await;
        let gate = view(&mapper, C_GATE, Some(1));
        let mut stack = apply(
            &mapper,
            set_leaves(
                &mapper.get_current_atlas(),
                area(KEEP),
                &gate,
                End::From,
                ExitDirection::Northeast,
            )
            .expect("a direction change"),
        );
        let turned = view(&mapper, C_GATE, Some(1));
        assert!(turned.two_way());
        assert_eq!(
            turned
                .from
                .exit
                .as_ref()
                .map(|exit| (exit.id, exit.from_direction)),
            Some((exit_id(E_GATE_N), ExitDirection::Northeast))
        );
        assert_eq!(
            turned
                .to
                .exit
                .as_ref()
                .map(|exit| (exit.id, exit.to_direction)),
            Some((exit_id(E_GATE_S), Some(ExitDirection::Northeast)))
        );
        undo(&mapper, &mut stack);
        let back = view(&mapper, C_GATE, Some(1));
        assert_eq!(
            back.from.exit.as_ref().map(|exit| exit.from_direction),
            Some(ExitDirection::North)
        );
    }

    /// Moving the To end keeps the link and its exits' ids within its
    /// place; moving it to a Secret's room makes it again in that Secret.
    #[tokio::test]
    async fn retargeting_an_end() {
        let mapper = maps().await;
        let atlas = || mapper.get_current_atlas();
        let gate = view(&mapper, C_GATE, Some(1));
        let (command, id) =
            retarget(&atlas(), area(KEEP), &gate, End::To, keep_room(3)).expect("a retarget");
        assert_eq!(id, link(C_GATE));
        let mut stack = apply(&mapper, command);
        assert!(exits(&mapper, KEEP, 2).is_empty());
        let moved = view(&mapper, C_GATE, Some(1));
        assert_eq!(moved.to.room, Destination::Room(keep_room(3)));
        assert_eq!(
            moved.to.exit.as_ref().map(|exit| exit.id),
            Some(exit_id(E_GATE_S))
        );
        assert_eq!(
            moved.from.exit.as_ref().map(|exit| exit.id),
            Some(exit_id(E_GATE_N))
        );
        undo(&mapper, &mut stack);
        let back = view(&mapper, C_GATE, Some(1));
        assert_eq!(back.to.room, Destination::Room(keep_room(2)));
        assert_eq!(exits(&mapper, KEEP, 2).len(), 1);

        let garden = view(&mapper, C_GARDEN, None);
        let (command, id) =
            retarget(&atlas(), area(KEEP), &garden, End::To, library()).expect("a retarget");
        assert_ne!(id, link(C_GARDEN), "made again in the Secret");
        let mut stack = apply(&mapper, command);
        let keep = atlas().get_area(&area(KEEP)).expect("loaded");
        assert!(keep.get_connection(link(C_GARDEN)).is_none());
        let moved = link_view(&atlas(), &keep, id, None).expect("the new link");
        assert_eq!(moved.place, secret());
        assert_eq!(moved.to.room, Destination::Room(library()));
        undo(&mapper, &mut stack);
        let keep = atlas().get_area(&area(KEEP)).expect("loaded");
        assert!(keep.get_connection(link(C_GARDEN)).is_some());
        assert!(link_view(&atlas(), &keep, id, None).is_none());

        let private = RoomId {
            place: SourceId::Private,
            ..library()
        };
        let passage = view(&mapper, C_LIBRARY, None);
        assert_eq!(
            retarget(&atlas(), area(KEEP), &passage, End::From, private).err(),
            Some("mapper-link-two-secrets")
        );
    }

    /// One edit sets a field on both exits; consecutive edits of that field
    /// make one undo step.
    #[tokio::test]
    async fn editing_both_sides_is_one_step() {
        let mapper = maps().await;
        let gate = view(&mapper, C_GATE, Some(1));
        let hide = |hidden: bool| {
            edit_exits(
                &mapper.get_current_atlas(),
                area(KEEP),
                &gate,
                Sides::Both,
                FieldId::Flags,
                move |updates| {
                    updates.is_hidden = Some(hidden);
                },
            )
            .expect("an edit")
        };
        let mut stack = apply(&mapper, hide(true));
        let _ = stack.push_and_apply(&mapper, hide(true));
        assert!(
            exits(&mapper, KEEP, 1)
                .iter()
                .find(|exit| exit.id == exit_id(E_GATE_N))
                .is_some_and(|exit| exit.is_hidden)
        );
        assert!(exits(&mapper, KEEP, 2)[0].is_hidden);
        undo(&mapper, &mut stack);
        assert!(!exits(&mapper, KEEP, 2)[0].is_hidden);
        assert!(!stack.can_undo(), "one step");

        let weight = edit_exits(
            &mapper.get_current_atlas(),
            area(KEEP),
            &gate,
            Sides::One(End::To),
            FieldId::Weight,
            |updates| {
                updates.weight = Some(4.0);
            },
        )
        .expect("an edit");
        let _stack = apply(&mapper, weight);
        assert!((exits(&mapper, KEEP, 2)[0].weight - 4.0).abs() < f32::EPSILON);
        assert!(
            exits(&mapper, KEEP, 1)
                .iter()
                .find(|exit| exit.id == exit_id(E_GATE_N))
                .is_some_and(|exit| (exit.weight - 1.0).abs() < f32::EPSILON)
        );
    }
}
