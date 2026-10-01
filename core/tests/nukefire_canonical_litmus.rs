//! The replay litmus for the `nukefire-mapper` package's canonical areas on a
//! real local map store.
//!
//! It copies the store's `areas-v2/` and `atlases/` into a session's local map
//! directory, starts a session with the real mapper and map-layout packages,
//! and stands the player once in every zone whose area name the store
//! reveals, one zone at a time, waiting for each visit to be mapped before the
//! next. Then it lets the mapper go quiet, stops the session, and writes the
//! resulting store and a report. The store itself is only read, to copy it;
//! the session's copy is removed at the end, and nothing is written inside a
//! repository.
//!
//! Run it against a store (PowerShell shown; any shell that sets the two
//! variables works). Visit times represent a shipped build only under
//! `--release`:
//!
//! ```text
//! $env:SMUDGY_NUKEFIRE_LITMUS_STORE = "C:\maps\local"   # holds areas-v2\ and atlases\
//! $env:SMUDGY_NUKEFIRE_LITMUS_OUT = "C:\Temp\litmus"    # created if missing
//! cargo test --release -p smudgy_core --test nukefire_canonical_litmus -- --ignored --nocapture
//! ```
//!
//! A zone's area name is what the game would report while the player stands
//! in it, read from the maps the mapper made (those carrying
//! `nukefire.mapper`), trying in order:
//!
//! 1. the name of a map made for the zone: not a `NukeFire Zone N`
//!    placeholder, and carrying the zone as its `nukefire.zone`;
//! 2. a non-empty `rawName` for the zone in a map's `nukefire.zones` list,
//!    from the maps holding most of the zone's rooms first;
//! 3. the name of a map that is no placeholder, carries no `nukefire.zone`,
//!    and holds rooms of this zone alone.
//!
//! Among several candidates the map with the most rooms wins, then the lowest
//! id. The harness holds no other knowledge of the game. Zones are visited in
//! numeric order.
//!
//! A visit is a `Room.Info` naming the area, then a one-room
//! `NukeFire.Map.Local` for one of the zone's rooms: the lowest VNUM the
//! store holds exactly once, with the room's stored title, terrain,
//! position and zone. The visit is over when the mapper's decision log
//! records the pass that maps that room, so the mapper runs with its
//! decision log on.
//!
//! The output directory receives `results.json` (every visit with its
//! outcome, time and notices; the store's totals before and after; what the
//! canonical-areas rules leave unmet; violations), `summary.txt`, the
//! mapper's `mapping-decisions.jsonl`, and the resulting store under
//! `store/`, replacing an earlier run's. The test fails only on a violation:
//! a room lost other than by joining a duplicate VNUM, a new exit to a
//! missing room, or a journal left behind.

mod support;

use std::{
    cmp::Ordering,
    collections::{BTreeMap, BTreeSet, HashMap, HashSet},
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use chrono::{DateTime, Utc};
use futures::StreamExt;
use serde::Serialize;
use serde_json::{Value, json};
use smudgy_cloud::{AreaId, AreaWithDetails, Mapper, Property, RoomNumber, RoomWithDetails};
use smudgy_core::models::shared_packages;
use smudgy_core::session::SessionEvent;
use support::nukefire::{
    ChartRoom, MAPPER_SPEC, NOTICE_PREFIX, Session, collect, find_file, install_mapper_packages,
    local_mapper, smudgy_home, start_session, wait_until_within,
};

const SERVER: &str = "tdome.nukefire.org";
/// The parts of a store the harness copies.
const STORE_DIRS: [&str; 2] = ["areas-v2", "atlases"];
/// On a room: its zone. On a map an older version made: the zone it was made for.
const ZONE: &str = "nukefire.zone";
/// On a map an unreleased version made: a JSON list of `{ zone, rawName }`.
const ZONES: &str = "nukefire.zones";
/// On a map: present on every map the mapper made.
const MANAGED: &str = "nukefire.mapper";
/// On a map: the folded area name it is the map for.
const AREA_KEY: &str = "nukefire.area";
/// On a room: the terrain its chart reported.
const TERRAIN: &str = "terrain";
const PLACEHOLDER_PREFIX: &str = "NukeFire Zone ";
/// The decision the mapper logs once it has mapped a visit's room.
const APPLY_PREFIX: &str = "Apply NukeFire rooms for ";
const DECISION_LOG: &str = "mapping-decisions.jsonl";
/// JavaScript's largest safe integer; a VNUM above it cannot cross GMCP intact.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// A main-isolate script that prints every mark it is sent. Lines arrive in
/// the order scripts print them, so a mark's line arriving means every line
/// printed before the mark was sent has arrived too.
const MARK_SCRIPT: &str = r#"import gmcp from "smudgy:state/gmcp";
import { echo } from "smudgy:core";
gmcp.watch("Litmus.Mark", (mark: unknown) => echo(`LITMUS_MARK ${mark}`));
"#;
const MARK_MESSAGE: &str = "Litmus.Mark";
const MARK_LINE: &str = "LITMUS_MARK ";

/// How long one visit may take before it is recorded as timed out.
const VISIT_LIMIT: Duration = Duration::from_mins(1);
/// The mapper is quiet once neither its maps nor its decision log changed for this long.
const QUIET_WINDOW: Duration = Duration::from_secs(10);
/// How long the mapper gets to go quiet after the last visit.
const QUIET_LIMIT: Duration = Duration::from_mins(5);

// === Naming zones ===

/// Where a zone's name came from, in the order they are tried.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "kebab-case")]
enum NameSource {
    LegacyMap,
    ZonesProperty,
    SingleZoneMap,
}

/// A zone and the area name the game would report in it.
struct ZoneName {
    zone: String,
    name: String,
    source: NameSource,
}

fn property<'a>(properties: &'a [Property], name: &str) -> Option<&'a str> {
    properties
        .iter()
        .find(|property| property.name == name)
        .map(|property| property.value.as_str())
}

fn trimmed(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

fn room_zone(room: &RoomWithDetails) -> Option<&str> {
    trimmed(property(&room.properties, ZONE))
}

/// The zone an older version made the map for.
fn legacy_zone(map: &AreaWithDetails) -> Option<&str> {
    trimmed(property(&map.properties, ZONE))
}

/// The zone number in a `NukeFire Zone N` placeholder's name.
fn placeholder_zone(name: &str) -> Option<&str> {
    let digits = name.trim().strip_prefix(PLACEHOLDER_PREFIX)?;
    (!digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())).then_some(digits)
}

fn is_managed(map: &AreaWithDetails) -> bool {
    property(&map.properties, MANAGED).is_some()
}

/// Whether the map's name can be an area name: not empty, not a placeholder.
fn named(map: &AreaWithDetails) -> bool {
    placeholder_zone(&map.area.name).is_none() && !map.area.name.trim().is_empty()
}

/// The `{ zone, rawName }` entries of a map's `nukefire.zones` list, zones trimmed.
fn zones_property(map: &AreaWithDetails) -> Vec<(String, String)> {
    let Some(raw) = property(&map.properties, ZONES).filter(|raw| !raw.is_empty()) else {
        return Vec::new();
    };
    let Ok(Value::Array(entries)) = serde_json::from_str::<Value>(raw) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let zone = match entry.get("zone")? {
                Value::Number(number) => number.to_string(),
                Value::String(text) => text.clone(),
                _ => return None,
            };
            let raw_name = entry.get("rawName")?.as_str()?;
            Some((zone.trim().to_string(), raw_name.to_string()))
        })
        .collect()
}

/// Zones in numeric order, then any others by their text.
fn zone_order(a: &str, b: &str) -> Ordering {
    match (a.parse::<u64>(), b.parse::<u64>()) {
        (Ok(x), Ok(y)) => x.cmp(&y).then_with(|| a.cmp(b)),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => a.cmp(b),
    }
}

/// Orders maps by `size`, largest first, then by id.
fn by_size_then_id(maps: &mut [&AreaWithDetails], size: impl Fn(&AreaWithDetails) -> usize) {
    maps.sort_by(|a, b| {
        size(b)
            .cmp(&size(a))
            .then_with(|| a.area.id.0.cmp(&b.area.id.0))
    });
}

fn rooms_of_zone(map: &AreaWithDetails, zone: &str) -> usize {
    map.rooms
        .iter()
        .filter(|room| room_zone(room) == Some(zone))
        .count()
}

/// The area name of every zone the managed maps reveal, in zone order.
fn name_zones(managed: &[&AreaWithDetails]) -> Vec<ZoneName> {
    let mut zones = BTreeSet::new();
    for map in managed {
        zones.extend(map.rooms.iter().filter_map(room_zone).map(str::to_string));
        zones.extend(legacy_zone(map).map(str::to_string));
        zones.extend(placeholder_zone(&map.area.name).map(str::to_string));
        zones.extend(zones_property(map).into_iter().map(|(zone, _)| zone));
    }
    let mut zones: Vec<String> = zones.into_iter().collect();
    zones.sort_by(|a, b| zone_order(a, b));

    let mut names = Vec::new();
    for zone in zones {
        let mut made_for_zone: Vec<_> = managed
            .iter()
            .copied()
            .filter(|map| named(map) && legacy_zone(map) == Some(zone.as_str()))
            .collect();
        by_size_then_id(&mut made_for_zone, |map| map.rooms.len());
        if let Some(map) = made_for_zone.first() {
            let name = map.area.name.trim().to_string();
            names.push(ZoneName {
                zone,
                name,
                source: NameSource::LegacyMap,
            });
            continue;
        }

        let mut holders = managed.to_vec();
        by_size_then_id(&mut holders, |map| rooms_of_zone(map, &zone));
        let listed = holders
            .iter()
            .flat_map(|map| zones_property(map))
            .find(|(listed_zone, raw_name)| *listed_zone == zone && !raw_name.trim().is_empty());
        if let Some((_, raw_name)) = listed {
            let name = raw_name.trim().to_string();
            names.push(ZoneName {
                zone,
                name,
                source: NameSource::ZonesProperty,
            });
            continue;
        }

        let mut single_zone: Vec<_> = managed
            .iter()
            .copied()
            .filter(|map| {
                named(map)
                    && legacy_zone(map).is_none()
                    && !map.rooms.is_empty()
                    && map
                        .rooms
                        .iter()
                        .all(|room| room_zone(room) == Some(zone.as_str()))
            })
            .collect();
        by_size_then_id(&mut single_zone, |map| map.rooms.len());
        if let Some(map) = single_zone.first() {
            let name = map.area.name.trim().to_string();
            names.push(ZoneName {
                zone,
                name,
                source: NameSource::SingleZoneMap,
            });
        }
    }
    names
}

// === Planning the visits ===

/// Standing once in a zone.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct Visit {
    zone: String,
    area_name: String,
    name_source: NameSource,
    vnum: u64,
    /// The map holding the room before the run.
    map: String,
    #[serde(skip)]
    zone_number: i64,
    #[serde(skip)]
    title: String,
    #[serde(skip)]
    terrain: String,
    #[serde(skip)]
    position: (i64, i64, i64),
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Skipped {
    zone: String,
    area_name: String,
    reason: &'static str,
}

/// A VNUM written exactly as the mapper writes it: canonical decimal digits.
fn canonical_vnum(external_id: Option<&str>) -> Option<u64> {
    let raw = external_id?;
    let vnum: u64 = raw.parse().ok()?;
    (vnum <= MAX_SAFE_INTEGER && vnum.to_string() == raw).then_some(vnum)
}

/// A stored coordinate on the grid the game charts on.
#[allow(clippy::cast_possible_truncation)] // Map coordinates are small whole numbers stored as floats.
fn grid(coordinate: f32) -> i64 {
    coordinate.round() as i64
}

/// One visit for every named zone that has a room to stand in.
fn plan_visits(maps: &[AreaWithDetails]) -> (Vec<Visit>, Vec<Skipped>) {
    let mut copies: HashMap<&str, usize> = HashMap::new();
    for room in maps.iter().flat_map(|map| &map.rooms) {
        if let Some(external_id) = room.external_id.as_deref() {
            *copies.entry(external_id).or_default() += 1;
        }
    }
    let managed: Vec<_> = maps.iter().filter(|map| is_managed(map)).collect();

    let (mut visits, mut skipped) = (Vec::new(), Vec::new());
    for ZoneName { zone, name, source } in name_zones(&managed) {
        let skip = |reason| Skipped {
            zone: zone.clone(),
            area_name: name.clone(),
            reason,
        };
        let Some(zone_number) = zone
            .parse::<i64>()
            .ok()
            .filter(|number| number.unsigned_abs() <= MAX_SAFE_INTEGER)
        else {
            skipped.push(skip("the zone is not a number"));
            continue;
        };
        let stand_in = managed
            .iter()
            .flat_map(|map| map.rooms.iter().map(move |room| (*map, room)))
            .filter(|(_, room)| room_zone(room) == Some(zone.as_str()))
            .filter(|(_, room)| {
                room.external_id
                    .as_deref()
                    .is_some_and(|external_id| copies.get(external_id) == Some(&1))
            })
            .filter_map(|(map, room)| {
                Some((canonical_vnum(room.external_id.as_deref())?, map, room))
            })
            .min_by_key(|(vnum, _, _)| *vnum);
        let Some((vnum, map, room)) = stand_in else {
            skipped.push(skip("no room of the zone has a VNUM held once"));
            continue;
        };
        visits.push(Visit {
            zone: zone.clone(),
            area_name: name.clone(),
            name_source: source,
            vnum,
            map: map.area.name.clone(),
            zone_number,
            title: room.title.clone(),
            terrain: property(&room.properties, TERRAIN)
                .unwrap_or_default()
                .to_string(),
            position: (grid(room.x), grid(room.y), i64::from(room.level)),
        });
    }
    (visits, skipped)
}

// === Replaying ===

/// The mapper's decision log, read as it grows.
struct DecisionLog {
    dir: PathBuf,
    path: Option<PathBuf>,
    offset: u64,
    partial: Vec<u8>,
    records: Vec<Value>,
}

impl DecisionLog {
    fn new(dir: PathBuf) -> Self {
        Self {
            dir,
            path: None,
            offset: 0,
            partial: Vec::new(),
            records: Vec::new(),
        }
    }

    /// Reads the records appended since the last read.
    fn read(&mut self) {
        if self.path.is_none() {
            self.path = find_file(&self.dir, DECISION_LOG);
        }
        let Some(path) = &self.path else {
            return;
        };
        let Ok(mut file) = fs::File::open(path) else {
            return;
        };
        let mut bytes = Vec::new();
        if file.seek(SeekFrom::Start(self.offset)).is_err() || file.read_to_end(&mut bytes).is_err()
        {
            return;
        }
        self.offset += u64::try_from(bytes.len()).expect("a file length");
        self.partial.extend_from_slice(&bytes);
        while let Some(end) = self.partial.iter().position(|&byte| byte == b'\n') {
            let line: Vec<u8> = self.partial.drain(..=end).collect();
            if let Ok(record) = serde_json::from_slice(&line) {
                self.records.push(record);
            }
        }
    }

    fn last_mutation_id(&self) -> u64 {
        self.records
            .iter()
            .filter_map(|record| record["mutationId"].as_u64())
            .max()
            .unwrap_or(0)
    }
}

/// How one visit went.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct VisitResult {
    #[serde(flatten)]
    visit: Visit,
    outcome: Outcome,
    /// From the `Room.Info` to the logged end of the pass that maps the room.
    millis: f64,
    /// From the `Room.Info` to the logged end of settling, when the zone had
    /// maps to take from.
    settle_millis: Option<f64>,
    /// The map holding the room afterwards, and that map's key.
    map_after: Option<String>,
    key_after: Option<String>,
    notices: Vec<String>,
    /// What the mapper logged about settling and errors during the visit.
    decisions: Vec<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum Outcome {
    /// The zone settled and rooms moved into its map.
    Merged,
    /// The zone settled by removing empty maps; no room moved.
    RemovedEmptyMaps,
    /// Nothing moved and no map was removed; a map may have been created,
    /// adopted or renamed.
    Mapped,
    /// A merge was refused.
    Refused,
    /// The pass that maps the room failed.
    Failed,
    /// No pass finished within the visit limit.
    TimedOut,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Self::Merged => "merged",
            Self::RemovedEmptyMaps => "removed-empty-maps",
            Self::Mapped => "mapped",
            Self::Refused => "refused",
            Self::Failed => "failed",
            Self::TimedOut => "timed-out",
        }
    }
}

/// The time from `start` to when the mapper logged `record`.
fn millis_since(start: DateTime<Utc>, record: &Value) -> Option<f64> {
    let logged = DateTime::parse_from_rfc3339(record["timestamp"].as_str()?).ok()?;
    let elapsed = logged.with_timezone(&Utc) - start;
    // The log keeps whole milliseconds, so a moment before `start` reads as none.
    Some(
        elapsed
            .to_std()
            .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1e3),
    )
}

/// The room with `vnum`: the name of its map and that map's key.
fn holder(mapper: &Mapper, vnum: u64) -> Option<(String, Option<String>)> {
    let atlas = mapper.get_current_atlas();
    let (key, _) = atlas.find_room_by_external_id(&vnum.to_string())?;
    let map = atlas.get_area(&key.area_id)?;
    Some((
        map.get_name().to_string(),
        map.get_property(AREA_KEY).map(str::to_string),
    ))
}

/// Whether `record` ends the pass that maps a visit: its room batch, for the
/// map now holding the room, finished after `floor`, or the pass failed.
fn ends_visit(record: &Value, floor: u64, map_name: Option<&str>) -> Option<Outcome> {
    match record["kind"].as_str()? {
        "mapping-error" => Some(Outcome::Failed),
        kind @ ("mutation-complete" | "mutation-error") => {
            let description = record["description"].as_str()?;
            let batch_for = description.strip_prefix(APPLY_PREFIX)?;
            let current = record["mutationId"].as_u64()? > floor && Some(batch_for) == map_name;
            current.then_some(if kind == "mutation-complete" {
                Outcome::Mapped
            } else {
                Outcome::Failed
            })
        }
        _ => None,
    }
}

/// The session being replayed, its decision log, and the marks sent so far.
struct Replayer<'a> {
    session: Session,
    log: DecisionLog,
    mapper: &'a Mapper,
    marks: u64,
}

impl Replayer<'_> {
    /// Waits until every line the session printed before this call has
    /// arrived: the mark script's line for a fresh mark arrives after them.
    /// Marks are sent again until one is echoed, since the script may still
    /// be loading.
    async fn mark(&mut self) {
        for _ in 0..20 {
            self.marks += 1;
            let echo = format!("{MARK_LINE}{}", self.marks);
            self.session.gmcp(MARK_MESSAGE, &self.marks.to_string());
            let session = &mut self.session;
            let ready = |lines: &[String]| lines.iter().rev().any(|line| *line == echo);
            let wait = Duration::from_millis(500);
            if wait_until_within(&mut session.events, &mut session.lines, ready, wait).await {
                return;
            }
        }
        panic!(
            "the mark script never answered:\n{}",
            self.session.transcript()
        );
    }

    /// Stands the player in the visit's room and waits for the mapper to map it.
    async fn visit(&mut self, visit: &Visit) -> VisitResult {
        let floor = self.log.last_mutation_id();
        let (first_line, first_record) = (self.session.lines.len(), self.log.records.len());
        let (x, y, z) = visit.position;
        let room = ChartRoom {
            vnum: visit.vnum,
            name: &visit.title,
            zone: visit.zone_number,
            terrain: &visit.terrain,
            x,
            y,
            z,
        };
        let (started, sent_at) = (Instant::now(), Utc::now());
        self.session.visit(&visit.area_name, &room, &[], &[]);
        let mut outcome = loop {
            self.log.read();
            let map_name = holder(self.mapper, visit.vnum).map(|(name, _)| name);
            let ended = self.log.records[first_record..]
                .iter()
                .find_map(|record| ends_visit(record, floor, map_name.as_deref()));
            if let Some(outcome) = ended {
                break outcome;
            }
            if started.elapsed() >= VISIT_LIMIT {
                break Outcome::TimedOut;
            }
            self.session.run_for(Duration::from_millis(5)).await;
        };
        let millis = started.elapsed().as_secs_f64() * 1e3;
        self.mark().await;
        self.log.read();

        let decisions: Vec<Value> = self.log.records[first_record..]
            .iter()
            .filter(|record| {
                matches!(
                    record["kind"].as_str(),
                    Some(
                        "zone-settled"
                            | "zone-merge-refused"
                            | "duplicate-room-kept"
                            | "mapping-error"
                            | "mutation-error"
                    )
                )
            })
            .cloned()
            .collect();
        // Settling that took from other maps; a zone with nothing to take is only mapped.
        let settled = decisions
            .iter()
            .find(|record| match record["kind"].as_str() {
                Some("zone-merge-refused") => true,
                Some("zone-settled") => record["sources"]
                    .as_array()
                    .is_some_and(|sources| !sources.is_empty()),
                _ => false,
            });
        if outcome == Outcome::Mapped
            && let Some(settled) = settled
        {
            outcome = match (settled["kind"].as_str(), settled["moved"].as_u64()) {
                (Some("zone-merge-refused"), _) => Outcome::Refused,
                (_, Some(0)) => Outcome::RemovedEmptyMaps,
                _ => Outcome::Merged,
            };
        }
        let (map_after, key_after) = holder(self.mapper, visit.vnum).unzip();
        VisitResult {
            visit: visit.clone(),
            outcome,
            millis,
            settle_millis: settled.and_then(|record| millis_since(sent_at, record)),
            map_after,
            key_after: key_after.flatten(),
            notices: notices(&self.session.lines[first_line..]),
            decisions,
        }
    }

    /// Lets the mapper run until neither its maps nor its decision log change
    /// for `QUIET_WINDOW`, or `QUIET_LIMIT` passes.
    async fn wait_for_quiet(&mut self) -> Quiet {
        let started = Instant::now();
        let mut last_change = Instant::now();
        let mut seen = (self.log.offset, self.mapper.sync_revision());
        loop {
            self.session.run_for(Duration::from_millis(250)).await;
            self.log.read();
            let now = (self.log.offset, self.mapper.sync_revision());
            if now != seen {
                seen = now;
                last_change = Instant::now();
            }
            let reached = last_change.elapsed() >= QUIET_WINDOW;
            if reached || started.elapsed() >= QUIET_LIMIT {
                return Quiet {
                    reached,
                    millis: started.elapsed().as_secs_f64() * 1e3,
                };
            }
        }
    }

    /// Stops the session and waits for it to end.
    async fn stop(&mut self) {
        self.session.shutdown();
        let session = &mut self.session;
        let drained = tokio::time::timeout(Duration::from_mins(1), async {
            while let Some(event) = session.events.next().await {
                if let SessionEvent::UpdateBuffer(updates) = event.event {
                    collect(&updates, &mut session.lines);
                }
            }
        })
        .await;
        assert!(drained.is_ok(), "the session did not stop");
    }
}

fn notices(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter(|line| line.starts_with(NOTICE_PREFIX))
        .cloned()
        .collect()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Quiet {
    reached: bool,
    millis: f64,
}

// === Reading stores ===

/// A store's map documents, with the files that could not be read.
struct Store {
    maps: Vec<AreaWithDetails>,
    unreadable: Vec<String>,
}

fn read_store(root: &Path) -> Store {
    let mut store = Store {
        maps: Vec::new(),
        unreadable: Vec::new(),
    };
    let mut files: Vec<PathBuf> = fs::read_dir(root.join("areas-v2"))
        .expect("list the store's maps")
        .map(|entry| entry.expect("list the store's maps").path())
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    files.sort();
    for file in files {
        let parsed = fs::read(&file)
            .map_err(|error| error.to_string())
            .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|error| error.to_string()));
        match parsed {
            Ok(map) => store.maps.push(map),
            Err(error) => store
                .unreadable
                .push(format!("{}: {error}", file.display())),
        }
    }
    store
}

/// Store-wide counts.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Totals {
    maps: usize,
    empty_maps: usize,
    rooms: usize,
    exits: usize,
    /// Exits that name a room no map holds.
    dangling_exits: usize,
    /// Rooms beyond the first carrying the same external id.
    duplicate_rooms: usize,
    /// Rooms sharing their grid cell with another room of their map.
    overlapping_rooms: usize,
    /// Maps named `NukeFire Zone N`.
    placeholders: usize,
    labels: usize,
    shapes: usize,
}

impl Totals {
    fn of(maps: &[AreaWithDetails]) -> Self {
        let rooms: HashSet<(AreaId, RoomNumber)> = maps
            .iter()
            .flat_map(|map| map.rooms.iter().map(|room| (map.area.id, room.room_number)))
            .collect();
        let exits = maps
            .iter()
            .flat_map(|map| &map.rooms)
            .flat_map(|room| &room.exits);
        let dangling = exits
            .clone()
            .filter(|exit| match (exit.to_area_id, exit.to_room_number) {
                (None, None) => false,
                (Some(area), Some(number)) => !rooms.contains(&(area, number)),
                _ => true,
            })
            .count();
        let mut copies: HashMap<&str, usize> = HashMap::new();
        for room in maps.iter().flat_map(|map| &map.rooms) {
            if let Some(external_id) = room.external_id.as_deref() {
                *copies.entry(external_id).or_default() += 1;
            }
        }
        Self {
            maps: maps.len(),
            empty_maps: maps.iter().filter(|map| map.rooms.is_empty()).count(),
            rooms: rooms.len(),
            exits: exits.count(),
            dangling_exits: dangling,
            duplicate_rooms: copies.values().map(|count| count - 1).sum(),
            overlapping_rooms: maps.iter().map(overlapping_rooms).sum(),
            placeholders: maps
                .iter()
                .filter(|map| placeholder_zone(&map.area.name).is_some())
                .count(),
            labels: maps.iter().map(|map| map.labels.len()).sum(),
            shapes: maps.iter().map(|map| map.shapes.len()).sum(),
        }
    }
}

fn overlapping_rooms(map: &AreaWithDetails) -> usize {
    let mut cells: HashMap<(i64, i64, i32), usize> = HashMap::new();
    for room in &map.rooms {
        *cells
            .entry((grid(room.x), grid(room.y), room.level))
            .or_default() += 1;
    }
    cells.values().filter(|&&count| count > 1).sum()
}

/// What the canonical-areas rules leave unmet after the run.
#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
struct Findings {
    /// Visited zones whose rooms still sit in more than one map, with those maps.
    zones_in_several_maps: BTreeMap<String, Vec<String>>,
    /// Keys more than one map carries, with those maps.
    keys_on_several_maps: BTreeMap<String, Vec<String>>,
    /// Maps whose key is not their own name, lowercased.
    maps_keyed_apart_from_their_name: Vec<String>,
    /// Empty managed maps tied to a visited zone by `nukefire.zone` or a placeholder name.
    empty_maps_of_visited_zones: Vec<String>,
    /// Placeholder maps of zones that were visited.
    placeholders_of_visited_zones: Vec<String>,
}

fn label(map: &AreaWithDetails) -> String {
    format!(
        "{} ({}, {} rooms)",
        map.area.name,
        map.area.id,
        map.rooms.len()
    )
}

fn findings(maps: &[AreaWithDetails], visited: &HashSet<&str>) -> Findings {
    let mut found = Findings::default();
    let mut holders: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut keys: BTreeMap<&str, Vec<String>> = BTreeMap::new();
    for map in maps {
        for zone in map.rooms.iter().filter_map(room_zone) {
            if visited.contains(zone) {
                holders.entry(zone).or_default().insert(label(map));
            }
        }
        if let Some(key) = property(&map.properties, AREA_KEY) {
            keys.entry(key).or_default().push(label(map));
            if key != map.area.name.trim().to_lowercase() {
                found
                    .maps_keyed_apart_from_their_name
                    .push(format!("{} keyed {key:?}", label(map)));
            }
        }
        let tied = [legacy_zone(map), placeholder_zone(&map.area.name)]
            .into_iter()
            .flatten()
            .any(|zone| visited.contains(zone));
        if is_managed(map) && map.rooms.is_empty() && tied {
            found.empty_maps_of_visited_zones.push(label(map));
        }
        if placeholder_zone(&map.area.name).is_some_and(|zone| visited.contains(zone)) {
            found.placeholders_of_visited_zones.push(label(map));
        }
    }
    found.zones_in_several_maps = holders
        .into_iter()
        .filter(|(_, maps)| maps.len() > 1)
        .map(|(zone, maps)| (zone.to_string(), maps.into_iter().collect()))
        .collect();
    found.keys_on_several_maps = keys
        .into_iter()
        .filter(|(_, maps)| maps.len() > 1)
        .map(|(key, maps)| (key.to_string(), maps))
        .collect();
    found
}

// === Files ===

/// Refuses a path inside a git repository or worktree: a player's map store
/// must never land where it could be committed.
fn assert_outside_repository(path: &Path) {
    let absolute = std::path::absolute(path).expect("resolve the path");
    for ancestor in absolute.ancestors() {
        assert!(
            !ancestor.join(".git").exists(),
            "{} is inside the repository at {}; choose a directory outside every repository",
            absolute.display(),
            ancestor.display()
        );
    }
}

/// Copies the store directories from `from` into `to`, replacing what `to` held.
fn copy_store(from: &Path, to: &Path) {
    for dir in STORE_DIRS {
        let target = to.join(dir);
        if target.exists() {
            fs::remove_dir_all(&target).expect("clear an earlier copy");
        }
        fs::create_dir_all(&target).expect("create the store copy");
        let Ok(entries) = fs::read_dir(from.join(dir)) else {
            continue;
        };
        for entry in entries {
            let path = entry.expect("list the store").path();
            if path.is_file() {
                fs::copy(&path, target.join(path.file_name().expect("a file name")))
                    .expect("copy a store file");
            }
        }
    }
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

fn env_path(name: &str) -> PathBuf {
    PathBuf::from(
        std::env::var_os(name).unwrap_or_else(|| panic!("set {name}; see the module docs")),
    )
}

// === The report ===

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Report {
    store: String,
    load_millis: f64,
    zones_named: usize,
    zones_visited: usize,
    skipped: Vec<Skipped>,
    outcomes: BTreeMap<&'static str, usize>,
    timings: Timings,
    quiet: Quiet,
    before: Totals,
    after: Totals,
    unreadable_before: Vec<String>,
    unreadable_after: Vec<String>,
    notices: Vec<String>,
    findings: Findings,
    violations: Vec<String>,
    visits: Vec<VisitResult>,
}

/// Visit times in milliseconds, from the `Room.Info` to the logged end of
/// the pass that maps the room.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Timings {
    total: f64,
    median: f64,
    p95: f64,
    max: f64,
    /// The same for the visits that moved rooms.
    merged_median: f64,
    merged_p95: f64,
    merged_max: f64,
    /// From the `Room.Info` to the logged end of settling, for every zone
    /// that had maps to take from.
    settle_median: f64,
    settle_p95: f64,
    settle_max: f64,
}

/// The nearest-rank percentile of sorted times.
fn percentile(sorted: &[f64], percent: usize) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (sorted.len() * percent).div_ceil(100).max(1);
    sorted[rank - 1]
}

fn timings(visits: &[VisitResult]) -> Timings {
    let sorted = |time: &dyn Fn(&VisitResult) -> Option<f64>| {
        let mut times: Vec<f64> = visits.iter().filter_map(time).collect();
        times.sort_by(f64::total_cmp);
        times
    };
    let all = sorted(&|visit| Some(visit.millis));
    let merged = sorted(&|visit| (visit.outcome == Outcome::Merged).then_some(visit.millis));
    let settles = sorted(&|visit| visit.settle_millis);
    Timings {
        total: all.iter().sum(),
        median: percentile(&all, 50),
        p95: percentile(&all, 95),
        max: all.last().copied().unwrap_or_default(),
        merged_median: percentile(&merged, 50),
        merged_p95: percentile(&merged, 95),
        merged_max: merged.last().copied().unwrap_or_default(),
        settle_median: percentile(&settles, 50),
        settle_p95: percentile(&settles, 95),
        settle_max: settles.last().copied().unwrap_or_default(),
    }
}

fn violations(
    before: &Totals,
    after: &Totals,
    journals: &[String],
    unreadable: &[String],
) -> Vec<String> {
    let mut found = Vec::new();
    let joined = before.duplicate_rooms.saturating_sub(after.duplicate_rooms);
    if before.rooms.checked_sub(after.rooms) != Some(joined) {
        found.push(format!(
            "{} rooms before and {} after, with {joined} duplicate VNUMs joined",
            before.rooms, after.rooms
        ));
    }
    if after.dangling_exits > before.dangling_exits {
        found.push(format!(
            "exits to missing rooms grew from {} to {}",
            before.dangling_exits, after.dangling_exits
        ));
    }
    found.extend(
        journals
            .iter()
            .map(|journal| format!("journal {journal} was left behind")),
    );
    found.extend(
        unreadable
            .iter()
            .map(|file| format!("the result store holds an unreadable map: {file}")),
    );
    found
}

fn summary(report: &Report) -> String {
    let (b, a) = (&report.before, &report.after);
    let mut lines = vec![
        format!(
            "{} zones named, {} visited, {} skipped; loading the store took {:.0} ms",
            report.zones_named,
            report.zones_visited,
            report.skipped.len(),
            report.load_millis
        ),
        format!("outcomes: {:?}", report.outcomes),
        format!(
            "visits: {:.0} ms in all, median {:.0} ms, p95 {:.0} ms, max {:.0} ms",
            report.timings.total, report.timings.median, report.timings.p95, report.timings.max
        ),
        format!(
            "visits that moved rooms: median {:.0} ms, p95 {:.0} ms, max {:.0} ms",
            report.timings.merged_median, report.timings.merged_p95, report.timings.merged_max
        ),
        format!(
            "settling, where the zone had maps to take from: median {:.0} ms, p95 {:.0} ms, max {:.0} ms",
            report.timings.settle_median, report.timings.settle_p95, report.timings.settle_max
        ),
        format!(
            "quiet after the last visit: {} after {:.0} ms",
            if report.quiet.reached {
                "reached"
            } else {
                "not reached"
            },
            report.quiet.millis
        ),
    ];
    let totals = [
        ("maps", b.maps, a.maps),
        ("empty maps", b.empty_maps, a.empty_maps),
        ("rooms", b.rooms, a.rooms),
        ("exits", b.exits, a.exits),
        ("exits to missing rooms", b.dangling_exits, a.dangling_exits),
        ("duplicate rooms", b.duplicate_rooms, a.duplicate_rooms),
        (
            "overlapping rooms",
            b.overlapping_rooms,
            a.overlapping_rooms,
        ),
        ("placeholder maps", b.placeholders, a.placeholders),
        ("labels", b.labels, a.labels),
        ("shapes", b.shapes, a.shapes),
    ];
    lines.extend(totals.map(|(what, before, after)| format!("{what}: {before} -> {after}")));
    let f = &report.findings;
    lines.push(format!(
        "\nvisited zones still in several maps: {}; keys on several maps: {}; maps keyed apart from their name: {}; empty maps of visited zones: {}; placeholders of visited zones: {}",
        f.zones_in_several_maps.len(),
        f.keys_on_several_maps.len(),
        f.maps_keyed_apart_from_their_name.len(),
        f.empty_maps_of_visited_zones.len(),
        f.placeholders_of_visited_zones.len(),
    ));
    lines.push("\nvisits that did more than map their room:".to_string());
    lines.extend(
        report
            .visits
            .iter()
            .filter(|result| result.outcome != Outcome::Mapped)
            .map(|result| {
                format!(
                    "{:>8.0} ms (settled after {:.0} ms)  zone {} {:?} ({}): {} -> {}",
                    result.millis,
                    result.settle_millis.unwrap_or_default(),
                    result.visit.zone,
                    result.visit.area_name,
                    result.outcome.as_str(),
                    result.visit.map,
                    result.map_after.as_deref().unwrap_or("(no map)")
                )
            }),
    );
    lines.push(format!("\n{} notices", report.notices.len()));
    lines.extend(report.notices.iter().map(|notice| format!("  {notice}")));
    lines.push(format!("\n{} violations", report.violations.len()));
    lines.extend(
        report
            .violations
            .iter()
            .map(|violation| format!("  {violation}")),
    );
    lines.join("\n") + "\n"
}

// === The test ===

/// Replays one visit per named zone of the store in
/// `SMUDGY_NUKEFIRE_LITMUS_STORE` and writes to `SMUDGY_NUKEFIRE_LITMUS_OUT`.
#[tokio::test]
#[ignore = "needs a map store; see the module docs"]
async fn nukefire_canonical_litmus() {
    let _ = pretty_env_logger::try_init();
    let (store, out) = (
        env_path("SMUDGY_NUKEFIRE_LITMUS_STORE"),
        env_path("SMUDGY_NUKEFIRE_LITMUS_OUT"),
    );
    assert!(
        store.join("areas-v2").is_dir(),
        "{} holds no areas-v2 directory",
        store.display()
    );
    assert_outside_repository(&out);
    let home = smudgy_home();
    assert_outside_repository(&home);
    install_mapper_packages(SERVER);
    shared_packages::save_param_value(SERVER, MAPPER_SPEC, "debugMappingDecisions", json!(true))
        .expect("turn the decision log on");
    fs::write(
        home.join(SERVER).join("modules").join("litmus-mark.ts"),
        MARK_SCRIPT,
    )
    .expect("write the mark script");

    let map_root = home.join("litmus");
    copy_store(&store, &map_root.join("local"));
    let before = read_store(&map_root.join("local"));
    let (visits, skipped) = plan_visits(&before.maps);

    let mapper = local_mapper(&map_root);
    let loading = Instant::now();
    mapper.ready().await.expect("load the copied store");
    let load_millis = loading.elapsed().as_secs_f64() * 1e3;

    let mut replayer = Replayer {
        session: start_session(SERVER, 9500, &mapper).await,
        log: DecisionLog::new(home.join(SERVER)),
        mapper: &mapper,
        marks: 0,
    };
    replayer.mark().await;
    let startup_lines = replayer.session.lines.len();
    let mut results = Vec::new();
    for visit in &visits {
        results.push(replayer.visit(visit).await);
    }
    let quiet = replayer.wait_for_quiet().await;
    replayer.stop().await;
    let Replayer { session, log, .. } = replayer;
    drop(mapper);

    fs::create_dir_all(&out).expect("create the output directory");
    let result_store = out.join("store");
    copy_store(&map_root.join("local"), &result_store);
    if let Some(path) = &log.path {
        fs::copy(path, out.join(DECISION_LOG)).expect("copy the decision log");
    }
    let after = read_store(&result_store);

    let visited: HashSet<&str> = visits.iter().map(|visit| visit.zone.as_str()).collect();
    let mut outcomes = BTreeMap::new();
    for result in &results {
        *outcomes.entry(result.outcome.as_str()).or_default() += 1;
    }
    let (before_totals, after_totals) = (Totals::of(&before.maps), Totals::of(&after.maps));
    let report = Report {
        store: store.display().to_string(),
        load_millis,
        zones_named: visits.len() + skipped.len(),
        zones_visited: visits.len(),
        skipped,
        outcomes,
        timings: timings(&results),
        quiet,
        violations: violations(
            &before_totals,
            &after_totals,
            &journals(&map_root.join("local")),
            &after.unreadable,
        ),
        before: before_totals,
        after: after_totals,
        unreadable_before: before.unreadable,
        unreadable_after: after.unreadable,
        notices: notices(&session.lines[startup_lines..]),
        findings: findings(&after.maps, &visited),
        visits: results,
    };
    let results_json = serde_json::to_vec_pretty(&report).expect("serialize the report");
    fs::write(out.join("results.json"), results_json).expect("write results.json");
    fs::write(out.join("summary.txt"), summary(&report)).expect("write summary.txt");

    // The session's copy of the store and its decision log hold the player's
    // maps: only the output directory keeps them.
    drop(session);
    for private in [map_root, home.join(SERVER)] {
        if let Err(error) = fs::remove_dir_all(&private) {
            eprintln!("could not remove {}: {error}", private.display());
        }
    }

    print!("{}", summary(&report));
    assert!(
        report.violations.is_empty(),
        "{} violations; see {}",
        report.violations.len(),
        out.join("results.json").display()
    );
}
