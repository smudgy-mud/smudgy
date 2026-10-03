use std::{
    collections::BTreeSet,
    path::Path,
    pin::Pin,
    sync::Arc,
    time::{Duration, Instant},
};

use futures::{Stream, StreamExt};
use serde_json::{Value, json};
use smudgy_cloud::{
    AreaId, AreaWithDetails, Credential, CredentialSource, ExitArgs, ExitDirection, LocalBackend,
    MapDestination, MapStorage, Mapper, MapperBackend, PackageApiClient, RoomNumber, RoomUpdates,
    RoomWithDetails,
    mapper::{MutationSubmission, RoomKey},
    mutation::AreaMutation,
};
use smudgy_core::session::runtime::{RuntimeAction, RuntimeThreadJoinOutcome, join_runtime_thread};
use smudgy_core::session::{
    BufferUpdate, SessionEvent, SessionId, SessionParams, TaggedSessionEvent, spawn,
};
use tokio::sync::{mpsc::UnboundedSender, watch};

use super::{Config, Sink, below, duration, packages, packing_measure};

struct Fixture {
    area: AreaWithDetails,
    center: usize,
    known: usize,
    discovery: usize,
    chart: BTreeSet<RoomNumber>,
}

struct Host {
    events: Pin<Box<dyn Stream<Item = TaggedSessionEvent>>>,
    tx: UnboundedSender<RuntimeAction>,
    session_id: SessionId,
    sink: Arc<Sink>,
    commits: watch::Receiver<u64>,
    local: Arc<LocalBackend>,
    lines: Vec<String>,
}

async fn durable(mapper: &Mapper, submitted: MutationSubmission) {
    if let Some(operation) = submitted.operation_id() {
        mapper.wait_for_mutation(operation).await.unwrap();
    }
}

async fn synthetic(mapper: &Mapper) -> AreaWithDetails {
    let id = mapper
        .create_area_at(
            "Synthetic benchmark".into(),
            MapDestination::loose(MapStorage::Local),
        )
        .await
        .unwrap();
    durable(
        mapper,
        mapper
            .mutate_area(
                id,
                vec![
                    AreaMutation::UpsertAreaProperty {
                        name: "nukefire.mapper".into(),
                        value: "NukeFire.Map.Local".into(),
                        is_secret: None,
                    },
                    AreaMutation::UpsertAreaProperty {
                        name: "nukefire.zone".into(),
                        value: "1".into(),
                        is_secret: None,
                    },
                    AreaMutation::UpsertAreaProperty {
                        name: "nukefire.area".into(),
                        value: "Synthetic benchmark".into(),
                        is_secret: None,
                    },
                ],
                "Synthetic benchmark metadata",
            )
            .unwrap(),
    )
    .await;
    seed_synthetic_rooms(mapper, id).await;
    seed_synthetic_links(mapper, id).await;
    let properties = (1..=16)
        .map(|number| AreaMutation::UpsertRoomProperty {
            room_number: RoomNumber(number),
            name: "nukefire.zone".into(),
            value: "1".into(),
            is_secret: None,
        })
        .collect();
    durable(
        mapper,
        mapper
            .mutate_area(id, properties, "Synthetic room zone metadata")
            .unwrap(),
    )
    .await;
    mapper.export_area(id).await.unwrap()
}

async fn seed_synthetic_rooms(mapper: &Mapper, id: AreaId) {
    for row in 0_i16..4 {
        for column in 0_i16..4 {
            let number = i32::from(row * 4 + column + 1);
            durable(
                mapper,
                mapper
                    .create_room(
                        RoomKey::new(id, RoomNumber(number)),
                        RoomUpdates {
                            title: Some(format!("Synthetic room {number}")),
                            external_id: Some(Some((1000 + number).to_string())),
                            x: Some(f32::from(column * 3)),
                            y: Some(f32::from(row * 3)),
                            level: Some(0),
                            ..RoomUpdates::default()
                        },
                    )
                    .unwrap(),
            )
            .await;
        }
    }
}

async fn seed_synthetic_links(mapper: &Mapper, id: AreaId) {
    for number in 1..=16 {
        for (direction, neighbor) in [
            (ExitDirection::East, (number % 4 != 0).then_some(number + 1)),
            (ExitDirection::South, (number <= 12).then_some(number + 4)),
        ] {
            if let Some(neighbor) = neighbor {
                for (from, to, direction) in [
                    (number, neighbor, direction),
                    (neighbor, number, direction.opposite()),
                ] {
                    let (_, submitted) = mapper
                        .create_exit_tracked(
                            RoomKey::new(id, RoomNumber(from)),
                            ExitArgs {
                                from_direction: direction,
                                to_area_id: Some(id),
                                to_room_number: Some(RoomNumber(to)),
                                to_direction: Some(direction.opposite()),
                                weight: 1.0,
                                ..ExitArgs::default()
                            },
                        )
                        .unwrap();
                    durable(mapper, submitted).await;
                }
            }
        }
    }
}

fn vnum(room: &RoomWithDetails) -> Option<u64> {
    room.external_id
        .as_deref()?
        .parse::<u64>()
        .ok()
        .filter(|value| *value <= 9_007_199_254_740_991)
}

fn neighbors(area: &AreaWithDetails, index: usize) -> Vec<usize> {
    let mut result = area.rooms[index]
        .exits
        .iter()
        .filter(|exit| {
            exit.to_area_id == Some(area.area.id)
                && matches!(
                    exit.from_direction,
                    ExitDirection::North
                        | ExitDirection::South
                        | ExitDirection::East
                        | ExitDirection::West
                        | ExitDirection::Northeast
                        | ExitDirection::Northwest
                        | ExitDirection::Southeast
                        | ExitDirection::Southwest
                )
        })
        .filter_map(|exit| {
            area.rooms.iter().position(|room| {
                Some(room.room_number) == exit.to_room_number && vnum(room).is_some()
            })
        })
        .collect::<Vec<_>>();
    result.sort_unstable();
    result.dedup();
    result
}

fn select(area: AreaWithDetails) -> Fixture {
    let (center, connected) = area
        .rooms
        .iter()
        .enumerate()
        .filter(|(_, room)| vnum(room).is_some())
        .map(|(index, _)| (index, neighbors(&area, index)))
        .find(|(_, connected)| connected.len() >= 2)
        .expect("map needs a numeric-VNUM room with two mapped compass neighbors");
    let known = connected[0];
    let discovery = connected[1];
    let mut chart = BTreeSet::from([area.rooms[center].room_number]);
    let mut frontier = vec![center];
    for _ in 0..2 {
        let mut next = Vec::new();
        for index in frontier {
            for neighbor in neighbors(&area, index) {
                if chart.insert(area.rooms[neighbor].room_number) {
                    next.push(neighbor);
                }
            }
        }
        frontier = next;
    }
    Fixture {
        area,
        center,
        known,
        discovery,
        chart,
    }
}

fn zone(area: &AreaWithDetails, room: &RoomWithDetails) -> i64 {
    room.properties
        .iter()
        .chain(&area.properties)
        .find(|property| property.name == "nukefire.zone")
        .and_then(|property| property.value.parse().ok())
        .unwrap_or(1)
}

fn room_info(fixture: &Fixture, index: usize) -> Value {
    let room = &fixture.area.rooms[index];
    json!({"num":vnum(room).unwrap(),"name":room.title,"area":fixture.area.area.name,
        "zone":zone(&fixture.area, room),"terrain":"field","exits":{},
        "coords":{"x":room.x,"y":room.y,"z":room.level}})
}

fn chart(fixture: &Fixture, index: usize, include_discovery: bool) -> Value {
    let center = &fixture.area.rooms[index];
    let removed = fixture.area.rooms[fixture.discovery].room_number;
    let included = |room: &RoomWithDetails| {
        fixture.chart.contains(&room.room_number)
            && (include_discovery || room.room_number != removed)
            && vnum(room).is_some()
    };
    let rooms = fixture.area.rooms.iter().filter(|room| included(room)).map(|room| json!({
        "vnum":vnum(room).unwrap(),"name":room.title,"zone":zone(&fixture.area, room),
        "terrain":"field","x":room.x-center.x,"y":room.y-center.y,"z":room.level-center.level,
        "current":room.room_number == center.room_number,"route":false,"destination":false,
    })).collect::<Vec<_>>();
    let mut links = Vec::new();
    for room in fixture.area.rooms.iter().filter(|room| included(room)) {
        for exit in &room.exits {
            if exit.to_area_id != Some(fixture.area.area.id) {
                continue;
            }
            let Some(target) =
                fixture.area.rooms.iter().find(|target| {
                    Some(target.room_number) == exit.to_room_number && included(target)
                })
            else {
                continue;
            };
            links.push(
                json!({"from":vnum(room).unwrap(),"to":vnum(target).unwrap(),
                "direction":exit.from_direction.to_string().to_lowercase(),"bidirectional":false,
                "closed":exit.is_closed,"locked":exit.is_locked,"route":false}),
            );
        }
    }
    json!({"version":1,"source":"bigmap+gps","center":vnum(center).unwrap(),"zone":zone(&fixture.area, center),
        "plane":0,"rooms":rooms,"links":links,"gps":{"active":false,"type":"none","target":-1,
        "description":"","steps":0,"route_raw":""},"truncated":false})
}

impl Host {
    async fn start(
        server: &str,
        mapper: &Mapper,
        local: Arc<LocalBackend>,
        sink: Arc<Sink>,
    ) -> Self {
        let session_id = SessionId::from(1);
        let extension_sink = sink.clone();
        let params = Arc::new(SessionParams {
            session_id,
            server_name: Arc::new(server.into()),
            profile_name: Arc::new("Benchmark".into()),
            profile_subtext: Arc::new(String::new()),
            mapper: Some(mapper.clone()),
            package_client: Some(PackageApiClient::new(
                "http://127.0.0.1:0",
                CredentialSource::new(Some(Credential::ApiKey("benchmark".into()))),
            )),
            extra_script_extensions: Arc::new(move || {
                vec![packing_measure::init(extension_sink.clone())]
            }),
            on_engine_rebuild: None,
        });
        let mut events: Pin<Box<dyn Stream<Item = TaggedSessionEvent>>> = Box::pin(spawn(params));
        let ready_deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        let tx = loop {
            let event = tokio::time::timeout_at(ready_deadline, events.next())
                .await
                .unwrap()
                .unwrap();
            if let SessionEvent::RuntimeReady(tx) = event.event {
                break tx;
            }
        };
        tx.send(RuntimeAction::GmcpEnabled).unwrap();
        let commits = local.subscribe_local().await.unwrap().unwrap();
        Self {
            events,
            tx,
            session_id,
            sink,
            commits,
            local,
            lines: Vec::new(),
        }
    }

    fn gmcp(&self, name: &str, payload: String) {
        self.tx
            .send(RuntimeAction::GmcpMessage {
                name: Arc::from(name),
                data: Some(Arc::from(payload)),
            })
            .unwrap();
    }

    fn input(&self, fixture: &Fixture, index: usize, discovery: bool, label: &str) {
        let prepare_started = Instant::now();
        let info = room_info(fixture, index).to_string();
        let chart = chart(fixture, index, discovery).to_string();
        self.sink.record(
            "input",
            json!({"label":label,"vnum":vnum(&fixture.area.rooms[index]),
            "fixturePreparationMs":prepare_started.elapsed().as_secs_f64()*1000.0,
            "plannerAtInput":self.sink.active_job()}),
        );
        self.gmcp("Room.Info", info);
        self.gmcp("NukeFire.Map.Local", chart);
    }

    fn command(&self, command: &str) {
        self.sink.record(
            "command",
            json!({"command":command,"plannerAtInput":self.sink.active_job()}),
        );
        self.tx
            .send(RuntimeAction::SubmitInput(Arc::new(command.into())))
            .unwrap();
    }

    fn event(&mut self, event: SessionEvent) {
        match event {
            SessionEvent::SetCurrentLocation(area, number) => {
                let vnum = self.local.local_snapshot().and_then(|snapshot| {
                    snapshot
                        .area(area)
                        .ok()
                        .and_then(|details| {
                            details
                                .rooms
                                .iter()
                                .find(|room| Some(room.room_number.0) == number)
                        })
                        .and_then(vnum)
                });
                self.sink
                    .record("location", json!({"vnum":vnum,"room":number}));
            }
            SessionEvent::UpdateBuffer(updates) => {
                for update in updates.iter() {
                    if let BufferUpdate::Append(line) = update {
                        self.lines.push(line.text.clone());
                    }
                }
            }
            _ => {}
        }
    }

    fn commit(&self, fixture: &Fixture) {
        let started = Instant::now();
        let snapshot = self.local.local_snapshot().unwrap();
        let discovered_vnum = vnum(&fixture.area.rooms[fixture.discovery]).unwrap();
        let origin_vnum = vnum(&fixture.area.rooms[fixture.center]).unwrap();
        let complete = snapshot.areas().any(|area| {
            let discovered = area
                .rooms
                .iter()
                .find(|room| vnum(room) == Some(discovered_vnum));
            let origin = area
                .rooms
                .iter()
                .find(|room| vnum(room) == Some(origin_vnum));
            origin.is_some_and(|room| {
                room.exits.iter().any(|exit| {
                    exit.to_area_id == Some(area.area.id)
                        && discovered
                            .is_some_and(|target| exit.to_room_number == Some(target.room_number))
                })
            })
        });
        self.sink.record(
            "commit",
            json!({"generation":snapshot.generation,"chartComplete":complete,"observerMs":started.elapsed().as_secs_f64()*1000.0}),
        );
    }

    async fn pump_until(
        &mut self,
        fixture: &Fixture,
        limit: Instant,
        ready: impl Fn(&Sink) -> bool,
    ) {
        while Instant::now() < limit && !ready(&self.sink) {
            tokio::select! {
                event = self.events.next() => { if let Some(event) = event { self.event(event.event); } else { break; } }
                changed = self.commits.changed() => { if changed.is_ok() { self.commit(fixture); } }
                () = tokio::time::sleep(Duration::from_millis(5)) => {}
            }
        }
    }

    async fn shutdown(mut self, fixture: &Fixture) {
        self.sink.record("shutdown-request", json!({}));
        self.tx.send(RuntimeAction::Shutdown).unwrap();
        self.pump_until(fixture, Instant::now() + Duration::from_millis(100), |_| {
            false
        })
        .await;
        drop(self.tx);
        drop(self.events);
        let session_id = self.session_id;
        let joined = tokio::task::spawn_blocking(move || join_runtime_thread(session_id))
            .await
            .unwrap();
        self.sink.record(
            "shutdown-complete",
            json!({"clean":joined == RuntimeThreadJoinOutcome::Clean { session_id }}),
        );
    }
}

async fn load_fixture(root: &Path, key: &str, local: &LocalBackend, mapper: &Mapper) -> Fixture {
    let area = if key == "synthetic" {
        synthetic(mapper).await
    } else {
        let mut area: AreaWithDetails = serde_json::from_slice(
            &std::fs::read(below(root, &format!("areas/{key}.json"))).unwrap(),
        )
        .unwrap();
        // A case imports one loose area, not the whole player's atlas selection.
        area.area.atlas_id = None;
        local.import_local_area(area.clone()).await.unwrap();
        mapper.refresh_local_store().await.unwrap();
        area
    };
    let fixture = select(area);
    let removed = fixture.area.rooms[fixture.discovery].room_number;
    durable(
        mapper,
        mapper
            .delete_room(RoomKey::new(fixture.area.area.id, removed))
            .unwrap(),
    )
    .await;
    fixture
}

fn configure_case(
    root: &Path,
    case_dir: &Path,
    home: &Path,
    config: &Config,
    fixture: &Fixture,
    key: &str,
    mode: &str,
) -> String {
    let server = format!("PackingBench-{}", super::unique());
    let server_dir = super::create_below(home, &server);
    super::create_below(&server_dir, "modules");
    super::create_below(&server_dir, "logs");
    let package_records = packages::prepare(
        root,
        config,
        &server,
        matches!(mode, "quiet" | "quiet-topology"),
        vnum(&fixture.area.rooms[fixture.discovery]).unwrap(),
    );
    std::fs::write(
        case_dir.join("packages.json"),
        serde_json::to_vec_pretty(&package_records).unwrap(),
    )
    .unwrap();
    std::fs::write(
        server_dir.join("modules/probe.ts"),
        packages::main_probe(
            vnum(&fixture.area.rooms[fixture.discovery]).unwrap(),
            vnum(&fixture.area.rooms[fixture.center]).unwrap(),
            vnum(&fixture.area.rooms[fixture.known]).unwrap(),
        ),
    )
    .unwrap();
    std::fs::write(case_dir.join("inputs.json"), serde_json::to_vec_pretty(&json!({
        "synthesized":true,"areaKey":key,"center":room_info(fixture, fixture.center),
        "known":room_info(fixture, fixture.known),"discovery":room_info(fixture, fixture.discovery),
        "initialChart":chart(fixture, fixture.center, false),"topologyChart":chart(fixture, fixture.center, true),
    })).unwrap()).unwrap();
    server
}

pub async fn run_case(
    root: &Path,
    case_dir: &Path,
    home: &Path,
    config: &Config,
    key: &str,
    mode: &str,
) -> Value {
    let local = Arc::new(LocalBackend::new(case_dir.join("local")));
    let mapper = Mapper::new(local.clone(), case_dir.join("cache"));
    mapper.ready().await.unwrap();
    let fixture = load_fixture(root, key, &local, &mapper).await;
    let server = configure_case(root, case_dir, home, config, &fixture, key, mode);
    let sink = Arc::new(Sink::new());
    let mut host = Host::start(&server, &mapper, local, sink.clone()).await;
    host.pump_until(&fixture, Instant::now() + Duration::from_secs(5), |sink| {
        sink.has("probe-ready")
    })
    .await;
    assert!(sink.has("probe-ready"), "measurement module must load");
    host.input(&fixture, fixture.center, false, "warmup");
    host.pump_until(&fixture, Instant::now() + Duration::from_secs(5), |sink| {
        sink.initial_chart_ready()
    })
    .await;
    assert!(sink.initial_chart_ready(), "initial topology must publish");
    let scenario_start = Instant::now();
    let deadline = scenario_start + duration(config);
    if mode == "tidy" {
        host.command("nfmap tidy");
    }
    if mode == "perfect" {
        host.command("nf reflow perfect");
    }
    if mode != "baseline" {
        let barrier_seconds = if mode == "quiet-topology" { 7 } else { 3 };
        host.pump_until(
            &fixture,
            (scenario_start + Duration::from_secs(barrier_seconds))
                .min(deadline.checked_sub(Duration::from_millis(500)).unwrap()),
            |sink| sink.mode_active(mode),
        )
        .await;
    }
    exercise(&mut host, &fixture, mode, deadline).await;
    std::fs::write(
        case_dir.join("transcript.json"),
        serde_json::to_vec_pretty(&host.lines).unwrap(),
    )
    .unwrap();
    host.shutdown(&fixture).await;
    let samples = sink.snapshot();
    std::fs::write(
        case_dir.join("samples.json"),
        serde_json::to_vec_pretty(&samples).unwrap(),
    )
    .unwrap();
    let mut result = summarize(&samples, key, mode, &config.engine);
    result["nativeProbeCost"] = sink.probe_cost();
    std::fs::write(
        case_dir.join("summary.json"),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
    result
}

async fn exercise(host: &mut Host, fixture: &Fixture, mode: &str, deadline: Instant) {
    let start = Instant::now();
    host.sink.record(
        "measurement-start",
        json!({"active":host.sink.active_job(),"requestedModeActive":host.sink.mode_active(mode)}),
    );
    if mode == "quiet-topology" {
        if host.sink.mode_active(mode) {
            // No movement precedes discovery: interrupt Worker-reported repair.
            host.input(fixture, fixture.center, true, "topology");
            let input = host
                .sink
                .snapshot()
                .into_iter()
                .find(|sample| sample.kind == "input" && sample.data["label"] == "topology")
                .unwrap();
            assert!(quiet_active(&input.data["plannerAtInput"]));
            assert_eq!(input.data["plannerAtInput"]["status"], "repairing");
        } else {
            host.sink.record(
                "scenario-censored",
                json!({"reason":"quiet Worker repair was not observed before the barrier deadline"}),
            );
        }
    } else {
        for (offset, index, discovery, label) in [
            (0, fixture.known, false, "move-1"),
            (100, fixture.center, false, "move-2"),
            (200, fixture.known, false, "move-3"),
            (350, fixture.center, true, "topology"),
        ] {
            host.pump_until(fixture, start + Duration::from_millis(offset), |_| false)
                .await;
            host.input(fixture, index, discovery, label);
        }
    }
    if mode == "tidy" {
        host.pump_until(fixture, start + Duration::from_millis(500), |_| false)
            .await;
        host.command("nfmap stop");
    }
    host.pump_until(fixture, deadline, |_| false).await;
    host.sink.record(
        "measurement-end",
        json!({"active":host.sink.active_job(),"activePlanners":host.sink.active_jobs()}),
    );
}

fn quiet_active(job: &Value) -> bool {
    job["realm"] == "mapper"
        && job["source"] == "nukefire:auto-polish"
        && matches!(job["status"].as_str(), Some("planning" | "repairing"))
}

fn cancellation_metrics(samples: &[super::Sample], mode: &str, start: &super::Sample) -> Value {
    let quiet = matches!(mode, "quiet" | "quiet-topology");
    let quiet_label = if mode == "quiet-topology" {
        "topology"
    } else {
        "move-1"
    };
    let cancel = samples.iter().find(|sample| {
        (quiet && sample.kind == "input" && sample.data["label"] == quiet_label)
            || (mode == "tidy"
                && sample.kind == "command"
                && sample.data["command"] == "nfmap stop")
    });
    cancel.map_or(Value::Null, |input| {
        let job = if mode == "tidy" {
            &start.data["active"]
        } else {
            &input.data["plannerAtInput"]
        };
        let terminal = samples.iter().find(|sample| {
            sample.kind == "planner"
                && sample.ms >= input.ms
                && sample.data["status"] == "cancelled"
                && sample.data["realm"] == job["realm"]
                && sample.data["startedAt"] == job["startedAt"]
        });
        let last_commit = samples
            .iter()
            .rev()
            .find(|sample| sample.kind == "commit" && sample.ms >= input.ms);
        let stopped = samples.iter().find(|sample| {
            sample.ms >= input.ms
                && match mode {
                    "tidy" => sample.kind == "tidy-settled",
                    "quiet" | "quiet-topology" => {
                        sample.kind == "worker-cancel-settled"
                            && sample.data["reason"] == "AbortError"
                            && sample.data["realm"] == "mapper"
                    }
                    _ => false,
                }
        });
        json!({"plannerAtRequest":input.data["plannerAtInput"],"targetPlanner":job,
            "terminalDelayMs":terminal.map(|sample|sample.ms-input.ms),
            "requestToTidySettledMs":if mode=="tidy" { stopped.map(|sample|sample.ms-input.ms) } else { None },
            "firstObservedWorkerCancellationMs":if quiet { stopped.map(|sample|sample.ms-input.ms) } else { None },
            "workerCancellationIsFirstObservedAfterMovement":mode=="quiet",
            "workerCancellationIsFirstObservedAfterInput":quiet,
            "trigger":if mode=="quiet-topology" { "same-center topology input" } else if mode=="quiet" { "movement input" } else { "tidy stop command" },
            "lastObservedCommitDelayMs":last_commit.map(|sample|sample.ms-input.ms),
            "commitsCanIncludeSubsequentTopology":true})
    })
}

fn manual_metrics(samples: &[super::Sample], mode: &str, end: &super::Sample) -> Value {
    let manual = samples
        .iter()
        .find(|sample| sample.kind == "command" && sample.data["command"] == "nf reflow perfect");
    let reply = samples.iter().find(|sample| sample.kind == "manual-ack");
    let accepted = samples
        .iter()
        .find(|sample| sample.kind == "manual-ack" && sample.data["accepted"] == true);
    let perfect_running = end.data["activePlanners"]
        .as_array()
        .unwrap()
        .iter()
        .any(|job| job["realm"] == "scripts");
    json!({"perfectCommandToReplyMs":manual.zip(reply).map(|(input,sample)|sample.ms-input.ms),
        "perfectReplyAccepted":reply.map(|sample|&sample.data["accepted"]),
        "perfectCommandToDurableAckMs":manual.zip(accepted).map(|(input,sample)|sample.ms-input.ms),
        "perfectAckCensored":mode=="perfect" && accepted.is_none() && perfect_running,
        "perfectSearchCensored":mode=="perfect" && perfect_running})
}

fn summarize(all_samples: &[super::Sample], key: &str, mode: &str, engine: &str) -> Value {
    let start = all_samples
        .iter()
        .find(|sample| sample.kind == "measurement-start")
        .unwrap();
    let end = all_samples
        .iter()
        .find(|sample| sample.kind == "measurement-end")
        .unwrap();
    let bounded = all_samples
        .iter()
        .filter(|sample| sample.ms <= end.ms)
        .cloned()
        .collect::<Vec<_>>();
    let samples = bounded.as_slice();
    let movements = samples.iter().filter(|sample| sample.kind == "input" && sample.data["label"] != "warmup")
        .map(|input| {
            let next_input = samples.iter().find(|sample| sample.kind == "input" && sample.ms > input.ms).map_or(end.ms, |sample| sample.ms);
            let following = |kind: &str| samples.iter().find(|sample| sample.kind == kind && sample.ms >= input.ms && sample.ms < next_input
                && sample.data["vnum"] == input.data["vnum"]).map(|sample| sample.ms - input.ms);
            json!({"label":input.data["label"],"plannerAtInput":input.data["plannerAtInput"],
                "fixturePreparationMs":input.data["fixturePreparationMs"],
                "inputToLocationMs":following("location"),"inputToMarkerPublicationMs":following("marker-publication")})
        }).collect::<Vec<_>>();
    let topology = samples
        .iter()
        .find(|sample| sample.kind == "input" && sample.data["label"] == "topology");
    let topology_delay = |kind: &str| {
        topology.and_then(|input| {
            samples
                .iter()
                .find(|sample| {
                    sample.kind == kind
                        && sample.ms >= input.ms
                        && sample.data["chartComplete"] == true
                })
                .map(|sample| sample.ms - input.ms)
        })
    };
    let cancellation = cancellation_metrics(samples, mode, start);
    let heartbeat_max = samples
        .iter()
        .filter(|sample| sample.kind == "heartbeat" && sample.ms >= start.ms && sample.ms <= end.ms)
        .filter_map(|sample| sample.data["delayMs"].as_f64())
        .reduce(f64::max);
    let shutdown = all_samples
        .iter()
        .find(|sample| sample.kind == "shutdown-request")
        .unwrap();
    let stopped = all_samples
        .iter()
        .find(|sample| sample.kind == "shutdown-complete")
        .unwrap();
    let topology_synchronized = topology.and_then(|input| {
        samples
            .iter()
            .find(|sample| {
                sample.ms >= input.ms
                    && sample.kind == "snapshot-synchronized"
                    && sample.data["center"] == input.data["vnum"]
                    && sample.data["allowExistingReflow"] == false
                    && sample.data["containsDiscovery"] == true
            })
            .map(|sample| sample.ms - input.ms)
    });
    let mut result = json!({"areaKey":key,"engine":engine,"mode":mode,"movements":movements,
        "requestedModeActiveAtMeasurementStart":start.data["requestedModeActive"],
        "measurementWindowMs":end.ms-start.ms,"activePlannersAtDeadline":end.data["activePlanners"],
        "shutdownDelayMs":stopped.ms-shutdown.ms,
        "topologyPlannerAtInput":topology.map(|input|&input.data["plannerAtInput"]),
        "quietTopologyActiveAtInput":topology.is_some_and(|input|quiet_active(&input.data["plannerAtInput"])),
        "quietTopologyRepairAtInput":topology.is_some_and(|input|quiet_active(&input.data["plannerAtInput"])&&input.data["plannerAtInput"]["status"]=="repairing"),
        "scenarioCensored":samples.iter().filter(|sample|sample.kind=="scenario-censored").map(|sample|&sample.data).collect::<Vec<_>>(),
        "topologyToDurableCommitMs":topology_delay("commit"),"topologyToCompleteMapObservationMs":topology_delay("marker-publication"),
        "topologyToSnapshotSynchronizedMs":topology_synchronized,
        "cancellation":cancellation,"perfectUserCancellationSupported":false,
        "heartbeatMaxDelayMs":heartbeat_max,"hostPublicationOnly":true,"watchCommitsMayCoalesce":true,
        "publicationEvent":"map:room; complete chart requires discovered room and known exit",
        "commitObserverMaxMs":samples.iter().filter(|sample|sample.kind=="commit").filter_map(|sample|sample.data["observerMs"].as_f64()).reduce(f64::max),
        "workerConstructions":samples.iter().filter(|sample|sample.kind=="worker-create-end").map(|sample|json!({"atMs":sample.ms,"duringMeasurement":sample.ms>=start.ms&&sample.ms<=end.ms,"data":sample.data})).collect::<Vec<_>>(),
        "workerTerminals":samples.iter().filter(|sample|sample.kind=="worker-terminal").map(|sample|&sample.data).collect::<Vec<_>>(),
        "manualAcks":samples.iter().filter(|sample|sample.kind=="manual-ack").map(|sample|&sample.data).collect::<Vec<_>>(),
        "postWindowEvents":all_samples.iter().filter(|sample|sample.ms>end.ms).collect::<Vec<_>>()});
    result.as_object_mut().unwrap().extend(
        manual_metrics(samples, mode, end)
            .as_object()
            .unwrap()
            .clone(),
    );
    result
}

#[test]
fn same_center_topology_correlates_the_interrupted_quiet_job() {
    let sample = |ms, kind: &str, data| super::Sample {
        ms,
        kind: kind.into(),
        data,
    };
    let active = json!({"realm":"mapper","source":"nukefire:auto-polish","status":"repairing","phase":"constraint compaction","startedAt":7});
    let samples = vec![
        sample(
            0.0,
            "measurement-start",
            json!({"requestedModeActive":true,"active":active}),
        ),
        sample(
            2.0,
            "input",
            json!({"label":"topology","vnum":42,"plannerAtInput":active}),
        ),
        sample(
            2.5,
            "planner",
            json!({"realm":"mapper","status":"cancelled","startedAt":8}),
        ),
        sample(
            3.0,
            "planner",
            json!({"realm":"mapper","status":"cancelled","startedAt":7}),
        ),
        sample(
            4.0,
            "snapshot-synchronized",
            json!({"center":42,"allowExistingReflow":false,"containsDiscovery":true}),
        ),
        sample(4.5, "commit", json!({"chartComplete":true})),
        sample(
            5.0,
            "marker-publication",
            json!({"vnum":42,"chartComplete":true}),
        ),
        sample(6.0, "measurement-end", json!({"activePlanners":[]})),
        sample(7.0, "shutdown-request", json!({})),
        sample(8.0, "shutdown-complete", json!({"clean":true})),
    ];
    let result = summarize(&samples, "synthetic", "quiet-topology", "test");
    assert_eq!(result["quietTopologyActiveAtInput"], true);
    assert_eq!(result["quietTopologyRepairAtInput"], true);
    assert_eq!(result["topologyToSnapshotSynchronizedMs"], 2.0);
    assert_eq!(result["topologyToDurableCommitMs"], 2.5);
    assert_eq!(result["topologyToCompleteMapObservationMs"], 3.0);
    assert_eq!(result["cancellation"]["terminalDelayMs"], 1.0);
    assert_eq!(result["cancellation"]["targetPlanner"]["startedAt"], 7);
    assert_eq!(
        result["cancellation"]["trigger"],
        "same-center topology input"
    );
}

#[test]
fn summaries_keep_rejected_and_post_deadline_replies_out_of_durable_ack_latency() {
    let sample = |ms, kind: &str, data| super::Sample {
        ms,
        kind: kind.into(),
        data,
    };
    let samples = vec![
        sample(0.0, "command", json!({"command":"nf reflow perfect"})),
        sample(
            1.0,
            "measurement-start",
            json!({"requestedModeActive":true,"active":{"realm":"mapper"}}),
        ),
        sample(2.0, "input", json!({"label":"topology","vnum":42})),
        sample(
            2.5,
            "snapshot-synchronized",
            json!({"center":42,"allowExistingReflow":false,"containsDiscovery":false}),
        ),
        sample(3.0, "manual-ack", json!({"accepted":false})),
        sample(
            5.0,
            "measurement-end",
            json!({"activePlanners":[{"realm":"mapper"},{"realm":"scripts"}]}),
        ),
        sample(6.0, "manual-ack", json!({"accepted":true})),
        sample(7.0, "commit", json!({"chartComplete":true})),
        sample(
            8.0,
            "snapshot-synchronized",
            json!({"center":42,"allowExistingReflow":false,"containsDiscovery":true}),
        ),
        sample(
            9.0,
            "marker-publication",
            json!({"vnum":42,"chartComplete":true}),
        ),
        sample(10.0, "shutdown-request", json!({})),
        sample(11.0, "shutdown-complete", json!({"clean":true})),
    ];
    let result = summarize(&samples, "synthetic", "perfect", "test");
    assert_eq!(result["perfectCommandToReplyMs"], 3.0);
    assert_eq!(result["perfectReplyAccepted"], false);
    assert!(result["perfectCommandToDurableAckMs"].is_null());
    assert_eq!(result["perfectSearchCensored"], true);
    assert!(result["topologyToDurableCommitMs"].is_null());
    assert!(result["topologyToCompleteMapObservationMs"].is_null());
    assert!(result["topologyToSnapshotSynchronizedMs"].is_null());
    assert_eq!(result["postWindowEvents"].as_array().unwrap().len(), 6);
}
