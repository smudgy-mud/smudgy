//! The pure applier behind `mapper.mergeAreas`: folds whole area documents
//! into one destination document, retargets every exit that named a moved
//! room, and reports the room remap and the resulting versions.
//!
//! It is a function over documents, modeled on the same-area room merge in
//! [`crate::mapper`]: every tier that owns full documents (local on disk,
//! ephemeral in memory, a future server endpoint) applies this one
//! implementation under its own locking and persistence, so the numbering,
//! retargeting and pairing rules have exactly one definition and one test
//! surface. Nothing here knows about the mapper cache, fences, or why a
//! caller wants several areas to become one.
//!
//! The applier mutates every document in place: the destination and the
//! third parties gain or lose exits, a partial source keeps its remaining
//! rooms and is written back, and a whole source is drained (its content
//! moves rather than copies) and then deleted by the caller, so what it
//! holds afterwards is unspecified. Any error leaves every document in an
//! unspecified partially-applied state; the caller discards them, exactly
//! as the CAS appliers in [`super::area_edits`] expect.

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::area_edits;
use crate::{
    AreaId, AreaWithDetails, CloudError, CloudResult, Connection, ConnectionId, ExitId, Label,
    RoomNumber, RoomWithDetails, Shape,
    connection_lifecycle::{self, ExitTopology},
    mapper::RoomKey,
    mutation::{ResourceKind, VersionInfo},
};

/// The rigid offset applied to everything in one source before it lands in
/// the destination: rooms, labels, shapes and stored route points. Omitted
/// axes are zero.
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
pub struct Translate {
    pub x: f32,
    pub y: f32,
    pub level: i32,
}

/// One area to fold into the destination: its offset and, for a partial
/// merge, the rooms to take.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaMergeSource {
    pub id: AreaId,
    #[serde(default)]
    pub translate: Translate,
    /// The rooms to move, when only some should. `None` moves everything
    /// and deletes the source. `Some` moves exactly the listed rooms, with
    /// their exits, properties and tags and the connections among them,
    /// and keeps the source as a written document with its labels and
    /// shapes, even when no room remains; an empty list is refused.
    #[serde(default)]
    pub rooms: Option<Vec<RoomNumber>>,
}

impl AreaMergeSource {
    /// Whether this source keeps its document because only some of its
    /// rooms move.
    #[must_use]
    pub fn is_partial(&self) -> bool {
        self.rooms.is_some()
    }
}

/// Everything the applier needs, decided by the mapper on its live cache
/// and verified by the backend against the authoritative documents. The
/// plan carries inputs only; the applier computes the outcome, so the
/// mapper never guesses at numbering or retargeting.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaMergePlan {
    /// The area that receives everything.
    pub into: AreaId,
    /// The areas that give up rooms, in the caller's order. Order matters
    /// twice: room numbers are handed out source by source, and when two
    /// sources hold the two halves of one link, the earlier source's half
    /// is kept. A whole source disappears; a partial one stays.
    pub sources: Vec<AreaMergeSource>,
    /// Third-party areas holding at least one exit into a source. Their
    /// exits follow the moved rooms; nothing else about them changes.
    pub inbound: Vec<AreaId>,
    /// Every touched area with the revision the plan was built against.
    /// A document whose revision differs is a [`CloudError::RevisionConflict`]
    /// and nothing is applied.
    pub expected: Vec<(AreaId, i64)>,
    /// The destination's allocation floor at plan time: the first number a
    /// renumbered room may receive. It sits at or above the destination's
    /// highest room plus one and covers the destination's reserved numbers
    /// (open drafts), so a draft committed after the merge cannot collide
    /// with a moved room. The numbers strictly between the destination's
    /// highest room and the floor are the reserved band: a source room
    /// never keeps one of those. It keeps a free number at or below the
    /// destination's highest room (a gap) or at or above the floor.
    pub number_floor: RoomNumber,
}

impl AreaMergePlan {
    /// Every area the plan names, each once, in first-mention order: the
    /// destination, the sources, the inbound third parties, then anything
    /// `expected` lists beyond those. This is the set a backend must own in
    /// one tier and a cache must drop after a commit.
    #[must_use]
    pub fn touched_areas(&self) -> Vec<AreaId> {
        let mut seen = HashSet::new();
        std::iter::once(self.into)
            .chain(self.sources.iter().map(|source| source.id))
            .chain(self.inbound.iter().copied())
            .chain(self.expected.iter().map(|(id, _)| *id))
            .filter(|id| seen.insert(*id))
            .collect()
    }

    /// The areas the merge deletes: every whole source, in plan order. A
    /// partial source is written instead and is not listed here.
    #[must_use]
    pub fn deleted_areas(&self) -> Vec<AreaId> {
        self.sources
            .iter()
            .filter(|source| !source.is_partial())
            .map(|source| source.id)
            .collect()
    }
}

/// Where one moved room was and the number it has now in the destination.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RoomRemap {
    pub from: RoomKey,
    pub to: RoomNumber,
}

/// The applier's result: every moved room, and the post-merge version of
/// every written document. The destination, each partial source and each
/// changed third party report `deleted: false` with their new revision;
/// each whole source reports `deleted: true` with its pre-merge revision as
/// a tombstone. A third party none of whose exits named a moved room is
/// left untouched and does not appear.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AreaMergeOutcome {
    pub rooms: Vec<RoomRemap>,
    pub versions: Vec<VersionInfo>,
}

/// What a backend hands back after committing a merge: the applier's
/// outcome plus the post-merge documents the mapper republishes without a
/// second read (the destination first, then each partial source in plan
/// order, then each changed third party, in `versions` order).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AreaMergeCommit {
    pub outcome: AreaMergeOutcome,
    pub documents: Vec<AreaWithDetails>,
}

/// Folds the sources into `into`: everything from a whole source, the
/// listed rooms from a partial one; retargets exits in every document; and
/// leaves whole sources for the caller to delete and partial sources
/// written.
///
/// Rooms are renumbered per the plan's floor, translated per source, and
/// copied field for field. Exits keep their ids and connection ids; those
/// naming a moved room are rewritten to its new address, in the destination,
/// in the sources and in the third parties alike. Exits with no destination
/// or a redacted one are carried unchanged, as is an exit naming a source
/// area without a room that exists there: it dangles toward the deleted area
/// the way it would after an ordinary delete. Source connections move with
/// endpoints renumbered and stored routes translated; where the two halves
/// of a former cross-area link now both live in the destination they are
/// paired with the `Pair` rules so the link renders once. Source area
/// properties are dropped; the destination's are untouched.
///
/// A partial source is, for its remaining rooms, a third party: their exits
/// into moved rooms follow them to the destination, exits from moved rooms
/// back into remaining rooms keep naming the source, and a paired
/// connection whose two exits end up on different sides is split into the
/// two one-member halves a cross-area link has always been made of (see
/// [`split_straddling_connections`]). Its labels, shapes and area
/// properties stay where they are.
///
/// The same inputs always produce the same outcome: nothing here consults a
/// clock, draws an identity, or iterates a hash map into the result.
///
/// # Errors
///
/// - [`CloudError::StructuralConflict`] `merge_areas_no_sources` when the
///   plan names no source, `merge_areas_same_area` when a source is the
///   destination or a source repeats, `merge_areas_no_rooms` when a partial
///   source lists no room, and `merge_areas_room_not_found` when it lists a
///   room its document does not hold.
/// - [`CloudError::AreaNotFound`] when a plan id (destination, source,
///   inbound or expected) has no matching document.
/// - [`CloudError::InvalidInput`] when a document is passed that the plan
///   does not name in that role, or `merge_areas_invalid_translation` when
///   an offset or translated coordinate is nonfinite or a level overflows.
///   Translation refusals leave every document unchanged.
/// - [`CloudError::StructuralConflict`] `merge_areas_room_numbers_exhausted`
///   when a colliding room cannot be allocated a representable number.
///   Numbering refusals leave every document unchanged.
/// - [`CloudError::RevisionConflict`] when a document's revision differs from
///   the plan's expectation.
/// - [`CloudError::InvalidConnection`] when the merged destination or a
///   partial source fails whole-graph validation.
pub fn apply_area_merge(
    plan: &AreaMergePlan,
    into: &mut AreaWithDetails,
    sources: &mut [AreaWithDetails],
    inbound: &mut [AreaWithDetails],
) -> CloudResult<AreaMergeOutcome> {
    validate_plan(plan)?;
    let order = source_order(plan, sources)?;
    check_documents(plan, into, sources, inbound)?;
    check_listed_rooms(plan, sources, &order)?;
    let tombstones: Vec<VersionInfo> = plan
        .sources
        .iter()
        .zip(&order)
        .filter(|(source, _)| !source.is_partial())
        .map(|(source, index)| version(source.id, sources[*index].area.rev, true))
        .collect();

    let remap = allocate_room_numbers(plan, into, sources, &order)?;
    check_translations(plan, sources, &order, &remap.by_key)?;
    let moved = move_content(plan, into, sources, &order, &remap.by_key)?;
    retarget_exits(into, plan.into, &remap.by_key);
    reattach_changed_exits(into, &moved.before);
    pair_cross_area_halves(into, &moved.origin_rank)?;
    area_edits::validate_connection_graph(into)?;
    into.area.rev += 1;

    let mut versions = vec![version(into.area.id, into.area.rev, false)];
    for (source, index) in plan.sources.iter().zip(&order) {
        if !source.is_partial() {
            continue;
        }
        // For its remaining rooms a partial source is a third party with
        // one difference: an exit that used to stay inside it may now leave
        // it, so its connections are re-anchored the way the destination's
        // are, not merely retargeted.
        let document = &mut sources[*index];
        retarget_exits(document, plan.into, &remap.by_key);
        reattach_changed_exits(document, &moved.before);
        area_edits::validate_connection_graph(document)?;
        document.area.rev += 1;
        versions.push(version(document.area.id, document.area.rev, false));
    }
    for id in &plan.inbound {
        let document = inbound
            .iter_mut()
            .find(|document| document.area.id == *id)
            .ok_or(CloudError::AreaNotFound(*id))?;
        if retarget_exits(document, plan.into, &remap.by_key) {
            document.area.rev += 1;
            versions.push(version(document.area.id, document.area.rev, false));
        }
    }
    versions.extend(tombstones);

    Ok(AreaMergeOutcome {
        rooms: remap.rooms,
        versions,
    })
}

fn version(id: AreaId, rev: i64, deleted: bool) -> VersionInfo {
    VersionInfo {
        resource: ResourceKind::Area,
        id: id.0,
        rev,
        deleted,
    }
}

fn structural_conflict(code: &str) -> CloudError {
    CloudError::StructuralConflict(code.to_string())
}

fn validate_plan(plan: &AreaMergePlan) -> CloudResult<()> {
    if plan.sources.is_empty() {
        return Err(structural_conflict("merge_areas_no_sources"));
    }
    let mut seen = HashSet::with_capacity(plan.sources.len());
    for source in &plan.sources {
        if source.id == plan.into || !seen.insert(source.id) {
            return Err(structural_conflict("merge_areas_same_area"));
        }
    }
    if plan
        .sources
        .iter()
        .any(|source| source.rooms.as_ref().is_some_and(Vec::is_empty))
    {
        return Err(structural_conflict("merge_areas_no_rooms"));
    }
    Ok(())
}

/// Where each plan source sits in the slice of documents, in plan order,
/// so numbering and pairing follow the caller's order rather than the
/// order the documents were loaded in.
fn source_order(plan: &AreaMergePlan, sources: &[AreaWithDetails]) -> CloudResult<Vec<usize>> {
    let mut by_id: HashMap<AreaId, usize> = HashMap::with_capacity(sources.len());
    for (index, source) in sources.iter().enumerate() {
        if by_id.insert(source.area.id, index).is_some() {
            return Err(CloudError::InvalidInput(
                "a source document was passed twice".to_string(),
            ));
        }
    }
    let order = plan
        .sources
        .iter()
        .map(|source| {
            by_id
                .remove(&source.id)
                .ok_or(CloudError::AreaNotFound(source.id))
        })
        .collect::<CloudResult<Vec<_>>>()?;
    if let Some(stray) = by_id.keys().next() {
        return Err(CloudError::InvalidInput(format!(
            "area {stray} is not a source of the merge plan"
        )));
    }
    Ok(order)
}

/// Every room a partial source lists must exist in its document. Checked
/// before anything moves, so a refused merge has touched nothing.
fn check_listed_rooms(
    plan: &AreaMergePlan,
    sources: &[AreaWithDetails],
    order: &[usize],
) -> CloudResult<()> {
    for (source, index) in plan.sources.iter().zip(order) {
        let Some(listed) = &source.rooms else {
            continue;
        };
        let document = &sources[*index];
        let missing = listed.iter().any(|number| {
            !document
                .rooms
                .iter()
                .any(|room| room.room_number == *number)
        });
        if missing {
            return Err(structural_conflict("merge_areas_room_not_found"));
        }
    }
    Ok(())
}

/// Every document must be the one the plan names in its role, and every
/// expected revision must hold. Checked before anything moves, so a refused
/// merge has touched nothing.
fn check_documents(
    plan: &AreaMergePlan,
    into: &AreaWithDetails,
    sources: &[AreaWithDetails],
    inbound: &[AreaWithDetails],
) -> CloudResult<()> {
    if into.area.id != plan.into {
        return Err(CloudError::AreaNotFound(plan.into));
    }
    let mut inbound_ids: HashSet<AreaId> = HashSet::with_capacity(inbound.len());
    for document in inbound {
        let id = document.area.id;
        let planned_elsewhere =
            id == plan.into || plan.sources.iter().any(|source| source.id == id);
        if planned_elsewhere || !plan.inbound.contains(&id) || !inbound_ids.insert(id) {
            return Err(CloudError::InvalidInput(format!(
                "area {id} is not an inbound third party of the merge plan"
            )));
        }
    }
    for id in &plan.inbound {
        if !inbound_ids.contains(id) {
            return Err(CloudError::AreaNotFound(*id));
        }
    }

    let current_rev = |id: AreaId| -> Option<i64> {
        std::iter::once(into)
            .chain(sources)
            .chain(inbound)
            .find(|document| document.area.id == id)
            .map(|document| document.area.rev)
    };
    for (id, expected_rev) in &plan.expected {
        let current_rev = current_rev(*id).ok_or(CloudError::AreaNotFound(*id))?;
        if current_rev != *expected_rev {
            return Err(CloudError::RevisionConflict {
                id: id.0,
                expected_rev: *expected_rev,
                current_rev,
            });
        }
    }
    Ok(())
}

/// The room numbering decision, in the order it is reported: sources in
/// plan order, rooms by ascending number.
struct Remap {
    rooms: Vec<RoomRemap>,
    by_key: HashMap<RoomKey, RoomNumber>,
}

/// Hands every moving room its destination number: each room of a whole
/// source, the listed rooms of a partial one. A room keeps its number
/// when nothing in the destination (and nothing already handed out) holds
/// it and it is not in the reserved band, the numbers strictly above the
/// destination's highest room and strictly below the plan's floor, which
/// open drafts on the destination may hold; otherwise it takes the lowest
/// free number at or above the cursor, which starts at
/// `max(floor, destination max + 1)` and only rises. A free gap at or below
/// the destination's highest room is kept, never allocated; a number at or
/// above the floor is kept too, since no draft can hold it.
/// Reserve all free original numbers first, in source order, so reallocating
/// one collision cannot displace a later room with a free original number.
fn allocate_room_numbers(
    plan: &AreaMergePlan,
    into: &AreaWithDetails,
    sources: &[AreaWithDetails],
    order: &[usize],
) -> CloudResult<Remap> {
    let mut used: HashSet<RoomNumber> = into.rooms.iter().map(|room| room.room_number).collect();
    let destination_max = into
        .rooms
        .iter()
        .map(|room| room.room_number)
        .max()
        .unwrap_or(RoomNumber(0));
    let reserved = |number: RoomNumber| number > destination_max && number < plan.number_floor;
    // Keep an exhausted cursor representable without rejecting rooms that
    // can still retain their original number, including gaps below the max.
    let mut cursor = (i64::from(destination_max.0) + 1).max(i64::from(plan.number_floor.0));
    let mut selected = Vec::new();
    for (source, index) in plan.sources.iter().zip(order) {
        let document = &sources[*index];
        let mut numbers: Vec<RoomNumber> = match &source.rooms {
            Some(listed) => listed.clone(),
            None => document.rooms.iter().map(|room| room.room_number).collect(),
        };
        numbers.sort_unstable();
        numbers.dedup();
        for from in numbers {
            let kept = !reserved(from) && used.insert(from);
            selected.push((RoomKey::new(document.area.id, from), kept));
        }
    }
    let mut rooms = Vec::with_capacity(selected.len());
    let mut by_key = HashMap::new();
    for (key, kept) in selected {
        let to = if kept {
            key.room_number
        } else {
            loop {
                let allocated = RoomNumber(
                    i32::try_from(cursor)
                        .map_err(|_| structural_conflict("merge_areas_room_numbers_exhausted"))?,
                );
                cursor += 1;
                if used.insert(allocated) {
                    break allocated;
                }
            }
        };
        by_key.insert(key.clone(), to);
        rooms.push(RoomRemap { from: key, to });
    }
    Ok(Remap { rooms, by_key })
}

/// Refuses unsafe arithmetic before any source is drained or connection is
/// split. Partial sources only translate their selected rooms and the
/// connections with a moving member; labels and shapes stay untouched.
fn check_translations(
    plan: &AreaMergePlan,
    sources: &[AreaWithDetails],
    order: &[usize],
    remap: &HashMap<RoomKey, RoomNumber>,
) -> CloudResult<()> {
    for (source, index) in plan.sources.iter().zip(order) {
        let translate = source.translate;
        if !translate.x.is_finite() || !translate.y.is_finite() {
            return Err(invalid_translation());
        }
        let document = &sources[*index];
        let mut moving_connections = HashSet::new();
        for room in &document.rooms {
            if !remap.contains_key(&RoomKey::new(source.id, room.room_number)) {
                continue;
            }
            check_translated_point(room.x, room.y, translate)?;
            translated_level(room.level, translate.level)?;
            moving_connections.extend(room.exits.iter().map(|exit| exit.connection_id));
        }
        for connection in &document.connections {
            if source.is_partial() && !moving_connections.contains(&connection.id) {
                continue;
            }
            for point in &connection.route_points {
                check_translated_point(point.x, point.y, translate)?;
            }
        }
        if !source.is_partial() {
            for label in &document.labels {
                check_translated_point(label.x, label.y, translate)?;
                translated_level(label.level, translate.level)?;
            }
            for shape in &document.shapes {
                check_translated_point(shape.x, shape.y, translate)?;
                translated_level(shape.level, translate.level)?;
            }
        }
    }
    Ok(())
}

fn invalid_translation() -> CloudError {
    CloudError::InvalidInput("merge_areas_invalid_translation".to_string())
}

fn check_translated_point(x: f32, y: f32, translate: Translate) -> CloudResult<()> {
    if !(x + translate.x).is_finite() || !(y + translate.y).is_finite() {
        return Err(invalid_translation());
    }
    Ok(())
}

fn translated_level(level: i32, offset: i32) -> CloudResult<i32> {
    level.checked_add(offset).ok_or_else(invalid_translation)
}

/// What the connection passes need to know about where the destination's
/// content came from.
struct MovedContent {
    /// Every exit's topology as it stood in its origin document, keyed by
    /// exit id. Compared with the post-retarget topology to decide which
    /// exits need their connection re-anchored.
    before: HashMap<ExitId, ExitTopology>,
    /// Which document each connection came from: `0` for the destination,
    /// `i + 1` for the `i`-th source. Decides the kept half when pairing.
    origin_rank: HashMap<ConnectionId, usize>,
}

/// Moves the rooms, connections, labels and shapes that leave each source
/// into the destination. Every moved connection's stored route receives the
/// same rigid offset as its rooms. A straddling connection is re-anchored
/// later, which clears the route when it becomes external.
///
/// Every exit's pre-move topology is captured here, in the sources as well
/// as the destination, for the moved exits and the ones a partial source
/// keeps alike: both sides of a split are re-anchored against it later.
///
/// # Errors
///
/// Splitting boundary connections uses the shared `Unlink` implementation,
/// whose refusals propagate.
fn move_content(
    plan: &AreaMergePlan,
    into: &mut AreaWithDetails,
    sources: &mut [AreaWithDetails],
    order: &[usize],
    remap: &HashMap<RoomKey, RoomNumber>,
) -> CloudResult<MovedContent> {
    let into_id = into.area.id;
    let mut before: HashMap<ExitId, ExitTopology> = HashMap::new();
    let mut origin_rank: HashMap<ConnectionId, usize> = HashMap::new();
    for room in &into.rooms {
        for exit in &room.exits {
            before.insert(
                exit.id,
                area_edits::exit_topology(into_id, room.room_number, exit),
            );
        }
    }
    for connection in &into.connections {
        origin_rank.insert(connection.id, 0);
    }

    for (index, (plan_source, slot)) in plan.sources.iter().zip(order).enumerate() {
        let source = &mut sources[*slot];
        let rank = index + 1;
        let translate = plan_source.translate;
        let source_id = source.area.id;
        let moving: HashSet<RoomNumber> = remap
            .keys()
            .filter(|key| key.area_id == source_id)
            .map(|key| key.room_number)
            .collect();
        if plan_source.is_partial() {
            split_straddling_connections(source, &moving)?;
        }
        for room in &source.rooms {
            for exit in &room.exits {
                before.insert(
                    exit.id,
                    area_edits::exit_topology(source_id, room.room_number, exit),
                );
            }
        }
        let taken = take_moving_content(source, &moving, plan_source.is_partial());
        let renumber = |number: RoomNumber| {
            remap
                .get(&RoomKey::new(source_id, number))
                .copied()
                .unwrap_or(number)
        };

        let mut rooms = taken.rooms;
        rooms.sort_by_key(|room| room.room_number);
        for mut room in rooms {
            room.room_number = renumber(room.room_number);
            room.x += translate.x;
            room.y += translate.y;
            room.level = translated_level(room.level, translate.level)?;
            into.rooms.push(room);
        }
        for mut connection in taken.connections {
            connection.endpoint_a.room_number = renumber(connection.endpoint_a.room_number);
            if let Some(endpoint_b) = connection.endpoint_b.as_mut() {
                endpoint_b.room_number = renumber(endpoint_b.room_number);
            }
            for point in &mut connection.route_points {
                point.x += translate.x;
                point.y += translate.y;
            }
            origin_rank.insert(connection.id, rank);
            into.connections.push(connection);
        }
        for mut label in taken.labels {
            label.x += translate.x;
            label.y += translate.y;
            label.level = translated_level(label.level, translate.level)?;
            into.labels.push(label);
        }
        for mut shape in taken.shapes {
            shape.x += translate.x;
            shape.y += translate.y;
            shape.level = translated_level(shape.level, translate.level)?;
            into.shapes.push(shape);
        }
    }

    Ok(MovedContent {
        before,
        origin_rank,
    })
}

/// What leaves one source.
struct Taken {
    rooms: Vec<RoomWithDetails>,
    connections: Vec<Connection>,
    labels: Vec<Label>,
    shapes: Vec<Shape>,
}

/// Takes what moves out of a source: everything from a whole source; from
/// a partial one the moving rooms and the connections all of whose member
/// exits sit on them, so a connection follows its exits, while the labels
/// and shapes stay. After [`split_straddling_connections`] no connection
/// has members on both sides, so every connection has exactly one place to
/// be.
fn take_moving_content(
    source: &mut AreaWithDetails,
    moving: &HashSet<RoomNumber>,
    partial: bool,
) -> Taken {
    if !partial {
        return Taken {
            rooms: std::mem::take(&mut source.rooms),
            connections: std::mem::take(&mut source.connections),
            labels: std::mem::take(&mut source.labels),
            shapes: std::mem::take(&mut source.shapes),
        };
    }
    // Per connection: whether any member exit sits on a moving room, and
    // whether any sits on a staying one.
    let mut sides: HashMap<ConnectionId, (bool, bool)> = HashMap::new();
    for room in &source.rooms {
        let moves = moving.contains(&room.room_number);
        for exit in &room.exits {
            let side = sides.entry(exit.connection_id).or_default();
            if moves {
                side.0 = true;
            } else {
                side.1 = true;
            }
        }
    }
    let (rooms, staying_rooms): (Vec<_>, Vec<_>) = std::mem::take(&mut source.rooms)
        .into_iter()
        .partition(|room| moving.contains(&room.room_number));
    source.rooms = staying_rooms;
    let (connections, staying_connections): (Vec<_>, Vec<_>) =
        std::mem::take(&mut source.connections)
            .into_iter()
            .partition(|connection| matches!(sides.get(&connection.id), Some((true, false))));
    source.connections = staying_connections;
    Taken {
        rooms,
        connections,
        labels: Vec::new(),
        shapes: Vec::new(),
    }
}

/// Splits every paired connection of a partial source whose two member
/// exits straddle the moved set, through the shared batch `Unlink`
/// implementation: the member on the moving room takes a fresh one-member connection, the half that
/// lands in the destination, and the member on the staying room keeps the
/// original. Each half is then re-anchored as a cross-area link once its
/// exit is moved or retargeted, which is the representation a link between
/// two areas has always had: two one-member `External` connections that
/// name each other's room.
///
/// The fresh ids come from [`split_connection_id`], not from a random
/// draw, and the splits are applied in exit-id order, so the same inputs
/// always produce the same document.
fn split_straddling_connections(
    source: &mut AreaWithDetails,
    moving: &HashSet<RoomNumber>,
) -> CloudResult<()> {
    let mut on_moving: HashMap<ConnectionId, Vec<ExitId>> = HashMap::new();
    let mut on_staying: HashSet<ConnectionId> = HashSet::new();
    for room in &source.rooms {
        for exit in &room.exits {
            if moving.contains(&room.room_number) {
                on_moving
                    .entry(exit.connection_id)
                    .or_default()
                    .push(exit.id);
            } else {
                on_staying.insert(exit.connection_id);
            }
        }
    }
    let mut splits: Vec<(ExitId, ConnectionId)> = on_moving
        .into_iter()
        .filter(|(connection_id, _)| on_staying.contains(connection_id))
        .flat_map(|(connection_id, exits)| {
            exits
                .into_iter()
                .map(move |exit_id| (exit_id, connection_id))
        })
        .collect();
    splits.sort_unstable_by_key(|(exit_id, _)| exit_id.0);
    let splits: Vec<_> = splits
        .into_iter()
        .map(|(exit_id, connection_id)| {
            (
                exit_id,
                split_connection_id(source.area.id, connection_id, exit_id),
            )
        })
        .collect();
    area_edits::unlink_exits(source, &splits)
}

/// The id of the connection minted when a partial merge splits a paired
/// connection: SHA-256 over a fixed tag, the source area id, the connection
/// being split and the exit that leaves it, folded into a UUID with the
/// version and variant bits of a custom (v8) UUID. Derived rather than
/// drawn so the applier stays a pure function of its inputs; the same split
/// applied twice mints the same id, and a split of a different connection or
/// exit never collides with it.
fn split_connection_id(source: AreaId, connection: ConnectionId, exit: ExitId) -> ConnectionId {
    let mut hasher = Sha256::new();
    hasher.update(b"smudgy:merge-areas:split-connection");
    hasher.update(source.0.as_bytes());
    hasher.update(connection.0.as_bytes());
    hasher.update(exit.0.as_bytes());
    let digest = hasher.finalize();
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    ConnectionId(uuid::Builder::from_custom_bytes(bytes).into_uuid())
}

/// Rewrites every exit that names a moved room to its new address. Returns
/// whether anything changed, which is what decides whether a third party is
/// written at all.
fn retarget_exits(
    document: &mut AreaWithDetails,
    into: AreaId,
    remap: &HashMap<RoomKey, RoomNumber>,
) -> bool {
    let mut changed = false;
    for room in &mut document.rooms {
        for exit in &mut room.exits {
            let (Some(area), Some(number)) = (exit.to_area_id, exit.to_room_number) else {
                continue;
            };
            if let Some(to) = remap.get(&RoomKey::new(area, number)) {
                exit.to_area_id = Some(into);
                exit.to_room_number = Some(*to);
                changed = true;
            }
        }
    }
    changed
}

/// Re-anchors the connection of every destination exit whose topology
/// changed: a former cross-area exit now lands inside the destination and
/// needs a second endpoint and an in-area kind. This is the local mirror of
/// the `UpdateExit` applier's §3.2 step, run once per changed exit after
/// every exit has been retargeted, so a pair renumbered together still
/// reads as reciprocal and stays paired.
fn reattach_changed_exits(into: &mut AreaWithDetails, before: &HashMap<ExitId, ExitTopology>) {
    let mut members: HashMap<ConnectionId, Vec<ExitTopology>> = HashMap::new();
    for exit in area_edits::exit_topologies(into, None) {
        members.entry(exit.connection_id).or_default().push(exit);
    }
    let connections: HashSet<_> = into.connections.iter().map(|c| c.id).collect();
    // Moving together cannot break a valid pair; boundary pairs were already
    // split. Preserve the ordinary editor's repair behavior for malformed
    // input, where reattachment can change membership while traversing it.
    if members.iter().any(|(id, exits)| {
        !connections.contains(id)
            || match exits.as_slice() {
                [_] => false,
                [a, b] => !area_edits::members_are_reciprocal(a, b),
                _ => true,
            }
    }) {
        repair_changed_exits(into, before);
        return;
    }
    let sites: HashMap<_, _> = into
        .rooms
        .iter()
        .map(|room| {
            (
                room.room_number,
                connection_lifecycle::RoomSite {
                    x: room.x,
                    y: room.y,
                    level: room.level,
                },
            )
        })
        .collect();
    for connection in &mut into.connections {
        let Some(exits) = members.get(&connection.id) else {
            continue;
        };
        let [after] = exits.as_slice() else { continue };
        if before
            .get(&after.id)
            .is_some_and(|before| connection_lifecycle::topology_differs(before, after))
        {
            connection_lifecycle::retarget_in_place(connection, after, &|number| {
                sites.get(&number).copied()
            });
        }
    }
}

/// Slow compatibility path for the editor's repair of malformed membership.
fn repair_changed_exits(into: &mut AreaWithDetails, before: &HashMap<ExitId, ExitTopology>) {
    for after in area_edits::exit_topologies(into, None) {
        let Some(before) = before.get(&after.id) else {
            continue;
        };
        if !connection_lifecycle::topology_differs(before, &after) {
            continue;
        }
        let peers = area_edits::exit_topologies(into, Some(after.id));
        let mut connections = std::mem::take(&mut into.connections);
        let connection_id = connection_lifecycle::reattach_after_update(
            before,
            &after,
            &peers,
            &mut connections,
            area_edits::room_site(into),
        );
        into.connections = connections;
        let exit = into
            .rooms
            .iter_mut()
            .flat_map(|room| room.exits.iter_mut())
            .find(|exit| exit.id == after.id)
            .expect("topologies were projected from stored exits");
        exit.connection_id = connection_id;
    }
}

/// Pairs the two halves of every former cross-area link: two one-member
/// connections from different origin documents whose exits are each
/// other's unique reciprocal. The destination's own half is kept when one
/// side already lived there, else the half from the earlier source in plan
/// order. Two reciprocal one-member connections from the same document were
/// never a cross-area link and stay as their author left them; an
/// ambiguous reciprocal (several candidates) stays one-way, as exit
/// creation would leave it.
fn pair_cross_area_halves(
    into: &mut AreaWithDetails,
    origin_rank: &HashMap<ConnectionId, usize>,
) -> CloudResult<()> {
    let topologies = area_edits::exit_topologies(into, None);
    let mut member_count: HashMap<ConnectionId, usize> = HashMap::new();
    for topology in &topologies {
        *member_count.entry(topology.connection_id).or_default() += 1;
    }
    let halves: Vec<&ExitTopology> = topologies
        .iter()
        .filter(|topology| member_count[&topology.connection_id] == 1)
        .collect();
    let rank = |topology: &ExitTopology| {
        origin_rank
            .get(&topology.connection_id)
            .copied()
            .unwrap_or(0)
    };

    // Reciprocal compatibility is symmetric. A mutually unique pair cannot
    // be a candidate for any third half, so consuming it cannot disambiguate
    // another pair. Candidate counts can therefore stay immutable throughout.
    let mut candidates = ReciprocalCandidates::default();
    for (index, half) in halves.iter().enumerate() {
        candidates.insert(half, rank(half), index);
    }
    let mut replacements = HashMap::new();
    let connection_ids: HashSet<_> = into.connections.iter().map(|c| c.id).collect();
    for (index, half) in halves.iter().enumerate() {
        let Some(other_index) = candidates.unique(half, rank(half)) else {
            continue;
        };
        if index >= other_index {
            continue;
        }
        let other = halves[other_index];
        if candidates.unique(other, rank(other)) != Some(index) {
            continue;
        }
        let (keep, merge) = if rank(half) <= rank(other) {
            (half.connection_id, other.connection_id)
        } else {
            (other.connection_id, half.connection_id)
        };
        if !connection_ids.contains(&keep) || !connection_ids.contains(&merge) {
            return Err(CloudError::InvalidConnection("connection_not_found".into()));
        }
        replacements.insert(merge, keep);
    }
    for exit in into.rooms.iter_mut().flat_map(|room| &mut room.exits) {
        if let Some(keep) = replacements.get(&exit.connection_id) {
            exit.connection_id = *keep;
        }
    }
    into.connections
        .retain(|connection| !replacements.contains_key(&connection.id));
    Ok(())
}

/// Direction buckets bound each reciprocal query to at most twice the number
/// of directions, even when thousands of ambiguous exits share two rooms.
#[derive(Default)]
struct ReciprocalCandidates {
    buckets: HashMap<
        (
            RoomNumber,
            RoomNumber,
            crate::ExitDirection,
            Option<crate::ExitDirection>,
        ),
        CandidateOrigins,
    >,
}

#[derive(Default)]
struct CandidateOrigins {
    count: usize,
    // Count and one representative index for each source document.
    origins: HashMap<usize, (usize, usize)>,
}

impl ReciprocalCandidates {
    fn insert(&mut self, exit: &ExitTopology, origin: usize, index: usize) {
        let Some(to) = exit.to_room_in_area.filter(|to| *to != exit.from_room) else {
            return;
        };
        let bucket = self
            .buckets
            .entry((exit.from_room, to, exit.from_direction, exit.to_direction))
            .or_default();
        bucket.count += 1;
        let entry = bucket.origins.entry(origin).or_insert((0, index));
        entry.0 += 1;
    }

    fn unique(&self, exit: &ExitTopology, origin: usize) -> Option<usize> {
        let to = exit.to_room_in_area.filter(|to| *to != exit.from_room)?;
        let directions = exit
            .to_direction
            .as_ref()
            .map_or(crate::ExitDirection::ALL.as_slice(), std::slice::from_ref);
        let mut found = None;
        for direction in directions {
            for arrival in [None, Some(exit.from_direction)] {
                let Some(bucket) = self.buckets.get(&(to, exit.from_room, *direction, arrival))
                else {
                    continue;
                };
                let own = bucket.origins.get(&origin).map_or(0, |(count, _)| *count);
                match bucket.count - own {
                    0 => {}
                    1 if found.is_none() => {
                        // With exactly one foreign member there are at most
                        // two origins, so this search is also bounded.
                        found = bucket
                            .origins
                            .iter()
                            .find(|(rank, _)| **rank != origin)
                            .map(|(_, (_, index))| *index);
                    }
                    _ => return None,
                }
            }
        }
        found
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use chrono::{DateTime, Utc};
    use uuid::Uuid;

    use super::*;
    use crate::{
        Area, AreaAccess, Connection, ConnectionKind, ConnectionRouting, Exit, ExitArgs,
        ExitDirection, HorizontalAlignment, Label, LabelId, MapPoint, Property, Shape, ShapeId,
        ShapeType, VerticalAlignment,
    };

    fn area_id(seed: u128) -> AreaId {
        AreaId(Uuid::from_u128(seed))
    }

    fn area(id: AreaId, rev: i64) -> AreaWithDetails {
        AreaWithDetails {
            area: Area {
                id,
                user_id: None,
                atlas_id: None,
                atlas_name: None,
                name: format!("area {id}"),
                created_at: DateTime::<Utc>::UNIX_EPOCH,
                rev,
                access: Some(AreaAccess::OWNER),
                owner_nickname: None,
                copied_from_area_id: None,
                copied_from_rev: None,
                copied_at: None,
                family_token: None,
            },
            format_version: crate::AREA_FORMAT_VERSION,
            content_hash: None,
            properties: vec![],
            rooms: vec![],
            labels: vec![],
            shapes: vec![],
            connections: vec![],
            linked_areas: vec![],
        }
    }

    fn room(number: i32, x: f32, y: f32, level: i32) -> RoomWithDetails {
        RoomWithDetails {
            room_number: RoomNumber(number),
            title: format!("room {number}"),
            description: String::new(),
            level,
            x,
            y,
            color: String::new(),
            properties: vec![],
            exits: vec![],
            tags: BTreeSet::new(),
            is_secret: false,
            external_id: None,
        }
    }

    fn with_rooms(id: AreaId, numbers: &[i32]) -> AreaWithDetails {
        let mut document = area(id, 1);
        let mut x = 0.0;
        for number in numbers {
            document.rooms.push(room(*number, x, 0.0, 0));
            x += 10.0;
        }
        document
    }

    /// Creates an exit through the shared exit applier, so its connection
    /// (auto-paired with a reciprocal, else a fresh one-member row with
    /// direction-default anchors) is exactly what a real document holds.
    fn add_exit(
        document: &mut AreaWithDetails,
        from: i32,
        direction: ExitDirection,
        to: Option<(AreaId, i32)>,
        seed: u128,
    ) -> ExitId {
        let id = ExitId(Uuid::from_u128(seed));
        let key = RoomKey::new(document.area.id, RoomNumber(from));
        area_edits::create_room_exit(
            document,
            &key,
            ExitArgs {
                id: Some(id),
                from_direction: direction,
                to_area_id: to.map(|(area, _)| area),
                to_room_number: to.map(|(_, number)| RoomNumber(number)),
                weight: 1.0,
                ..ExitArgs::default()
            },
        )
        .expect("fixture exit");
        id
    }

    /// A reciprocal cross-area link: `a_room` east to `b_room`, `b_room`
    /// west back. Each side is a one-member External connection.
    fn link(
        a: &mut AreaWithDetails,
        a_room: i32,
        b: &mut AreaWithDetails,
        b_room: i32,
        seed: u128,
    ) {
        add_exit(
            a,
            a_room,
            ExitDirection::East,
            Some((b.area.id, b_room)),
            seed,
        );
        add_exit(
            b,
            b_room,
            ExitDirection::West,
            Some((a.area.id, a_room)),
            seed + 1,
        );
    }

    fn assert_valid(document: &mut AreaWithDetails) {
        area_edits::validate_connection_graph(document).expect("fixture graph is valid");
    }

    fn plan(
        into: &AreaWithDetails,
        sources: &[(&AreaWithDetails, Translate)],
        inbound: &[&AreaWithDetails],
        floor: i32,
    ) -> AreaMergePlan {
        let mut expected = vec![(into.area.id, into.area.rev)];
        expected.extend(sources.iter().map(|(s, _)| (s.area.id, s.area.rev)));
        expected.extend(inbound.iter().map(|i| (i.area.id, i.area.rev)));
        AreaMergePlan {
            into: into.area.id,
            sources: sources
                .iter()
                .map(|(source, translate)| AreaMergeSource {
                    id: source.area.id,
                    translate: *translate,
                    rooms: None,
                })
                .collect(),
            inbound: inbound.iter().map(|document| document.area.id).collect(),
            expected,
            number_floor: RoomNumber(floor),
        }
    }

    fn shift(x: f32, y: f32, level: i32) -> Translate {
        Translate { x, y, level }
    }

    fn room_in(document: &AreaWithDetails, number: i32) -> &RoomWithDetails {
        document
            .rooms
            .iter()
            .find(|room| room.room_number == RoomNumber(number))
            .unwrap_or_else(|| panic!("room {number} present"))
    }

    fn exit_in(document: &AreaWithDetails, id: ExitId) -> &Exit {
        document
            .rooms
            .iter()
            .flat_map(|room| &room.exits)
            .find(|exit| exit.id == id)
            .expect("exit present")
    }

    fn connection_in(document: &AreaWithDetails, id: ConnectionId) -> &Connection {
        document
            .connections
            .iter()
            .find(|connection| connection.id == id)
            .expect("connection present")
    }

    fn members(document: &AreaWithDetails, id: ConnectionId) -> usize {
        document
            .rooms
            .iter()
            .flat_map(|room| &room.exits)
            .filter(|exit| exit.connection_id == id)
            .count()
    }

    fn numbers(document: &AreaWithDetails) -> Vec<i32> {
        let mut numbers: Vec<i32> = document
            .rooms
            .iter()
            .map(|room| room.room_number.0)
            .collect();
        numbers.sort_unstable();
        numbers
    }

    fn remap_of(outcome: &AreaMergeOutcome, source: AreaId) -> Vec<(i32, i32)> {
        outcome
            .rooms
            .iter()
            .filter(|remap| remap.from.area_id == source)
            .map(|remap| (remap.from.room_number.0, remap.to.0))
            .collect()
    }

    fn approx(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() < 1e-4
    }

    #[test]
    fn partial_merge_splits_a_thousand_boundary_connections() {
        let mut source = with_rooms(area_id(2), &[1, 2]);
        add_exit(
            &mut source,
            1,
            ExitDirection::East,
            Some((area_id(2), 2)),
            10,
        );
        add_exit(
            &mut source,
            2,
            ExitDirection::West,
            Some((area_id(2), 1)),
            11,
        );
        let rooms = source.rooms.clone();
        let connection = source.connections[0].clone();
        source.rooms.clear();
        source.connections.clear();
        let mut moving = Vec::new();
        for pair in 0..1024 {
            let from = 2 * pair + 1;
            let to = from + 1;
            let mut connection = connection.clone();
            connection.id = ConnectionId(Uuid::from_u128(100 + u128::try_from(pair).unwrap()));
            connection.endpoint_a.room_number = RoomNumber(from);
            connection.endpoint_b.as_mut().unwrap().room_number = RoomNumber(to);
            for (template, number, destination) in [(&rooms[0], from, to), (&rooms[1], to, from)] {
                let mut room = template.clone();
                room.room_number = RoomNumber(number);
                room.exits[0].id = ExitId(Uuid::from_u128(100 + u128::try_from(number).unwrap()));
                room.exits[0].connection_id = connection.id;
                room.exits[0].to_room_number = Some(RoomNumber(destination));
                source.rooms.push(room);
            }
            source.connections.push(connection);
            moving.push(from);
        }
        assert_valid(&mut source);
        let mut into = with_rooms(area_id(1), &[1]);
        let plan = partial(
            plan(&into, &[(&source, Translate::default())], &[], 2),
            0,
            &moving,
        );
        let mut sources = [source];
        let outcome = apply_area_merge(&plan, &mut into, &mut sources, &mut []).unwrap();
        assert_eq!(outcome.rooms.len(), 1024);
        assert_eq!(into.rooms.len(), 1025);
        assert_eq!(sources[0].rooms.len(), 1024);
        for document in [&into, &sources[0]] {
            assert_eq!(document.connections.len(), 1024);
            assert!(
                document
                    .connections
                    .iter()
                    .all(|connection| connection.kind == ConnectionKind::External
                        && connection.endpoint_b.is_none())
            );
        }
    }

    #[test]
    fn indexed_candidates_match_exhaustive_reciprocity() {
        // Dense shared endpoints, absent/explicit arrival directions, same
        // origins, self loops, and external exits. Compare the index with the
        // original all-pairs rule independently of its bucket implementation.
        for seed in 0..32_u64 {
            let mut state = seed + 1;
            let mut draw = |limit| {
                state = state
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                usize::try_from((state >> 32) % limit).unwrap()
            };
            let mut exits = Vec::new();
            let mut ranks = Vec::new();
            let mut index = ReciprocalCandidates::default();
            for n in 0..200 {
                let topology = ExitTopology {
                    id: ExitId(Uuid::from_u128(n + 1)),
                    connection_id: ConnectionId(Uuid::from_u128(n + 1)),
                    from_room: RoomNumber(i32::try_from(draw(4)).unwrap()),
                    from_direction: ExitDirection::ALL[draw(14)],
                    to_room_in_area: if draw(8) == 0 {
                        None
                    } else {
                        Some(RoomNumber(i32::try_from(draw(4)).unwrap()))
                    },
                    to_direction: if draw(3) == 0 {
                        None
                    } else {
                        Some(ExitDirection::ALL[draw(14)])
                    },
                    leaves_area: false,
                };
                let rank = draw(3);
                index.insert(&topology, rank, exits.len());
                exits.push(topology);
                ranks.push(rank);
            }
            for (n, exit) in exits.iter().enumerate() {
                let expected: Vec<_> = exits
                    .iter()
                    .enumerate()
                    .filter(|(m, other)| {
                        ranks[*m] != ranks[n] && area_edits::members_are_reciprocal(exit, other)
                    })
                    .map(|(m, _)| m)
                    .collect();
                assert_eq!(
                    index.unique(exit, ranks[n]),
                    match expected.as_slice() {
                        [m] => Some(*m),
                        _ => None,
                    },
                    "seed {seed}, exit {n}"
                );
            }
        }
    }

    #[test]
    fn indexed_reattachment_matches_editor_for_renumbered_and_split_links() {
        let mut into = with_rooms(area_id(1), &[1, 2]);
        let mut source = with_rooms(area_id(2), &[1, 2, 3]);
        link(&mut into, 1, &mut source, 1, 10);
        for (from, to, direction, id) in [
            (1, 2, ExitDirection::East, 20),
            (2, 1, ExitDirection::West, 21),
            (2, 3, ExitDirection::North, 22),
            (3, 2, ExitDirection::South, 23),
        ] {
            add_exit(&mut source, from, direction, Some((area_id(2), to)), id);
        }
        let plan = partial(
            plan(&into, &[(&source, shift(10.0, 20.0, 1))], &[], 3),
            0,
            &[1, 2],
        );
        let mut sources = [source];
        let order = source_order(&plan, &sources).unwrap();
        let remap = allocate_room_numbers(&plan, &into, &sources, &order).unwrap();
        let moved = move_content(&plan, &mut into, &mut sources, &order, &remap.by_key).unwrap();
        for document in [&mut into, &mut sources[0]] {
            retarget_exits(document, plan.into, &remap.by_key);
            let mut expected = document.clone();
            repair_changed_exits(&mut expected, &moved.before);
            reattach_changed_exits(document, &moved.before);
            assert_eq!(
                serde_json::to_value(&*document).unwrap(),
                serde_json::to_value(expected).unwrap()
            );
            assert_valid(document);
        }
    }

    #[test]
    fn thousands_of_colliding_rooms_are_remapped_without_loss() {
        let room_numbers: Vec<_> = (1..=4096).collect();
        let mut into = with_rooms(area_id(1), &room_numbers);
        let source = with_rooms(area_id(2), &room_numbers);
        let plan = plan(&into, &[(&source, Translate::default())], &[], 4097);
        let outcome = apply_area_merge(&plan, &mut into, &mut [source], &mut []).unwrap();
        assert_eq!(numbers(&into), (1..=8192).collect::<Vec<_>>());
        assert_eq!(outcome.rooms.len(), 4096);
        assert_eq!(
            remap_of(&outcome, area_id(2)),
            (1..=4096).map(|n| (n, n + 4096)).collect::<Vec<_>>()
        );
    }

    #[test]
    fn free_numbers_are_kept() {
        let mut into = with_rooms(area_id(1), &[1, 2]);
        let source = with_rooms(area_id(2), &[5, 7]);
        let plan = plan(&into, &[(&source, Translate::default())], &[], 3);

        let outcome = apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("merge");

        assert_eq!(remap_of(&outcome, area_id(2)), vec![(5, 5), (7, 7)]);
        assert_eq!(numbers(&into), vec![1, 2, 5, 7]);
    }

    #[test]
    fn colliding_numbers_are_allocated_above_the_destination() {
        let mut into = with_rooms(area_id(1), &[1, 2, 3]);
        let source = with_rooms(area_id(2), &[1, 2]);
        let plan = plan(&into, &[(&source, Translate::default())], &[], 4);

        let outcome = apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("merge");

        assert_eq!(remap_of(&outcome, area_id(2)), vec![(1, 4), (2, 5)]);
        assert_eq!(numbers(&into), vec![1, 2, 3, 4, 5]);
    }

    /// The destination holds 1 and 2 and a draft holds 3..9 (floor 10):
    /// a colliding number and a number inside the reserved band are both
    /// allocated at the floor, in source order.
    #[test]
    fn allocation_starts_at_the_floor_and_a_number_in_the_reserved_band_is_not_kept() {
        let mut into = with_rooms(area_id(1), &[1, 2]);
        let source = with_rooms(area_id(2), &[1, 4]);
        let plan = plan(&into, &[(&source, Translate::default())], &[], 10);

        let outcome = apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("merge");

        assert_eq!(remap_of(&outcome, area_id(2)), vec![(1, 10), (4, 11)]);
        assert_eq!(numbers(&into), vec![1, 2, 10, 11]);
    }

    /// The destination holds 1 and 3 and a draft holds 4 (floor 5): the gap
    /// at 2 is kept, 4 is in the reserved band and moves to the floor, and
    /// 9 is above the floor, where no draft can reach, so it is kept.
    #[test]
    fn a_gap_below_the_destination_maximum_is_kept_and_the_reserved_band_is_reallocated() {
        let mut into = with_rooms(area_id(1), &[1, 3]);
        let source = with_rooms(area_id(2), &[2, 4, 9]);
        let plan = plan(&into, &[(&source, Translate::default())], &[], 5);

        let outcome = apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("merge");

        assert_eq!(remap_of(&outcome, area_id(2)), vec![(2, 2), (4, 5), (9, 9)]);
        assert_eq!(numbers(&into), vec![1, 2, 3, 5, 9]);
    }

    #[test]
    fn allocation_skips_numbers_already_assigned() {
        let mut into = with_rooms(area_id(1), &[1]);
        let first = with_rooms(area_id(2), &[1, 2, 3]);
        let second = with_rooms(area_id(3), &[1, 9]);
        let plan = plan(
            &into,
            &[
                (&first, Translate::default()),
                (&second, Translate::default()),
            ],
            &[],
            2,
        );

        let outcome =
            apply_area_merge(&plan, &mut into, &mut [first, second], &mut []).expect("merge");

        assert_eq!(remap_of(&outcome, area_id(2)), vec![(1, 4), (2, 2), (3, 3)]);
        assert_eq!(remap_of(&outcome, area_id(3)), vec![(1, 5), (9, 9)]);
        assert_eq!(numbers(&into), vec![1, 2, 3, 4, 5, 9]);
    }

    #[test]
    fn rooms_labels_shapes_and_route_points_are_translated() {
        let mut into = with_rooms(area_id(1), &[1]);
        let mut source = area(area_id(2), 1);
        source.rooms.push(room(1, 0.0, 0.0, 0));
        source.rooms.push(room(2, 10.0, 0.0, 0));
        let east = add_exit(
            &mut source,
            1,
            ExitDirection::East,
            Some((area_id(2), 2)),
            10,
        );
        add_exit(
            &mut source,
            2,
            ExitDirection::West,
            Some((area_id(2), 1)),
            11,
        );
        let paired_id = exit_in(&source, east).connection_id;
        assert_eq!(members(&source, paired_id), 2, "fixture auto-paired");
        {
            let paired = source
                .connections
                .iter_mut()
                .find(|connection| connection.id == paired_id)
                .expect("paired connection");
            paired.routing = ConnectionRouting::Manual;
            paired.route_points = vec![MapPoint::new(5.0, -3.0)];
        }
        let dangling = add_exit(&mut source, 2, ExitDirection::North, None, 12);
        let dangling_id = exit_in(&source, dangling).connection_id;
        source
            .connections
            .iter_mut()
            .find(|connection| connection.id == dangling_id)
            .expect("dangling connection")
            .route_points = vec![MapPoint::new(1.0, 1.0)];
        source.labels.push(Label {
            id: LabelId(Uuid::from_u128(20)),
            level: 1,
            x: 2.0,
            y: 3.0,
            width: 4.0,
            height: 1.0,
            horizontal_alignment: HorizontalAlignment::Center,
            vertical_alignment: VerticalAlignment::Center,
            text: "sign".to_string(),
            color: String::new(),
            background_color: String::new(),
            font_size: 12,
            font_weight: 400,
            is_secret: false,
        });
        source.shapes.push(Shape {
            id: ShapeId(Uuid::from_u128(21)),
            level: -1,
            x: -2.0,
            y: 8.0,
            width: 3.0,
            height: 3.0,
            background_color: None,
            stroke_color: None,
            shape_type: ShapeType::Rectangle,
            border_radius: 0.0,
            stroke_width: 1.0,
            is_secret: false,
        });
        assert_valid(&mut source);
        let plan = plan(&into, &[(&source, shift(100.0, 50.0, 2))], &[], 2);

        apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("merge");

        let moved_1 = room_in(&into, 3);
        let moved_2 = room_in(&into, 2);
        assert!(approx(moved_1.x, 100.0) && approx(moved_1.y, 50.0));
        assert_eq!(moved_1.level, 2);
        assert!(approx(moved_2.x, 110.0) && approx(moved_2.y, 50.0));
        assert_eq!(moved_2.level, 2);
        let paired = connection_in(&into, paired_id);
        assert_eq!(paired.route_points, vec![MapPoint::new(105.0, 47.0)]);
        assert_eq!(paired.endpoint_a.room_number, RoomNumber(2));
        assert_eq!(
            paired.endpoint_b.map(|endpoint| endpoint.room_number),
            Some(RoomNumber(3))
        );
        let dangling = connection_in(&into, dangling_id);
        assert_eq!(dangling.route_points, vec![MapPoint::new(101.0, 51.0)]);
        assert_eq!(dangling.endpoint_a.room_number, RoomNumber(2));
        let label = &into.labels[0];
        assert_eq!(label.id, LabelId(Uuid::from_u128(20)));
        assert!(approx(label.x, 102.0) && approx(label.y, 53.0));
        assert_eq!(label.level, 3);
        let shape = &into.shapes[0];
        assert_eq!(shape.id, ShapeId(Uuid::from_u128(21)));
        assert!(approx(shape.x, 98.0) && approx(shape.y, 58.0));
        assert_eq!(shape.level, 1);
        assert!(
            approx(into.rooms[0].x, 0.0),
            "destination rooms do not move"
        );
    }

    #[test]
    fn exits_within_a_source_follow_the_renumbering() {
        let mut into = with_rooms(area_id(1), &[1, 2]);
        let mut source = with_rooms(area_id(2), &[1, 2]);
        let east = add_exit(
            &mut source,
            1,
            ExitDirection::East,
            Some((area_id(2), 2)),
            10,
        );
        let west = add_exit(
            &mut source,
            2,
            ExitDirection::West,
            Some((area_id(2), 1)),
            11,
        );
        let pair = exit_in(&source, east).connection_id;
        assert_valid(&mut source);
        let plan = plan(&into, &[(&source, Translate::default())], &[], 3);

        apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("merge");

        let east = exit_in(&into, east);
        assert_eq!(
            (east.to_area_id, east.to_room_number),
            (Some(area_id(1)), Some(RoomNumber(4)))
        );
        let west = exit_in(&into, west);
        assert_eq!(
            (west.to_area_id, west.to_room_number),
            (Some(area_id(1)), Some(RoomNumber(3)))
        );
        assert_eq!(east.connection_id, pair, "connection ids are carried");
        assert_eq!(west.connection_id, pair);
        assert_eq!(members(&into, pair), 2, "the pair survives renumbering");
        let connection = connection_in(&into, pair);
        assert_eq!(connection.endpoint_a.room_number, RoomNumber(3));
        assert_eq!(
            connection.endpoint_b.map(|endpoint| endpoint.room_number),
            Some(RoomNumber(4))
        );
        assert_eq!(connection.kind, ConnectionKind::Internal);
        assert_eq!(into.connections.len(), 1);
    }

    #[test]
    fn destination_exits_into_a_source_are_retargeted() {
        let mut into = with_rooms(area_id(1), &[1]);
        let mut source = with_rooms(area_id(2), &[1]);
        let exit = add_exit(&mut into, 1, ExitDirection::East, Some((area_id(2), 1)), 10);
        let connection_id = exit_in(&into, exit).connection_id;
        assert_valid(&mut into);
        assert_valid(&mut source);
        let plan = plan(&into, &[(&source, Translate::default())], &[], 2);

        apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("merge");

        let retargeted = exit_in(&into, exit);
        assert_eq!(
            (retargeted.to_area_id, retargeted.to_room_number),
            (Some(area_id(1)), Some(RoomNumber(2)))
        );
        assert_eq!(retargeted.connection_id, connection_id);
        let connection = connection_in(&into, connection_id);
        assert_eq!(connection.kind, ConnectionKind::Internal);
        assert_eq!(connection.endpoint_a.room_number, RoomNumber(1));
        assert_eq!(
            connection.endpoint_b.map(|endpoint| endpoint.room_number),
            Some(RoomNumber(2)),
            "a one-way exit that now stays in the area gains its far endpoint"
        );
        assert_eq!(members(&into, connection_id), 1);
    }

    #[test]
    fn inbound_exits_into_a_source_are_retargeted_and_the_document_bumped() {
        let mut into = with_rooms(area_id(1), &[1]);
        let source = with_rooms(area_id(2), &[1, 2]);
        let mut third = with_rooms(area_id(3), &[1]);
        third.area.rev = 7;
        let exit = add_exit(&mut third, 1, ExitDirection::Up, Some((area_id(2), 2)), 10);
        let connection_id = exit_in(&third, exit).connection_id;
        assert_valid(&mut third);
        let plan = plan(&into, &[(&source, Translate::default())], &[&third], 2);
        let mut inbound = vec![third];

        let outcome =
            apply_area_merge(&plan, &mut into, &mut [source], &mut inbound).expect("merge");

        let third = &inbound[0];
        let retargeted = exit_in(third, exit);
        assert_eq!(
            (retargeted.to_area_id, retargeted.to_room_number),
            (Some(area_id(1)), Some(RoomNumber(2)))
        );
        assert_eq!(third.area.rev, 8);
        let connection = connection_in(third, connection_id);
        assert_eq!(connection.kind, ConnectionKind::External);
        assert!(connection.endpoint_b.is_none());
        assert!(outcome.versions.contains(&VersionInfo {
            resource: ResourceKind::Area,
            id: area_id(3).0,
            rev: 8,
            deleted: false,
        }));
    }

    #[test]
    fn reciprocal_halves_pair_and_the_destination_half_is_kept() {
        let mut into = with_rooms(area_id(1), &[1]);
        let mut source = with_rooms(area_id(2), &[1]);
        link(&mut into, 1, &mut source, 1, 10);
        let into_exit = ExitId(Uuid::from_u128(10));
        let source_exit = ExitId(Uuid::from_u128(11));
        let kept = exit_in(&into, into_exit).connection_id;
        let merged = exit_in(&source, source_exit).connection_id;
        assert_valid(&mut into);
        assert_valid(&mut source);
        let plan = plan(&into, &[(&source, shift(10.0, 0.0, 0))], &[], 2);

        apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("merge");

        assert_eq!(into.connections.len(), 1, "the link renders once");
        assert_eq!(exit_in(&into, into_exit).connection_id, kept);
        assert_eq!(exit_in(&into, source_exit).connection_id, kept);
        assert!(!into.connections.iter().any(|c| c.id == merged));
        let connection = connection_in(&into, kept);
        assert_eq!(connection.kind, ConnectionKind::Internal);
        assert_eq!(connection.endpoint_a.room_number, RoomNumber(1));
        assert_eq!(
            connection.endpoint_b.map(|endpoint| endpoint.room_number),
            Some(RoomNumber(2))
        );
        let back = exit_in(&into, source_exit);
        assert_eq!(
            (back.to_area_id, back.to_room_number),
            (Some(area_id(1)), Some(RoomNumber(1)))
        );
    }

    #[test]
    fn reciprocal_halves_between_two_sources_keep_the_earlier_source_half() {
        let mut into = with_rooms(area_id(1), &[1]);
        let mut first = with_rooms(area_id(2), &[1]);
        let mut second = with_rooms(area_id(3), &[1]);
        link(&mut second, 1, &mut first, 1, 10);
        let second_exit = ExitId(Uuid::from_u128(10));
        let first_exit = ExitId(Uuid::from_u128(11));
        let kept = exit_in(&first, first_exit).connection_id;
        assert_valid(&mut first);
        assert_valid(&mut second);
        let plan = plan(
            &into,
            &[
                (&first, Translate::default()),
                (&second, Translate::default()),
            ],
            &[],
            2,
        );

        apply_area_merge(&plan, &mut into, &mut [first, second], &mut []).expect("merge");

        assert_eq!(into.connections.len(), 1);
        assert_eq!(exit_in(&into, first_exit).connection_id, kept);
        assert_eq!(exit_in(&into, second_exit).connection_id, kept);
        assert_eq!(members(&into, kept), 2);
        let connection = connection_in(&into, kept);
        assert_eq!(connection.endpoint_a.room_number, RoomNumber(2));
        assert_eq!(
            connection.endpoint_b.map(|endpoint| endpoint.room_number),
            Some(RoomNumber(3))
        );
    }

    #[test]
    fn a_one_way_exit_into_the_destination_stays_its_own_connection() {
        let mut into = with_rooms(area_id(1), &[1, 2]);
        let mut source = with_rooms(area_id(2), &[1]);
        // Not reciprocal: the destination reaches the source room, but the
        // source room comes back to a different destination room.
        let out = add_exit(&mut into, 1, ExitDirection::East, Some((area_id(2), 1)), 10);
        let back = add_exit(
            &mut source,
            1,
            ExitDirection::West,
            Some((area_id(1), 2)),
            11,
        );
        let out_connection = exit_in(&into, out).connection_id;
        let back_connection = exit_in(&source, back).connection_id;
        assert_valid(&mut into);
        assert_valid(&mut source);
        let plan = plan(&into, &[(&source, Translate::default())], &[], 3);

        apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("merge");

        assert_eq!(into.connections.len(), 2, "nothing is paired");
        assert_eq!(members(&into, out_connection), 1);
        assert_eq!(members(&into, back_connection), 1);
        for (id, from, to) in [(out_connection, 1, 3), (back_connection, 2, 3)] {
            let connection = connection_in(&into, id);
            assert_eq!(connection.kind, ConnectionKind::Internal);
            assert_eq!(connection.endpoint_a.room_number, RoomNumber(from));
            assert_eq!(
                connection.endpoint_b.map(|endpoint| endpoint.room_number),
                Some(RoomNumber(to))
            );
        }
    }

    #[test]
    fn exits_without_a_destination_are_carried_unchanged() {
        let mut into = with_rooms(area_id(1), &[1]);
        let mut source = with_rooms(area_id(2), &[1]);
        let dangling = add_exit(&mut source, 1, ExitDirection::North, None, 10);
        let redacted = add_exit(&mut source, 1, ExitDirection::South, None, 11);
        {
            let exit = source.rooms[0]
                .exits
                .iter_mut()
                .find(|exit| exit.id == redacted)
                .expect("redacted exit");
            exit.to_unknown = true;
            exit.to_area_token = Some("hidden".to_string());
        }
        assert_valid(&mut source);
        let before_dangling = exit_in(&source, dangling).clone();
        let before_redacted = exit_in(&source, redacted).clone();
        let plan = plan(&into, &[(&source, shift(5.0, 5.0, 1))], &[], 2);

        apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("merge");

        let carried = exit_in(&into, dangling);
        assert_eq!(
            (
                carried.to_area_id,
                carried.to_room_number,
                carried.connection_id
            ),
            (None, None, before_dangling.connection_id)
        );
        assert_eq!(
            connection_in(&into, carried.connection_id).kind,
            ConnectionKind::Dangling
        );
        let carried = exit_in(&into, redacted);
        assert!(carried.to_unknown);
        assert_eq!(carried.to_area_token, before_redacted.to_area_token);
        assert_eq!((carried.to_area_id, carried.to_room_number), (None, None));
        assert_eq!(carried.connection_id, before_redacted.connection_id);
        assert_eq!(
            connection_in(&into, carried.connection_id).kind,
            ConnectionKind::External
        );
    }

    #[test]
    fn a_graph_validation_failure_propagates() {
        let mut into = with_rooms(area_id(1), &[1]);
        add_exit(&mut into, 1, ExitDirection::North, None, 10);
        // The exit's connection row is missing: an invariant the merged
        // document's final validation must reject.
        into.connections.clear();
        let source = with_rooms(area_id(2), &[1]);
        let plan = plan(&into, &[(&source, Translate::default())], &[], 2);

        let error =
            apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect_err("invalid graph");

        assert!(
            matches!(&error, CloudError::InvalidConnection(reason) if reason == "connection_not_found"),
            "{error:?}"
        );
    }

    #[test]
    fn an_empty_source_list_is_refused() {
        let mut into = with_rooms(area_id(1), &[1]);
        let plan = AreaMergePlan {
            into: area_id(1),
            sources: vec![],
            inbound: vec![],
            expected: vec![(area_id(1), 1)],
            number_floor: RoomNumber(2),
        };

        let error = apply_area_merge(&plan, &mut into, &mut [], &mut []).expect_err("refused");

        assert!(
            matches!(&error, CloudError::StructuralConflict(code) if code == "merge_areas_no_sources"),
            "{error:?}"
        );
        assert_eq!(into.area.rev, 1, "nothing touched");
    }

    #[test]
    fn a_source_equal_to_the_destination_or_repeated_is_refused() {
        let mut into = with_rooms(area_id(1), &[1]);
        let source = with_rooms(area_id(2), &[1]);
        let same = |ids: &[u128]| AreaMergePlan {
            into: area_id(1),
            sources: ids
                .iter()
                .map(|id| AreaMergeSource {
                    id: area_id(*id),
                    translate: Translate::default(),
                    rooms: None,
                })
                .collect(),
            inbound: vec![],
            expected: vec![],
            number_floor: RoomNumber(2),
        };

        for plan in [same(&[1]), same(&[2, 2])] {
            let error = apply_area_merge(&plan, &mut into, &mut [source.clone()], &mut [])
                .expect_err("refused");
            assert!(
                matches!(&error, CloudError::StructuralConflict(code) if code == "merge_areas_same_area"),
                "{error:?}"
            );
        }
        assert_eq!(into.rooms.len(), 1, "nothing touched");
    }

    #[test]
    fn a_plan_id_without_a_document_is_area_not_found() {
        let mut into = with_rooms(area_id(1), &[1]);
        let source = with_rooms(area_id(2), &[1]);
        let third = with_rooms(area_id(3), &[1]);
        let plan = plan(&into, &[(&source, Translate::default())], &[&third], 2);

        let missing_source = apply_area_merge(&plan, &mut into, &mut [], &mut [third.clone()])
            .expect_err("no source document");
        assert!(matches!(missing_source, CloudError::AreaNotFound(id) if id == area_id(2)));

        let missing_inbound = apply_area_merge(&plan, &mut into, &mut [source.clone()], &mut [])
            .expect_err("no inbound document");
        assert!(matches!(missing_inbound, CloudError::AreaNotFound(id) if id == area_id(3)));

        let mut other = with_rooms(area_id(9), &[1]);
        let wrong_destination = apply_area_merge(&plan, &mut other, &mut [source], &mut [third])
            .expect_err("wrong into");
        assert!(matches!(wrong_destination, CloudError::AreaNotFound(id) if id == area_id(1)));
    }

    #[test]
    fn a_revision_mismatch_is_a_revision_conflict() {
        let mut into = with_rooms(area_id(1), &[1]);
        let mut source = with_rooms(area_id(2), &[1]);
        let mut plan = plan(&into, &[(&source, Translate::default())], &[], 2);
        source.area.rev = 5;
        plan.expected = vec![(area_id(1), 1), (area_id(2), 4)];

        let error =
            apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect_err("stale plan");

        assert!(
            matches!(
                error,
                CloudError::RevisionConflict {
                    id,
                    expected_rev: 4,
                    current_rev: 5
                } if id == area_id(2).0
            ),
            "{error:?}"
        );
        assert_eq!(into.rooms.len(), 1, "nothing touched");
    }

    #[test]
    fn the_remap_lists_every_source_room_in_plan_order() {
        let mut into = with_rooms(area_id(1), &[1]);
        let first = with_rooms(area_id(2), &[3, 1]);
        let second = with_rooms(area_id(3), &[1, 2]);
        let plan = plan(
            &into,
            &[
                (&second, Translate::default()),
                (&first, Translate::default()),
            ],
            &[],
            2,
        );

        let outcome =
            apply_area_merge(&plan, &mut into, &mut [first, second], &mut []).expect("merge");

        let listed: Vec<(AreaId, i32, i32)> = outcome
            .rooms
            .iter()
            .map(|remap| (remap.from.area_id, remap.from.room_number.0, remap.to.0))
            .collect();
        assert_eq!(
            listed,
            vec![
                (area_id(3), 1, 4),
                (area_id(3), 2, 2),
                (area_id(2), 1, 5),
                (area_id(2), 3, 3),
            ]
        );
        assert_eq!(into.rooms.len(), 5);
    }

    #[test]
    fn versions_report_the_destination_changed_inbound_and_deleted_sources() {
        let mut into = with_rooms(area_id(1), &[1]);
        into.area.rev = 3;
        let mut source = with_rooms(area_id(2), &[1]);
        source.area.rev = 9;
        let mut linked = with_rooms(area_id(3), &[1]);
        linked.area.rev = 4;
        add_exit(&mut linked, 1, ExitDirection::Up, Some((area_id(2), 1)), 10);
        assert_valid(&mut linked);
        let mut unrelated = with_rooms(area_id(4), &[1]);
        unrelated.area.rev = 6;
        add_exit(
            &mut unrelated,
            1,
            ExitDirection::Up,
            Some((area_id(1), 1)),
            11,
        );
        assert_valid(&mut unrelated);
        let untouched = serde_json::to_value(&unrelated).expect("serializable");
        let plan = plan(
            &into,
            &[(&source, Translate::default())],
            &[&linked, &unrelated],
            2,
        );
        let mut inbound = vec![linked, unrelated];

        let outcome =
            apply_area_merge(&plan, &mut into, &mut [source], &mut inbound).expect("merge");

        assert_eq!(
            outcome.versions,
            vec![
                version(area_id(1), 4, false),
                version(area_id(3), 5, false),
                version(area_id(2), 9, true),
            ]
        );
        assert_eq!(into.area.rev, 4);
        assert_eq!(
            serde_json::to_value(&inbound[1]).expect("serializable"),
            untouched,
            "an inbound document with nothing to retarget is left as it was"
        );
    }

    #[test]
    fn source_properties_are_dropped_and_identities_are_unchanged() {
        let mut into = with_rooms(area_id(1), &[1]);
        into.properties.push(Property {
            name: "zone".to_string(),
            value: "keep".to_string(),
            is_secret: false,
        });
        let mut source = with_rooms(area_id(2), &[1]);
        source.properties.push(Property {
            name: "zone".to_string(),
            value: "drop".to_string(),
            is_secret: false,
        });
        source.rooms[0].properties.push(Property {
            name: "kind".to_string(),
            value: "inn".to_string(),
            is_secret: true,
        });
        source.rooms[0].tags.insert("SAFE".to_string());
        source.rooms[0].external_id = Some("r-1".to_string());
        source.rooms[0].is_secret = true;
        source.rooms[0].color = "#ff0000".to_string();
        source.rooms[0].description = "cozy".to_string();
        let exit = add_exit(&mut source, 1, ExitDirection::North, None, 10);
        let connection_id = exit_in(&source, exit).connection_id;
        assert_valid(&mut source);
        let plan = plan(&into, &[(&source, Translate::default())], &[], 2);

        apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("merge");

        assert_eq!(into.properties.len(), 1);
        assert_eq!(into.properties[0].value, "keep");
        let moved = room_in(&into, 2);
        assert_eq!(moved.title, "room 1");
        assert_eq!(moved.description, "cozy");
        assert_eq!(moved.color, "#ff0000");
        assert_eq!(moved.properties.len(), 1);
        assert!(moved.properties[0].is_secret);
        assert!(moved.tags.contains("SAFE"));
        assert_eq!(moved.external_id.as_deref(), Some("r-1"));
        assert!(moved.is_secret);
        assert_eq!(moved.exits[0].id, exit);
        assert_eq!(moved.exits[0].connection_id, connection_id);
        assert!(into.connections.iter().any(|c| c.id == connection_id));
    }

    /// The plan with its `index`-th source narrowed to `rooms`.
    fn partial(mut plan: AreaMergePlan, index: usize, rooms: &[i32]) -> AreaMergePlan {
        plan.sources[index].rooms = Some(rooms.iter().map(|number| RoomNumber(*number)).collect());
        plan
    }

    fn sign_label(seed: u128) -> Label {
        Label {
            id: LabelId(Uuid::from_u128(seed)),
            level: 0,
            x: 1.0,
            y: 1.0,
            width: 4.0,
            height: 1.0,
            horizontal_alignment: HorizontalAlignment::Center,
            vertical_alignment: VerticalAlignment::Center,
            text: "sign".to_string(),
            color: String::new(),
            background_color: String::new(),
            font_size: 12,
            font_weight: 400,
            is_secret: false,
        }
    }

    fn box_shape(seed: u128) -> Shape {
        Shape {
            id: ShapeId(Uuid::from_u128(seed)),
            level: 0,
            x: 2.0,
            y: 2.0,
            width: 3.0,
            height: 3.0,
            background_color: None,
            stroke_color: None,
            shape_type: ShapeType::Rectangle,
            border_radius: 0.0,
            stroke_width: 1.0,
            is_secret: false,
        }
    }

    #[test]
    fn a_partial_source_moves_only_the_listed_rooms_and_is_written_not_deleted() {
        let mut into = with_rooms(area_id(1), &[1]);
        let mut source = with_rooms(area_id(2), &[1, 2, 3]);
        source.area.rev = 4;
        source.labels.push(sign_label(20));
        source.shapes.push(box_shape(21));
        let plan = partial(
            plan(&into, &[(&source, shift(10.0, 0.0, 1))], &[], 2),
            0,
            &[3, 2],
        );
        let mut sources = [source];

        let outcome =
            apply_area_merge(&plan, &mut into, &mut sources, &mut []).expect("partial merge");

        assert_eq!(
            remap_of(&outcome, area_id(2)),
            vec![(2, 2), (3, 3)],
            "the listed rooms, by ascending number"
        );
        assert_eq!(numbers(&into), vec![1, 2, 3]);
        let source = &sources[0];
        assert_eq!(numbers(source), vec![1], "the unlisted room stays");
        assert_eq!(source.area.rev, 5, "the source is written");
        assert_eq!(source.labels.len(), 1, "labels stay with the source");
        assert_eq!(source.shapes.len(), 1, "so do shapes");
        assert!(into.labels.is_empty() && into.shapes.is_empty());
        let moved = room_in(&into, 2);
        assert!(approx(moved.x, 20.0) && approx(moved.y, 0.0));
        assert_eq!(moved.level, 1);
        assert_eq!(
            outcome.versions,
            vec![version(area_id(1), 2, false), version(area_id(2), 5, false)],
            "a partial source is reported written, never deleted"
        );
    }

    #[test]
    fn a_partial_source_that_gives_every_room_is_still_written_not_deleted() {
        let mut into = with_rooms(area_id(1), &[1]);
        let source = with_rooms(area_id(2), &[1]);
        let plan = partial(
            plan(&into, &[(&source, Translate::default())], &[], 2),
            0,
            &[1],
        );
        let mut sources = [source];

        let outcome = apply_area_merge(&plan, &mut into, &mut sources, &mut []).expect("merge");

        assert_eq!(numbers(&into), vec![1, 2]);
        assert!(sources[0].rooms.is_empty(), "the source is emptied");
        assert_eq!(sources[0].area.rev, 2);
        assert_eq!(
            outcome.versions,
            vec![version(area_id(1), 2, false), version(area_id(2), 2, false)]
        );
    }

    /// The remaining rooms of a partial source see the merge as a third
    /// party would: their exits into moved rooms follow them, and the
    /// moved rooms' exits back keep naming the source. Each such exit
    /// now leaves its area, so its connection becomes a one-member
    /// `External` row, as any cross-area exit's is.
    #[test]
    fn a_partial_source_is_a_third_party_for_its_remaining_rooms() {
        let mut into = with_rooms(area_id(1), &[1]);
        let mut source = with_rooms(area_id(2), &[1, 2, 3]);
        // One-way exits, so nothing pairs: 1 -> 2 (remaining into moved) and
        // 2 -> 3 (moved into remaining).
        let inward = add_exit(
            &mut source,
            1,
            ExitDirection::East,
            Some((area_id(2), 2)),
            10,
        );
        let outward = add_exit(
            &mut source,
            2,
            ExitDirection::North,
            Some((area_id(2), 3)),
            11,
        );
        assert_valid(&mut source);
        let plan = partial(
            plan(&into, &[(&source, Translate::default())], &[], 2),
            0,
            &[2],
        );
        let mut sources = [source];

        apply_area_merge(&plan, &mut into, &mut sources, &mut []).expect("merge");

        let source = &mut sources[0];
        let retargeted = exit_in(source, inward);
        assert_eq!(
            (retargeted.to_area_id, retargeted.to_room_number),
            (Some(area_id(1)), Some(RoomNumber(2))),
            "the remaining room's exit follows the moved room"
        );
        let connection = connection_in(source, retargeted.connection_id);
        assert_eq!(connection.kind, ConnectionKind::External);
        assert!(connection.endpoint_b.is_none());
        assert_eq!(connection.endpoint_a.room_number, RoomNumber(1));

        let carried = exit_in(&into, outward);
        assert_eq!(
            (carried.to_area_id, carried.to_room_number),
            (Some(area_id(2)), Some(RoomNumber(3))),
            "the moved room's exit still names the source"
        );
        let connection = connection_in(&into, carried.connection_id);
        assert_eq!(connection.kind, ConnectionKind::External);
        assert!(connection.endpoint_b.is_none());
        assert_eq!(connection.endpoint_a.room_number, RoomNumber(2));
        assert_valid(source);
        assert_valid(&mut into);
    }

    /// A paired connection with one exit on each side of the split becomes
    /// two one-member halves: the staying exit keeps the original
    /// connection, the moving exit takes one whose id is derived from the
    /// split, so applying the same merge twice mints the same document.
    #[test]
    fn a_connection_straddling_the_split_is_unlinked_into_two_halves_with_derived_ids() {
        let into = with_rooms(area_id(1), &[1]);
        let mut source = with_rooms(area_id(2), &[1, 2]);
        let staying_exit = add_exit(
            &mut source,
            1,
            ExitDirection::East,
            Some((area_id(2), 2)),
            10,
        );
        let moving_exit = add_exit(
            &mut source,
            2,
            ExitDirection::West,
            Some((area_id(2), 1)),
            11,
        );
        let original = exit_in(&source, staying_exit).connection_id;
        assert_eq!(members(&source, original), 2, "fixture auto-paired");
        let connection = source
            .connections
            .iter_mut()
            .find(|c| c.id == original)
            .unwrap();
        connection.routing = ConnectionRouting::Manual;
        connection.route_points = vec![MapPoint::new(1.0, 2.0), MapPoint::new(3.0, 4.0)];
        assert_valid(&mut source);
        let plan = partial(
            plan(&into, &[(&source, shift(5.0, 0.0, 0))], &[], 2),
            0,
            &[2],
        );
        let run = || {
            let mut into = into.clone();
            let mut sources = [source.clone()];
            let outcome = apply_area_merge(&plan, &mut into, &mut sources, &mut []).expect("merge");
            (
                outcome,
                serde_json::to_value(&into).expect("serializable"),
                serde_json::to_value(&sources).expect("serializable"),
                into,
                sources,
            )
        };

        let (outcome, into_json, sources_json, mut into, mut sources) = run();

        let source = &mut sources[0];
        let staying = exit_in(source, staying_exit);
        assert_eq!(
            staying.connection_id, original,
            "the staying half keeps the id"
        );
        assert_eq!(
            (staying.to_area_id, staying.to_room_number),
            (Some(area_id(1)), Some(RoomNumber(2)))
        );
        assert_eq!(members(source, original), 1);
        let kept = connection_in(source, original);
        assert_eq!(kept.kind, ConnectionKind::External);
        assert!(kept.endpoint_b.is_none());
        assert!(
            kept.route_points.is_empty(),
            "the staying half becomes an external link"
        );
        assert_eq!(source.connections.len(), 1);

        let moving = exit_in(&into, moving_exit);
        let derived = split_connection_id(area_id(2), original, moving_exit);
        assert_eq!(
            moving.connection_id, derived,
            "the moving half's id is derived"
        );
        assert_ne!(derived, original);
        assert_eq!(
            (moving.to_area_id, moving.to_room_number),
            (Some(area_id(2)), Some(RoomNumber(1))),
            "the moved room's exit still names the source room"
        );
        let half = connection_in(&into, derived);
        assert_eq!(half.kind, ConnectionKind::External);
        assert!(half.endpoint_b.is_none());
        assert!(
            half.route_points.is_empty(),
            "the moved half must not retain translated internal route points"
        );
        assert_eq!(half.endpoint_a.room_number, RoomNumber(2));
        assert_eq!(into.connections.len(), 1);
        assert_valid(source);
        assert_valid(&mut into);

        let again = run();
        assert_eq!(again.0, outcome);
        assert_eq!(
            again.1, into_json,
            "the same merge twice mints the same destination"
        );
        assert_eq!(again.2, sources_json, "and the same source");
    }

    #[test]
    fn a_connection_wholly_within_the_moved_rooms_moves_with_them() {
        let mut into = with_rooms(area_id(1), &[1]);
        let mut source = with_rooms(area_id(2), &[1, 2, 3]);
        let east = add_exit(
            &mut source,
            2,
            ExitDirection::East,
            Some((area_id(2), 3)),
            10,
        );
        add_exit(
            &mut source,
            3,
            ExitDirection::West,
            Some((area_id(2), 2)),
            11,
        );
        let pair = exit_in(&source, east).connection_id;
        assert_eq!(members(&source, pair), 2);
        assert_valid(&mut source);
        let plan = partial(
            plan(&into, &[(&source, Translate::default())], &[], 2),
            0,
            &[2, 3],
        );
        let mut sources = [source];

        apply_area_merge(&plan, &mut into, &mut sources, &mut []).expect("merge");

        assert_eq!(numbers(&into), vec![1, 2, 3]);
        assert_eq!(numbers(&sources[0]), vec![1]);
        assert!(
            sources[0].connections.is_empty(),
            "the pair left the source"
        );
        assert_eq!(members(&into, pair), 2, "and arrived whole");
        let connection = connection_in(&into, pair);
        assert_eq!(connection.kind, ConnectionKind::Internal);
        assert_eq!(connection.endpoint_a.room_number, RoomNumber(2));
        assert_eq!(
            connection.endpoint_b.map(|endpoint| endpoint.room_number),
            Some(RoomNumber(3))
        );
    }

    #[test]
    fn a_partial_source_listing_a_missing_room_or_no_room_is_refused() {
        let mut into = with_rooms(area_id(1), &[1]);
        let source = with_rooms(area_id(2), &[1, 2]);
        let whole = plan(&into, &[(&source, Translate::default())], &[], 2);

        for (rooms, code) in [
            (&[9][..], "merge_areas_room_not_found"),
            (&[1, 9][..], "merge_areas_room_not_found"),
            (&[][..], "merge_areas_no_rooms"),
        ] {
            let plan = partial(whole.clone(), 0, rooms);
            let mut sources = [source.clone()];
            let error =
                apply_area_merge(&plan, &mut into, &mut sources, &mut []).expect_err("refused");
            assert!(
                matches!(&error, CloudError::StructuralConflict(got) if got == code),
                "{rooms:?}: {error:?}"
            );
            assert_eq!(into.rooms.len(), 1, "nothing touched");
            assert_eq!(sources[0].rooms.len(), 2);
            assert_eq!(sources[0].area.rev, 1);
        }
    }

    #[test]
    fn the_outcome_is_deterministic() {
        let mut into = with_rooms(area_id(1), &[1, 2]);
        let mut first = with_rooms(area_id(2), &[1, 2]);
        let mut second = with_rooms(area_id(3), &[1]);
        let mut third = with_rooms(area_id(4), &[1]);
        link(&mut into, 1, &mut first, 2, 10);
        link(&mut first, 1, &mut second, 1, 20);
        add_exit(
            &mut third,
            1,
            ExitDirection::Down,
            Some((area_id(3), 1)),
            30,
        );
        add_exit(
            &mut first,
            1,
            ExitDirection::South,
            Some((area_id(2), 2)),
            40,
        );
        for document in [&mut into, &mut first, &mut second, &mut third] {
            assert_valid(document);
        }
        let plan = plan(
            &into,
            &[
                (&first, shift(10.0, 0.0, 0)),
                (&second, shift(0.0, 10.0, 1)),
            ],
            &[&third],
            3,
        );

        let run = || {
            let mut into = into.clone();
            let mut inbound = vec![third.clone()];
            let outcome = apply_area_merge(
                &plan,
                &mut into,
                &mut [first.clone(), second.clone()],
                &mut inbound,
            )
            .expect("merge");
            (
                outcome,
                serde_json::to_value(&into).expect("serializable"),
                serde_json::to_value(&inbound).expect("serializable"),
            )
        };

        assert_eq!(run(), run());
    }

    /// Numerical refusals are preflight failures, including when an earlier
    /// source would otherwise have moved and an inbound exit would retarget.
    fn assert_numeric_refusal_unchanged(
        plan: &AreaMergePlan,
        mut into: AreaWithDetails,
        mut sources: Vec<AreaWithDetails>,
        mut inbound: Vec<AreaWithDetails>,
        expected_code: &str,
    ) {
        let before = serde_json::to_value((&into, &sources, &inbound)).expect("snapshot");
        let error = apply_area_merge(plan, &mut into, &mut sources, &mut inbound)
            .expect_err("unsafe arithmetic must refuse the merge");
        match error {
            CloudError::InvalidInput(code) | CloudError::StructuralConflict(code) => {
                assert_eq!(code, expected_code);
            }
            error => panic!("unexpected refusal: {error}"),
        }
        assert_eq!(
            serde_json::to_value((&into, &sources, &inbound)).expect("snapshot"),
            before,
            "a numerical refusal must not mutate any document",
        );
    }

    #[test]
    fn nonfinite_translation_is_refused_before_any_document_changes() {
        for value in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            for translate in [shift(value, 0.0, 0), shift(0.0, value, 0)] {
                let into = with_rooms(area_id(1), &[1]);
                let first = with_rooms(area_id(2), &[1]);
                let second = with_rooms(area_id(3), &[1]);
                let mut third = with_rooms(area_id(4), &[1]);
                add_exit(
                    &mut third,
                    1,
                    ExitDirection::East,
                    Some((first.area.id, 1)),
                    10,
                );
                let plan = plan(
                    &into,
                    &[(&first, Translate::default()), (&second, translate)],
                    &[&third],
                    2,
                );
                assert_numeric_refusal_unchanged(
                    &plan,
                    into,
                    vec![first, second],
                    vec![third],
                    "merge_areas_invalid_translation",
                );
            }
        }
    }

    #[test]
    fn translated_geometry_overflow_is_refused_before_mutation() {
        for axis in 0..2 {
            for kind in 0..4 {
                let into = with_rooms(area_id(1), &[1]);
                let mut source = with_rooms(area_id(2), &[1]);
                let (x, y) = if axis == 0 {
                    (f32::MAX, 0.0)
                } else {
                    (0.0, -f32::MAX)
                };
                match kind {
                    0 => {
                        source.rooms[0].x = x;
                        source.rooms[0].y = y;
                    }
                    1 => {
                        let mut label = sign_label(20);
                        label.x = x;
                        label.y = y;
                        source.labels.push(label);
                    }
                    2 => {
                        let mut shape = box_shape(21);
                        shape.x = x;
                        shape.y = y;
                        source.shapes.push(shape);
                    }
                    _ => {
                        add_exit(&mut source, 1, ExitDirection::North, None, 10);
                        source.connections[0].route_points = vec![MapPoint::new(x, y)];
                    }
                }
                let plan = plan(&into, &[(&source, shift(x, y, 0))], &[], 2);
                assert_numeric_refusal_unchanged(
                    &plan,
                    into,
                    vec![source],
                    vec![],
                    "merge_areas_invalid_translation",
                );
            }
        }
    }

    #[test]
    fn translated_level_overflow_is_refused_for_rooms_labels_and_shapes() {
        for (level, offset) in [(1, i32::MAX), (-1, i32::MIN)] {
            for kind in 0..3 {
                let into = with_rooms(area_id(1), &[1]);
                let mut source = with_rooms(area_id(2), &[1]);
                match kind {
                    0 => source.rooms[0].level = level,
                    1 => {
                        let mut label = sign_label(20);
                        label.level = level;
                        source.labels.push(label);
                    }
                    _ => {
                        let mut shape = box_shape(21);
                        shape.level = level;
                        source.shapes.push(shape);
                    }
                }
                let plan = plan(&into, &[(&source, shift(0.0, 0.0, offset))], &[], 2);
                assert_numeric_refusal_unchanged(
                    &plan,
                    into,
                    vec![source],
                    vec![],
                    "merge_areas_invalid_translation",
                );
            }
        }
    }

    #[test]
    fn partial_translation_checks_only_geometry_that_moves() {
        let mut into = with_rooms(area_id(1), &[1]);
        let mut source = with_rooms(area_id(2), &[1, 2]);
        source.rooms[1].level = i32::MAX;
        source.labels.push(sign_label(20));
        source.labels[0].level = i32::MAX;
        source.shapes.push(box_shape(21));
        source.shapes[0].level = i32::MAX;
        let plan = partial(
            plan(&into, &[(&source, shift(0.0, 0.0, 1))], &[], 2),
            0,
            &[1],
        );
        let mut sources = [source];
        apply_area_merge(&plan, &mut into, &mut sources, &mut []).expect("partial merge");
        assert_eq!(sources[0].rooms[0].level, i32::MAX);
        assert_eq!(sources[0].labels[0].level, i32::MAX);
        assert_eq!(sources[0].shapes[0].level, i32::MAX);
        assert_eq!(room_in(&into, 2).level, 1);
    }

    #[test]
    fn exhausted_number_cursor_still_allows_original_free_numbers() {
        let mut into = with_rooms(area_id(1), &[i32::MAX]);
        let source = with_rooms(area_id(2), &[1]);
        let plan = plan(&into, &[(&source, Translate::default())], &[], i32::MAX);
        let outcome = apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("free gap");
        assert_eq!(remap_of(&outcome, area_id(2)), vec![(1, 1)]);
    }

    #[test]
    fn allocating_the_last_number_does_not_reject_a_later_free_gap() {
        let mut into = with_rooms(area_id(1), &[1, i32::MAX - 1]);
        let source = with_rooms(area_id(2), &[1, 2]);
        let plan = plan(&into, &[(&source, Translate::default())], &[], i32::MAX);
        let outcome =
            apply_area_merge(&plan, &mut into, &mut [source], &mut []).expect("last number");
        assert_eq!(remap_of(&outcome, area_id(2)), vec![(1, i32::MAX), (2, 2)]);
    }

    #[test]
    fn number_exhaustion_is_refused_before_any_document_changes() {
        let into = with_rooms(area_id(1), &[1, 2, i32::MAX - 1]);
        let source = with_rooms(area_id(2), &[1, 2]);
        let plan = plan(&into, &[(&source, Translate::default())], &[], i32::MAX);
        assert_numeric_refusal_unchanged(
            &plan,
            into,
            vec![source],
            vec![],
            "merge_areas_room_numbers_exhausted",
        );
    }
}
