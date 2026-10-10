//! A litmus harness for `mapper.mergeAreas` on real map stores.
//!
//! `run` copies a local store's `areas-v2/` and `atlases/` into a temporary
//! directory, opens a real `LocalBackend` and a `Mapper` over the copy,
//! applies a list of merges through `Mapper::merge_areas`, and reports how
//! long each merge took and whether core kept every invariant below. It
//! holds no game knowledge, and no map data belongs in this repository: the
//! store, the plan and the output all live outside it.
//!
//! Run it against a store (PowerShell shown; any shell that sets the three
//! variables works, `RUST_LOG=warn` adds core's warnings):
//!
//! ```text
//! $env:SMUDGY_MERGE_LITMUS_STORE = "C:\maps\local"      # holds areas-v2\ and atlases\
//! $env:SMUDGY_MERGE_LITMUS_PLAN = "C:\maps\plan.json"
//! $env:SMUDGY_MERGE_LITMUS_OUT = "C:\maps\litmus-out"   # created if missing
//! cargo test -p smudgy_cloud --test merge_litmus -- --ignored --nocapture
//! ```
//!
//! The plan lists merges in the order they apply. A source without `rooms`
//! moves whole and is deleted; one with `rooms` gives only those rooms and
//! stays, exactly as in the script API. Omitted translation axes are zero.
//! Sources are parsed strictly, since a misspelled `rooms` would silently
//! move a whole area; merges and the plan itself may carry extra notes.
//!
//! ```json
//! { "merges": [ { "label": "fold the annex",
//!                 "into": "<area uuid>",
//!                 "sources": [ { "area": "<area uuid>" },
//!                              { "area": "<area uuid>", "rooms": [12, 13],
//!                                "translate": { "x": 4, "y": -2, "level": 1 } } ] } ] }
//! ```
//!
//! The output directory receives `results.json` (each merge's outcome,
//! error text and time; the totals before and after; captured exits; every
//! violation), `summary.txt`, and the merged store under `store/`,
//! replacing an earlier run's. A refused merge is data: it is recorded and
//! the run continues. The test fails only when an invariant is violated:
//!
//! - right after each merge, since a later merge may move the same rooms
//!   again: every whole source is gone; every partial source remains
//!   without the rooms it gave; every moved room sits in the destination
//!   under its remapped number with its content and exits intact at its
//!   translated position; a refused merge changed none of the areas it
//!   names; no journal is left under `transactions/`;
//! - after the last merge: the room count, and the room external ids and
//!   exit, label and shape ids as multisets, are unchanged; the areas are
//!   those before less the deleted sources; every exit that led to a room
//!   leads to that same room, following the remaps, and every exit that
//!   led nowhere still does; and the files read back as what the session
//!   holds.
//!
//! An exit that already named a missing room is the store's own damage and
//! violates nothing. When a moved room takes the number it names, it is
//! listed as captured: it now leads to a room it may never have meant.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fmt, fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Instant,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use smudgy_cloud::{
    AreaId, AreaMergeSource, AreaWithDetails, AtlasId, CloudResult, Connection, ConnectionId,
    CreateAreaRequest, Exit, ExitArgs, ExitDirection, ExitId, Label, LabelArgs, LocalBackend,
    Mapper, MapperBackend, Property, RoomNumber, RoomRemap, RoomUpdates, RoomWithDetails, Shape,
    ShapeArgs, Translate,
    mapper::{RoomKey, area_cache::AreaCache, room_cache::RoomCache},
    mutation::{AreaMutation, MutationEnvelope, Precondition},
};
use uuid::Uuid;

/// The parts of a store the harness copies. Anything else in a store
/// directory, such as queued cloud mutations, stays behind.
const STORE_DIRS: [&str; 2] = ["areas-v2", "atlases"];

// === The plan ===

/// The merges to apply, in order; the JSON shape is in the module docs.
#[derive(Debug, Deserialize)]
struct Plan {
    merges: Vec<PlannedMerge>,
}

#[derive(Debug, Deserialize)]
struct PlannedMerge {
    label: String,
    into: AreaId,
    sources: Vec<PlannedSource>,
}

/// One source as the script API spells it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PlannedSource {
    area: AreaId,
    #[serde(default)]
    rooms: Option<Vec<RoomNumber>>,
    #[serde(default)]
    translate: PlannedTranslate,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct PlannedTranslate {
    x: f32,
    y: f32,
    level: i32,
}

impl PlannedSource {
    fn to_source(&self) -> AreaMergeSource {
        let PlannedTranslate { x, y, level } = self.translate;
        AreaMergeSource {
            id: self.area,
            translate: Translate { x, y, level },
            rooms: self.rooms.clone(),
        }
    }
}

// === The report ===

/// Everything `results.json` holds.
#[derive(Debug, Default, Serialize)]
struct Report {
    /// The `*.json` files in the store's `areas-v2/`; more than
    /// `before.areas` means some could not be read.
    area_files: usize,
    before: Totals,
    after: Totals,
    merges: Vec<MergeResult>,
    /// Exits that named a missing room before the run and name a moved
    /// room after it.
    captured_exits: Vec<String>,
    violations: Vec<String>,
}

/// One merge's outcome, in plan order.
#[derive(Debug, Serialize)]
struct MergeResult {
    label: String,
    ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    millis: f64,
    whole_sources: usize,
    partial_sources: usize,
    rooms_moved: usize,
}

/// Store-wide counts. Connections are reported, not conserved: a merge
/// pairs the two halves of a link that becomes internal and splits a pair
/// that a partial merge cuts.
#[derive(Debug, Default, Serialize)]
struct Totals {
    areas: usize,
    rooms: usize,
    exits: usize,
    /// Exits that name a room that does not exist.
    dangling_exits: usize,
    connections: usize,
    labels: usize,
    shapes: usize,
}

impl Totals {
    fn of(areas: &Areas) -> Self {
        let sum = |count: fn(&Content) -> usize| areas.values().map(count).sum::<usize>();
        Self {
            areas: areas.len(),
            rooms: sum(|area| area.rooms.len()),
            exits: sum(|area| area.exits().count()),
            dangling_exits: targets(areas)
                .values()
                .filter(|(_, target)| *target == Target::Missing)
                .count(),
            connections: sum(|area| area.connections.len()),
            labels: sum(|area| area.labels.len()),
            shapes: sum(|area| area.shapes.len()),
        }
    }
}

// === Area content ===

/// One area's content in a canonical order (rooms by number; exits,
/// connections, labels and shapes by id; properties by name), so what the
/// session holds and what the store reads back compare through their JSON.
/// The revision is left out because the session's copy is a render counter
/// that may run ahead of the stored one. The rest of the area header
/// (creation time, owner, access) is left out too: it is not map content
/// and the session does not keep all of it.
#[derive(Serialize)]
struct Content {
    name: String,
    atlas_id: Option<AtlasId>,
    properties: Vec<Property>,
    rooms: Vec<RoomWithDetails>,
    connections: Vec<Connection>,
    labels: Vec<Label>,
    shapes: Vec<Shape>,
}

/// Areas by id, in id order so reports read the same from run to run.
type Areas = BTreeMap<Uuid, Content>;

impl Content {
    fn stored(area: &AreaWithDetails) -> Self {
        Self {
            name: area.area.name.clone(),
            atlas_id: area.area.atlas_id,
            properties: area.properties.clone(),
            rooms: area.rooms.clone(),
            connections: area.connections.clone(),
            labels: area.labels.clone(),
            shapes: area.shapes.clone(),
        }
        .canonical()
    }

    /// Rebuilds the document field for field from the cache the session
    /// renders and routes from.
    fn cached(area: &AreaCache) -> Self {
        Self {
            name: area.get_name().to_owned(),
            atlas_id: area.meta().atlas_id,
            properties: area.properties().map(property).collect(),
            rooms: area
                .get_rooms()
                .iter()
                .map(|room| cached_room(room))
                .collect(),
            connections: area.get_connections().to_vec(),
            labels: area.get_labels().to_vec(),
            shapes: area.get_shapes().to_vec(),
        }
        .canonical()
    }

    fn canonical(mut self) -> Self {
        self.properties.sort_by(|a, b| a.name.cmp(&b.name));
        self.rooms.sort_by_key(|room| room.room_number);
        for room in &mut self.rooms {
            room.properties.sort_by(|a, b| a.name.cmp(&b.name));
            room.exits.sort_by_key(|exit| exit.id.0);
        }
        self.connections.sort_by_key(|connection| connection.id);
        self.labels.sort_by_key(|label| label.id.0);
        self.shapes.sort_by_key(|shape| shape.id.0);
        self
    }

    fn room(&self, number: RoomNumber) -> Option<&RoomWithDetails> {
        let index = self
            .rooms
            .binary_search_by_key(&number, |room| room.room_number)
            .ok()?;
        Some(&self.rooms[index])
    }

    fn exits(&self) -> impl Iterator<Item = (&RoomWithDetails, &Exit)> {
        self.rooms
            .iter()
            .flat_map(|room| room.exits.iter().map(move |exit| (room, exit)))
    }
}

fn property((name, value): (&str, &str)) -> Property {
    Property {
        name: name.to_owned(),
        value: value.to_owned(),
    }
}

fn cached_room(room: &RoomCache) -> RoomWithDetails {
    let exits = room.get_exits().iter().map(|exit| Exit {
        to_source: None,
        id: exit.id,
        from_direction: exit.from_direction,
        to_area_id: exit.to_area_id,
        to_room_number: exit.to_room_number,
        to_direction: exit.to_direction,
        path: exit.path.clone().unwrap_or_default(),
        is_hidden: exit.is_hidden,
        door: exit.door.clone(),
        weight: exit.weight,
        command: exit.command.clone().unwrap_or_default(),
        connection_id: exit.connection_id,
        to_unknown: exit.to_unknown,
        to_area_token: exit.to_area_token.clone(),
    });
    RoomWithDetails {
        room_number: room.get_room_number(),
        title: room.get_title().to_owned(),
        description: room.get_description().to_owned(),
        level: room.get_level(),
        x: room.get_x(),
        y: room.get_y(),
        color: room.get_color().to_owned(),
        properties: room.properties().map(property).collect(),
        exits: exits.collect(),
        tags: room.get_tags().clone(),
        external_id: room.get_external_id().map(str::to_owned),
    }
}

/// The session's areas that `keep` selects.
fn session_areas(mapper: &Mapper, keep: impl Fn(AreaId) -> bool) -> Areas {
    mapper
        .get_current_atlas()
        .areas()
        .filter(|area| keep(*area.get_id()))
        .map(|area| (area.get_id().0, Content::cached(&area)))
        .collect()
}

// === The run ===

/// Applies `plan` to a copy of the store at `store`, writes the report and
/// the merged store to `out`, and returns the report.
async fn run(store: &Path, plan: &Plan, out: &Path) -> Report {
    assert!(
        store.join("areas-v2").is_dir(),
        "{} holds no areas-v2 directory",
        store.display()
    );
    let scratch = Scratch::new("run");
    let root = scratch.0.join("store");
    let area_files = copy_store(store, &root);
    let mapper = Mapper::new(Arc::new(LocalBackend::new(&root)), scratch.0.join("cache"));
    mapper.ready().await.expect("load the copied store");

    let before = session_areas(&mapper, |_| true);
    let mut report = Report {
        area_files,
        before: Totals::of(&before),
        ..Report::default()
    };
    let mut ledger = Ledger::default();
    for merge in &plan.merges {
        let result = apply_merge(&mapper, &root, merge, &mut ledger, &mut report.violations).await;
        report.merges.push(result);
    }
    let after = session_areas(&mapper, |_| true);
    report.after = Totals::of(&after);
    check_conservation(&before, &after, &ledger, &mut report.violations);
    report.captured_exits = check_exit_targets(&before, &after, &ledger, &mut report.violations);
    drop(mapper);
    check_reload(&root, &after, &mut report.violations).await;
    write_outputs(&root, out, &report);
    report
}

/// Applies one merge, times it, and checks the areas it names.
async fn apply_merge(
    mapper: &Mapper,
    root: &Path,
    merge: &PlannedMerge,
    ledger: &mut Ledger,
    violations: &mut Vec<String>,
) -> MergeResult {
    let sources: Vec<AreaMergeSource> =
        merge.sources.iter().map(PlannedSource::to_source).collect();
    let named: HashSet<AreaId> = sources
        .iter()
        .map(|source| source.id)
        .chain([merge.into])
        .collect();
    let before = session_areas(mapper, |id| named.contains(&id));
    let started = Instant::now();
    let outcome = mapper.merge_areas(merge.into, sources.clone()).await;
    let millis = started.elapsed().as_secs_f64() * 1e3;
    let after = session_areas(mapper, |id| named.contains(&id));

    let mut found = Vec::new();
    match &outcome {
        Ok(commit) => {
            let remap = &commit.outcome.rooms;
            check_merge(merge.into, &sources, remap, &before, &after, &mut found);
            ledger.record(merge.into, &sources, remap);
        }
        Err(_) => {
            for (id, area) in &before {
                if after.get(id).map(|now| json!(now)) != Some(json!(area)) {
                    found.push(format!("the refused merge changed area {id}"));
                }
            }
        }
    }
    for journal in journals(root) {
        found.push(format!("journal {journal} was left in transactions/"));
    }
    for finding in found {
        violations.push(format!("{}: {finding}", merge.label));
    }

    let partial = sources.iter().filter(|source| source.is_partial()).count();
    MergeResult {
        label: merge.label.clone(),
        ok: outcome.is_ok(),
        error: outcome.as_ref().err().map(ToString::to_string),
        millis,
        whole_sources: sources.len() - partial,
        partial_sources: partial,
        rooms_moved: outcome.map_or(0, |commit| commit.outcome.rooms.len()),
    }
}

/// What the committed merges did, for the checks after the last one.
#[derive(Default)]
struct Ledger {
    /// Where each moved room sat before the run, by where it sits now.
    origins: HashMap<RoomKey, RoomKey>,
    /// Every whole source a committed merge deleted.
    deleted: HashSet<AreaId>,
}

impl Ledger {
    fn record(&mut self, into: AreaId, sources: &[AreaMergeSource], remap: &[RoomRemap]) {
        let whole = sources.iter().filter(|source| !source.is_partial());
        self.deleted.extend(whole.map(|source| source.id));
        for moved in remap {
            let origin = self
                .origins
                .remove(&moved.from)
                .unwrap_or_else(|| moved.from.clone());
            self.origins.insert(RoomKey::new(into, moved.to), origin);
        }
    }

    /// Where the room now at `key` sat before the run.
    fn origin(&self, key: &RoomKey) -> RoomKey {
        self.origins.get(key).unwrap_or(key).clone()
    }
}

// === Checks ===

/// Checks a committed merge against the areas it names, as they stood just
/// before it and just after.
fn check_merge(
    into: AreaId,
    sources: &[AreaMergeSource],
    remap: &[RoomRemap],
    before: &Areas,
    after: &Areas,
    found: &mut Vec<String>,
) {
    let mut expected_moves = 0;
    for source in sources {
        let (id, remaining) = (source.id, after.get(&source.id.0));
        match (&source.rooms, remaining) {
            (None, Some(_)) => found.push(format!("whole source {id} still exists")),
            (Some(_), None) => found.push(format!("partial source {id} is gone")),
            (Some(rooms), Some(remaining)) => {
                for number in rooms
                    .iter()
                    .filter(|&&number| remaining.room(number).is_some())
                {
                    found.push(format!("partial source {id} still holds room {number}"));
                }
            }
            (None, None) => {}
        }
        expected_moves += match &source.rooms {
            Some(rooms) => rooms.iter().collect::<HashSet<_>>().len(),
            None => before.get(&id.0).map_or(0, |area| area.rooms.len()),
        };
    }
    if remap.len() != expected_moves {
        found.push(format!(
            "{} rooms moved, {expected_moves} expected",
            remap.len()
        ));
    }
    for moved in remap {
        let from = &moved.from;
        let source = sources.iter().find(|source| source.id == from.area_id);
        let old = before
            .get(&from.area_id.0)
            .and_then(|area| area.room(from.room_number));
        let new = after.get(&into.0).and_then(|area| area.room(moved.to));
        let arrived = match (source, old, new) {
            (Some(source), Some(old), Some(new)) => {
                let t = source.translate;
                let expected = RoomWithDetails {
                    x: old.x + t.x,
                    y: old.y + t.y,
                    level: old.level.wrapping_add(t.level),
                    ..old.clone()
                };
                fingerprint(&expected) == fingerprint(new)
            }
            _ => false,
        };
        if !arrived {
            let to = RoomKey::new(into, moved.to);
            found.push(format!(
                "room {} did not arrive intact as {}",
                place(from),
                place(&to)
            ));
        }
    }
}

/// A room without the parts a merge rewrites: its number and its exits'
/// targets and connections. Everything else must arrive unchanged.
fn fingerprint(room: &RoomWithDetails) -> Value {
    let mut room = room.clone();
    room.room_number = RoomNumber(0);
    for exit in &mut room.exits {
        exit.to_area_id = None;
        exit.to_room_number = None;
        exit.connection_id = ConnectionId::default();
    }
    json!(room)
}

/// Checks what no sequence of merges may change, from the states before
/// the first merge and after the last.
fn check_conservation(before: &Areas, after: &Areas, ledger: &Ledger, found: &mut Vec<String>) {
    let rooms = |areas: &Areas| areas.values().map(|area| area.rooms.len()).sum::<usize>();
    if rooms(before) != rooms(after) {
        found.push(format!(
            "the store held {} rooms and holds {}",
            rooms(before),
            rooms(after)
        ));
    }
    for ((what, was), (_, now)) in identities(before).into_iter().zip(identities(after)) {
        let (lost, gained) = (surplus(&was, &now), surplus(&now, &was));
        if !lost.is_empty() || !gained.is_empty() {
            found.push(format!(
                "{what} changed: {} lost{}, {} gained{}",
                lost.len(),
                examples(&lost),
                gained.len(),
                examples(&gained)
            ));
        }
    }
    let expected: BTreeSet<&Uuid> = before
        .keys()
        .filter(|id| !ledger.deleted.contains(&AreaId(**id)))
        .collect();
    let present: BTreeSet<&Uuid> = after.keys().collect();
    for id in expected.symmetric_difference(&present) {
        let fate = if present.contains(id) {
            "appeared"
        } else {
            "vanished"
        };
        found.push(format!("area {id} {fate} without being a source"));
    }
}

/// The identities no merge may add or lose, each as a multiset.
fn identities(areas: &Areas) -> [(&'static str, BTreeMap<String, usize>); 4] {
    let rooms = || areas.values().flat_map(|area| &area.rooms);
    let external_ids = rooms().filter_map(|room| room.external_id.clone());
    let exits = rooms().flat_map(|room| &room.exits);
    let labels = areas.values().flat_map(|area| &area.labels);
    let shapes = areas.values().flat_map(|area| &area.shapes);
    [
        ("room external ids", tally(external_ids)),
        ("exit ids", tally(exits.map(|exit| exit.id.to_string()))),
        ("label ids", tally(labels.map(|label| label.id.to_string()))),
        ("shape ids", tally(shapes.map(|shape| shape.id.to_string()))),
    ]
}

fn tally(ids: impl Iterator<Item = String>) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for id in ids {
        *counts.entry(id).or_insert(0) += 1;
    }
    counts
}

/// The ids `a` holds more often than `b`.
fn surplus<'a>(a: &'a BTreeMap<String, usize>, b: &BTreeMap<String, usize>) -> Vec<&'a String> {
    let fewer_in_b = |(id, count): &(&String, &usize)| b.get(*id).unwrap_or(&0) < *count;
    a.iter().filter(fewer_in_b).map(|(id, _)| id).collect()
}

fn examples(ids: &[&String]) -> String {
    if ids.is_empty() {
        return String::new();
    }
    let shown: Vec<&str> = ids.iter().take(3).map(|id| id.as_str()).collect();
    format!(" (e.g. {})", shown.join(", "))
}

/// Where an exit leads.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    /// It names no destination.
    Nowhere,
    Room(RoomKey),
    /// It names a room that does not exist.
    Missing,
}

impl fmt::Display for Target {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Nowhere => f.write_str("nowhere"),
            Self::Room(key) => f.write_str(&place(key)),
            Self::Missing => f.write_str("a missing room"),
        }
    }
}

fn place(key: &RoomKey) -> String {
    format!("{}/{}", key.area_id, key.room_number)
}

/// Every exit's target by exit id, with the room that holds the exit.
fn targets(areas: &Areas) -> HashMap<ExitId, (RoomKey, Target)> {
    let key = |area: &Uuid, number| RoomKey::new(AreaId(*area), number);
    let rooms: HashSet<RoomKey> = areas
        .iter()
        .flat_map(|(id, area)| area.rooms.iter().map(|room| key(id, room.room_number)))
        .collect();
    let mut targets = HashMap::new();
    for (id, area) in areas {
        for (room, exit) in area.exits() {
            let target = match (exit.to_area_id, exit.to_room_number) {
                (None, None) => Target::Nowhere,
                (Some(to), Some(number)) if rooms.contains(&key(&to.0, number)) => {
                    Target::Room(key(&to.0, number))
                }
                _ => Target::Missing,
            };
            targets.insert(exit.id, (key(id, room.room_number), target));
        }
    }
    targets
}

/// Every exit that led to a room must lead to the same room, following the
/// merges' remaps, and every exit that led nowhere or to a missing room
/// must still, except that a moved room may take the number a missing one
/// had. Returns the exits captured that way.
fn check_exit_targets(
    before: &Areas,
    after: &Areas,
    ledger: &Ledger,
    found: &mut Vec<String>,
) -> Vec<String> {
    let was = targets(before);
    let mut now: Vec<_> = targets(after).into_iter().collect();
    now.sort_by_key(|(id, _)| id.0);
    let mut captured = Vec::new();
    for (id, (holder, target)) in now {
        // An exit missing from either side is reported by the id tallies.
        let Some((_, old)) = was.get(&id) else {
            continue;
        };
        let exit = || format!("exit {id} of room {}", place(&holder));
        match (old, &target) {
            (Target::Nowhere, Target::Nowhere) | (Target::Missing, Target::Missing) => {}
            (Target::Room(old), Target::Room(new)) if ledger.origin(new) == *old => {}
            (Target::Missing, Target::Room(new)) => captured.push(format!(
                "{} named a missing room and now leads to {target}, which was {} before the run",
                exit(),
                place(&ledger.origin(new))
            )),
            _ => found.push(format!("{} led to {old} and now leads to {target}", exit())),
        }
    }
    captured
}

/// Reads every area of the store at `root` from its files.
async fn stored_areas(root: &Path) -> CloudResult<Areas> {
    // Sessions that mount one directory share one in-memory store, so a
    // second backend on the root would see the live generation; `refresh`
    // makes it read every file again, as a restart would.
    let backend = LocalBackend::new(root);
    backend.refresh().await?;
    let snapshot = backend.snapshot().await?;
    let areas = snapshot
        .areas()
        .map(|area| (area.area.id.0, Content::stored(area)));
    Ok(areas.collect())
}

/// Re-reads the store from disk and compares every area with what the
/// session held after the last merge.
async fn check_reload(root: &Path, session: &Areas, found: &mut Vec<String>) {
    let stored = match stored_areas(root).await {
        Ok(stored) => stored,
        Err(error) => {
            found.push(format!("the merged store could not be read back: {error}"));
            return;
        }
    };
    let ids: BTreeSet<&Uuid> = session.keys().chain(stored.keys()).collect();
    for id in ids {
        let (Some(held), Some(read)) = (session.get(id), stored.get(id)) else {
            let fate = if session.contains_key(id) {
                "is missing"
            } else {
                "reappears"
            };
            found.push(format!("area {id} {fate} when the store is read back"));
            continue;
        };
        let (held, read) = (json!(held), json!(read));
        for (field, value) in held.as_object().into_iter().flatten() {
            if read.get(field) != Some(value) {
                let at = first_difference(value, &read[field]);
                found.push(format!("area {id} reads back with different {field}{at}"));
            }
        }
    }
}

/// Where two canonical arrays first part, to point a reader at it.
fn first_difference(held: &Value, read: &Value) -> String {
    let (Some(held), Some(read)) = (held.as_array(), read.as_array()) else {
        return String::new();
    };
    if held.len() != read.len() {
        return format!(" ({} held, {} read back)", held.len(), read.len());
    }
    held.iter()
        .zip(read)
        .find(|(a, b)| a != b)
        .and_then(|(entry, _)| {
            ["room_number", "id", "name"]
                .into_iter()
                .find_map(|key| entry.get(key))
        })
        .map_or_else(String::new, |key| format!(" (first at {key})"))
}

/// Whatever the local store left in its journal directory.
fn journals(root: &Path) -> Vec<String> {
    let Ok(entries) = fs::read_dir(root.join("transactions")) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect()
}

// === Files ===

/// Copies the store directories from `from` into `to` and returns how many
/// area files there were. A store without atlases is fine.
fn copy_store(from: &Path, to: &Path) -> usize {
    let mut area_files = 0;
    for dir in STORE_DIRS {
        let target = to.join(dir);
        fs::create_dir_all(&target).expect("create the store copy");
        let Ok(entries) = fs::read_dir(from.join(dir)) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("list the store").path();
            if path.is_file() {
                fs::copy(&path, target.join(path.file_name().expect("a file name")))
                    .expect("copy a store file");
                if dir == "areas-v2" && path.extension().is_some_and(|ext| ext == "json") {
                    area_files += 1;
                }
            }
        }
    }
    area_files
}

/// Writes `results.json`, `summary.txt` and the merged store under `store/`.
fn write_outputs(root: &Path, out: &Path, report: &Report) {
    let store = out.join("store");
    if store.exists() {
        fs::remove_dir_all(&store).expect("clear the previous run's store");
    }
    copy_store(root, &store);
    let results = serde_json::to_vec_pretty(report).expect("serialize the report");
    fs::write(out.join("results.json"), results).expect("write results.json");
    fs::write(out.join("summary.txt"), summary(report)).expect("write summary.txt");
}

/// The report for a reader.
fn summary(report: &Report) -> String {
    let (b, a) = (&report.before, &report.after);
    let totals = [
        ("areas", b.areas, a.areas),
        ("rooms", b.rooms, a.rooms),
        ("exits", b.exits, a.exits),
        ("dangling exits", b.dangling_exits, a.dangling_exits),
        ("connections", b.connections, a.connections),
        ("labels", b.labels, a.labels),
        ("shapes", b.shapes, a.shapes),
    ];
    let committed = report.merges.iter().filter(|merge| merge.ok).count();
    let refused = report.merges.len() - committed;
    let mut lines = vec![
        format!("{committed} merges committed, {refused} refused"),
        format!("{} area files, {} areas loaded", report.area_files, b.areas),
    ];
    lines.extend(totals.map(|(what, b, a)| format!("{what}: {b} -> {a}")));
    lines.push(String::new());
    lines.extend(report.merges.iter().map(|merge| {
        let outcome = if merge.ok {
            format!(
                "{} whole, {} partial, {} rooms moved",
                merge.whole_sources, merge.partial_sources, merge.rooms_moved
            )
        } else {
            format!("refused: {}", merge.error.as_deref().unwrap_or_default())
        };
        format!("{:>10.1} ms  {}: {outcome}", merge.millis, merge.label)
    }));
    let lists = [
        ("exits captured by a moved room", &report.captured_exits),
        ("invariant violations", &report.violations),
    ];
    for (what, items) in lists {
        lines.push(format!("\n{} {what}", items.len()));
        lines.extend(items.iter().take(50).map(|item| format!("  {item}")));
        if items.len() > 50 {
            lines.push("  (the rest are in results.json)".to_owned());
        }
    }
    lines.join("\n") + "\n"
}

/// A directory under the system temp directory, removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> Self {
        let path =
            std::env::temp_dir().join(format!("smudgy-merge-litmus-{label}-{}", Uuid::new_v4()));
        fs::create_dir_all(&path).expect("create a scratch directory");
        Self(path)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

// === Tests ===

/// Runs the plan in `SMUDGY_MERGE_LITMUS_PLAN` against the store in
/// `SMUDGY_MERGE_LITMUS_STORE` and writes to `SMUDGY_MERGE_LITMUS_OUT`.
#[tokio::test]
#[ignore = "needs a map store and a plan; see the module docs"]
async fn merge_litmus() {
    let _ = pretty_env_logger::try_init();
    let path = |name: &str| {
        PathBuf::from(
            std::env::var_os(name).unwrap_or_else(|| panic!("set {name}; see the module docs")),
        )
    };
    let (store, out) = (
        path("SMUDGY_MERGE_LITMUS_STORE"),
        path("SMUDGY_MERGE_LITMUS_OUT"),
    );
    let plan = fs::read(path("SMUDGY_MERGE_LITMUS_PLAN")).expect("read the plan");
    let plan: Plan = serde_json::from_slice(&plan).expect("parse the plan");

    let report = run(&store, &plan, &out).await;

    print!("{}", summary(&report));
    assert!(
        report.violations.is_empty(),
        "{} invariant violations; see {}",
        report.violations.len(),
        out.join("results.json").display()
    );
}

/// The harness over a store built here: Alpha and Beta, joined by one link
/// from Alpha 2 to Beta 1, and Gamma, whose dangling exit names a missing
/// Alpha 3. Beta folds into Alpha whole and skips that number; a merge of
/// Alpha into itself is refused; Gamma 2 moves into Alpha with a
/// translation. The checks must pass, and must fire on a lost room.
#[tokio::test]
async fn merge_litmus_passes_a_synthetic_store_and_flags_a_lost_room() {
    let (source, out) = (Scratch::new("source"), Scratch::new("out"));
    let [alpha, beta, gamma] = synthetic_store(&source.0).await;
    let plan: Plan = serde_json::from_value(json!({ "merges": [
        { "label": "whole", "into": alpha, "sources": [{ "area": beta }] },
        { "label": "refused", "into": alpha, "sources": [{ "area": alpha }] },
        { "label": "partial", "into": alpha, "sources": [{
            "area": gamma, "rooms": [2], "translate": { "x": 10, "y": -5, "level": 1 },
        }] },
    ] }))
    .expect("the plan parses");

    let report = run(&source.0, &plan, &out.0).await;

    assert!(report.violations.is_empty(), "{:#?}", report.violations);
    let outcome = |m: &MergeResult| (m.ok, m.whole_sources, m.partial_sources, m.rooms_moved);
    let outcomes: Vec<_> = report.merges.iter().map(outcome).collect();
    assert_eq!(
        outcomes,
        [(true, 1, 0, 2), (false, 1, 0, 0), (true, 0, 1, 1)]
    );
    let refusal = report.merges[1].error.as_deref().unwrap_or_default();
    assert!(refusal.contains("merge_areas_same_area"), "{refusal}");
    let (before, after) = (&report.before, &report.after);
    assert_eq!((before.areas, after.areas), (3, 2));
    assert_eq!((before.rooms, after.rooms), (6, 6));
    assert_eq!((before.exits, after.exits), (9, 9));
    // Beta 1 lands as Alpha 4: Gamma 1's dangling exit still names Alpha 3,
    // so no moved room takes that number and the exit keeps leading nowhere.
    assert_eq!((before.dangling_exits, after.dangling_exits), (1, 1));
    assert!(
        report.captured_exits.is_empty(),
        "{:#?}",
        report.captured_exits
    );
    let results = fs::read(out.0.join("results.json")).expect("results.json");
    let results: Value = serde_json::from_slice(&results).expect("results.json parses");
    assert_eq!(results["merges"].as_array().map(Vec::len), Some(3));
    assert!(out.0.join("summary.txt").is_file());
    let merged = out.0.join("store");
    let merged_areas = fs::read_dir(merged.join("areas-v2")).expect("the merged store");
    assert_eq!(merged_areas.count(), 2);

    // The same store with one room dropped must not pass.
    let intact = stored_areas(&merged).await.expect("read the merged store");
    let mut damaged = stored_areas(&merged).await.expect("read the merged store");
    damaged.get_mut(&alpha.0).expect("alpha").rooms.pop();
    let mut found = Vec::new();
    check_conservation(&intact, &damaged, &Ledger::default(), &mut found);
    assert!(
        found.iter().any(|finding| finding.contains("rooms")),
        "{found:#?}"
    );
}

/// Writes the synthetic store through a real `LocalBackend`, so every
/// connection is the one the appliers make: three areas in one atlas, each
/// with two paired rooms, Alpha 2 linked east to Beta 1 and back, a
/// dangling exit from Gamma 1 into Alpha, a label on Beta, a shape on
/// Gamma, and properties and a tag to carry.
async fn synthetic_store(root: &Path) -> [AreaId; 3] {
    use ExitDirection::{East, North, South, West};

    let backend = LocalBackend::new(root);
    let atlas = backend
        .create_atlas("Synthetic")
        .await
        .expect("create the atlas");
    let mut ids = Vec::new();
    for name in ["Alpha", "Beta", "Gamma"] {
        let request = CreateAreaRequest {
            name: name.to_owned(),
            atlas_id: Some(atlas.id),
            clan_id: None,
            ownership: None,
            ephemeral: false,
            properties: BTreeMap::new(),
        };
        let area = backend.create_area(request).await.expect("create an area");
        ids.push(area.id);
    }
    let [alpha, beta, gamma] = [ids[0], ids[1], ids[2]];
    let alpha_edits = vec![
        room(1, 0.0, 0.0, "a1"),
        room(2, 1.0, 0.0, "a2"),
        exit(1, East, (alpha, 2), West),
        exit(2, West, (alpha, 1), East),
        exit(2, East, (beta, 1), West),
        AreaMutation::UpsertAreaProperty {
            name: "kind".to_owned(),
            value: "synthetic".to_owned(),
        },
    ];
    let beta_edits = vec![
        room(1, 0.0, 0.0, "b1"),
        room(2, 1.0, 0.0, "b2"),
        exit(1, East, (beta, 2), West),
        exit(2, West, (beta, 1), East),
        exit(1, West, (alpha, 2), East),
        AreaMutation::UpsertRoomProperty {
            room_source: None,
            room_number: RoomNumber(2),
            name: "shop".to_owned(),
            value: "yes".to_owned(),
        },
        AreaMutation::AddRoomTag {
            room_source: None,
            room_number: RoomNumber(2),
            tag: "market".to_owned(),
        },
        label("Beta"),
    ];
    let gamma_edits = vec![
        room(1, 0.0, 0.0, "g1"),
        room(2, 0.0, -1.0, "g2"),
        exit(1, North, (gamma, 2), South),
        exit(2, South, (gamma, 1), North),
        // Alpha has no room 3: this exit dangles.
        exit(1, West, (alpha, 3), East),
        shape(),
    ];
    for (area, edits) in [
        (alpha, alpha_edits),
        (beta, beta_edits),
        (gamma, gamma_edits),
    ] {
        let current = backend.get_area(&area).await.expect("read the area");
        let envelope = MutationEnvelope {
            source: smudgy_cloud::SourceId::map(),
            operation_id: Uuid::new_v4(),
            preconditions: vec![Precondition::source(
                area.0,
                smudgy_cloud::SourceId::map(),
                current.area.rev,
            )],
            payload: edits,
        };
        let result = backend.execute_mutation(&area, &envelope).await;
        result.expect("edit the synthetic store");
    }
    [alpha, beta, gamma]
}

fn room(number: i32, x: f32, y: f32, external_id: &str) -> AreaMutation {
    AreaMutation::CreateRoom {
        room_source: None,
        room_number: RoomNumber(number),
        body: RoomUpdates {
            title: Some(external_id.to_uppercase()),
            x: Some(x),
            y: Some(y),
            level: Some(0),
            external_id: Some(Some(external_id.to_owned())),
            ..RoomUpdates::default()
        },
    }
}

fn exit(
    from: i32,
    direction: ExitDirection,
    (area, number): (AreaId, i32),
    back: ExitDirection,
) -> AreaMutation {
    AreaMutation::CreateExit {
        room_source: None,
        room_number: RoomNumber(from),
        body: ExitArgs {
            from_direction: direction,
            to_area_id: Some(area),
            to_room_number: Some(RoomNumber(number)),
            to_direction: Some(back),
            weight: 1.0,
            ..ExitArgs::default()
        },
    }
}

fn label(text: &str) -> AreaMutation {
    AreaMutation::CreateLabel {
        body: LabelArgs {
            text: text.to_owned(),
            y: -1.0,
            width: 40.0,
            height: 10.0,
            color: "#ffffff".to_owned(),
            font_size: 12,
            font_weight: 400,
            ..LabelArgs::default()
        },
    }
}

fn shape() -> AreaMutation {
    AreaMutation::CreateShape {
        body: ShapeArgs {
            x: -1.0,
            y: -2.0,
            width: 3.0,
            height: 3.0,
            stroke_color: Some("#888888".to_owned()),
            ..ShapeArgs::default()
        },
    }
}
