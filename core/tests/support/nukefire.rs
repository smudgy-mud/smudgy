//! Running the authored `nukefire-mapper` package end to end. The real mapper
//! and map-layout sources are installed under a temporary smudgy home and run
//! sandboxed under their manifests. A minimal local `nukefire-gmcp` fixture
//! exposes the same retained-tree and per-message helpers the mapper consumes,
//! so a test drives the mapper by sending the GMCP messages the game would.

use std::{
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, OnceLock},
    time::Duration,
};

use futures::{Stream, StreamExt};
use serde_json::json;
use smudgy_cloud::{
    CloudMapper, CompositeBackend, Credential, CredentialSource, LocalBackend, Mapper,
    MapperBackend, PackageApiClient,
};
use smudgy_core::models::local_packages::packages_dir;
use smudgy_core::models::shared_packages::{self, UpdateMode};
use smudgy_core::session::runtime::RuntimeAction;
use smudgy_core::session::{
    BufferUpdate, SessionEvent, SessionId, SessionParams, TaggedSessionEvent, spawn,
};
use tokio::sync::mpsc::UnboundedSender;

/// How long a test waits for the mapper to reach an expected state.
pub const MAP_WAIT: Duration = Duration::from_secs(15);
/// The mapper package as installed from the local package directory.
pub const MAPPER_SPEC: &str = "smudgy://local/nukefire-mapper";
/// The profile every session runs under.
pub const PROFILE: &str = "Test";

/// The prefix of every line the mapper prints.
pub const NOTICE_PREFIX: &str = "[nukefire-mapper] ";

/// The smudgy home every test in this process shares. The override is
/// process-wide and set once, so tests keep apart by server name and by the
/// directory their maps live in.
pub fn smudgy_home() -> PathBuf {
    static HOME: OnceLock<PathBuf> = OnceLock::new();
    HOME.get_or_init(|| {
        let home = tempfile::tempdir().expect("create temp home");
        let home_path = home.path().to_path_buf();
        std::mem::forget(home);
        smudgy_core::set_smudgy_home(&home_path);
        smudgy_core::get_smudgy_home().expect("smudgy home")
    })
    .clone()
}

pub fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in std::fs::read_dir(root).ok()? {
        let entry = entry.ok()?;
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|candidate| candidate == name) {
            return Some(path);
        }
    }
    None
}

fn copy_package(server: &str, name: &str) {
    let source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("packages")
        .join(name);
    let destination = packages_dir(server).expect("packages dir").join(name);
    std::fs::create_dir_all(&destination).expect("create package directory");
    for entry in std::fs::read_dir(&source).unwrap_or_else(|_| panic!("read package {name}")) {
        let entry = entry.expect("package entry");
        if entry.file_type().expect("entry type").is_file() {
            std::fs::copy(entry.path(), destination.join(entry.file_name()))
                .expect("copy package source");
        }
    }
}

fn localize_mapper_dependencies(server: &str) {
    let directory = packages_dir(server)
        .expect("packages dir")
        .join("nukefire-mapper");
    for entry in std::fs::read_dir(&directory).expect("read mapper package") {
        let entry = entry.expect("mapper package entry");
        let path = entry.path();
        let is_source = path.extension().is_some_and(|extension| extension == "ts");
        let is_manifest = path
            .file_name()
            .is_some_and(|name| name == "smudgy.package.json");
        if !is_source && !is_manifest {
            continue;
        }
        let source = std::fs::read_to_string(&path).expect("read mapper source");
        let localized = source
            .replace(
                "smudgy://kapusniak/nukefire-gmcp",
                "smudgy://local/nukefire-gmcp",
            )
            .replace("smudgy://kapusniak/map-layout", "smudgy://local/map-layout");
        std::fs::write(path, localized).expect("localize mapper dependency");
    }
}

fn write_gmcp_fixture(server: &str) {
    let directory = packages_dir(server)
        .expect("packages dir")
        .join("nukefire-gmcp");
    std::fs::create_dir_all(&directory).expect("create GMCP fixture");
    std::fs::write(
        directory.join("smudgy.package.json"),
        r#"{
          "version": "0.0.0-test",
          "entry": "index.ts",
          "permissions": { "smudgy": { "interop": ["read"] } }
        }"#,
    )
    .expect("write GMCP fixture manifest");
    std::fs::write(
        directory.join("index.ts"),
        r#"import gmcp from "smudgy:state/gmcp";
export const nukefire = gmcp;
export function watchMessage(name: string, handler: (payload: any) => void) {
  return nukefire.watch(name, handler);
}
export function onMessage(name: string, handler: (payload: any) => void) {
  return nukefire.onWrite(name, (path: string, snapshot: any) => {
    if (path.toLowerCase() === name.toLowerCase() && snapshot !== undefined) handler(snapshot);
  });
}
"#,
    )
    .expect("write GMCP fixture source");
}

/// Installs the mapper, map-layout and the GMCP fixture for `server`.
pub fn install_mapper_packages(server: &str) {
    let home = smudgy_home();
    std::fs::create_dir_all(home.join(server).join("modules")).unwrap();
    std::fs::create_dir_all(home.join(server).join("logs")).unwrap();
    copy_package(server, "map-layout");
    copy_package(server, "nukefire-mapper");
    write_gmcp_fixture(server);
    localize_mapper_dependencies(server);
    shared_packages::install_package(server, MAPPER_SPEC, UpdateMode::Auto, true)
        .expect("install NukeFire mapper");
}

/// A mapper whose local tier lives in `map_root/local`. Its cloud tier points
/// nowhere and is never reached.
pub fn local_mapper(map_root: &Path) -> Mapper {
    let local = Arc::new(LocalBackend::new(map_root.join("local")));
    let cloud = Arc::new(CloudMapper::new(
        "http://127.0.0.1:0".to_string(),
        "test-key".to_string(),
    ));
    let backend: Arc<dyn MapperBackend + Send + Sync> =
        Arc::new(CompositeBackend::new(local, cloud));
    Mapper::new(backend, map_root.join("cache"))
}

/// The events a running session reports.
pub type Events = Pin<Box<dyn Stream<Item = TaggedSessionEvent>>>;

/// A running session and the lines it has printed so far.
pub struct Session {
    pub tx: UnboundedSender<RuntimeAction>,
    pub events: Events,
    pub lines: Vec<String>,
}

/// Starts a session for `server` over `mapper`, waits until its runtime is
/// ready and turns GMCP on, as a connection to the game would.
pub async fn start_session(server: &str, session_id: u32, mapper: &Mapper) -> Session {
    let params = Arc::new(SessionParams {
        session_id: SessionId::from(session_id),
        server_name: Arc::new(server.to_string()),
        profile_name: Arc::new(PROFILE.to_string()),
        profile_subtext: Arc::new(String::new()),
        mapper: Some(mapper.clone()),
        package_client: Some(PackageApiClient::new(
            "http://127.0.0.1:0",
            CredentialSource::new(Some(Credential::ApiKey("test".into()))),
        )),
        extra_script_extensions: Arc::new(Vec::new),
        on_engine_rebuild: None,
    });

    let mut events: Events = Box::pin(spawn(params));
    let mut lines = Vec::new();
    let tx = loop {
        let event = tokio::time::timeout(Duration::from_mins(1), events.next())
            .await
            .expect("timed out waiting for RuntimeReady")
            .expect("event stream ended before RuntimeReady");
        match event.event {
            SessionEvent::RuntimeReady(tx) => break tx,
            SessionEvent::UpdateBuffer(updates) => collect(&updates, &mut lines),
            _ => {}
        }
    };
    tx.send(RuntimeAction::GmcpEnabled).unwrap();
    Session { tx, events, lines }
}

impl Session {
    /// Delivers one GMCP message, as the game would send it.
    pub fn gmcp(&self, name: &str, data: &str) {
        self.tx
            .send(gmcp(name, data))
            .expect("the session is running");
    }

    /// Submits `line` as the player would type it, aliases first.
    pub fn send(&self, line: &str) {
        self.tx
            .send(RuntimeAction::Send(Arc::new(line.to_string())))
            .expect("the session is running");
    }

    /// Stands the player in `room` of the area the game calls `area`: its
    /// `Room.Info`, then a `NukeFire.Map.Local` chart centered on it that also
    /// shows the rooms of `chart` and `links`.
    pub fn visit(
        &self,
        area: &str,
        room: &ChartRoom<'_>,
        chart: &[ChartRoom<'_>],
        links: &[ChartLink<'_>],
    ) {
        self.gmcp("Room.Info", &room_info(room, area));
        self.gmcp("NukeFire.Map.Local", &map_local(room, chart, links));
    }

    /// Pumps events until `ready` holds for the printed lines, or `MAP_WAIT` passes.
    pub async fn wait_until<F>(&mut self, ready: F) -> bool
    where
        F: Fn(&[String]) -> bool,
    {
        wait_until(&mut self.events, &mut self.lines, ready).await
    }

    /// Pumps events for `duration`, collecting what the session prints.
    pub async fn run_for(&mut self, duration: Duration) {
        wait_until_within(&mut self.events, &mut self.lines, |_| false, duration).await;
    }

    /// Everything the session printed so far, one line each.
    pub fn transcript(&self) -> String {
        self.lines.join("\n")
    }

    /// The lines the mapper printed, without their prefix.
    pub fn notices(&self) -> Vec<&str> {
        self.lines
            .iter()
            .filter_map(|line| line.strip_prefix(NOTICE_PREFIX))
            .collect()
    }

    pub fn shutdown(&self) {
        self.tx.send(RuntimeAction::Shutdown).ok();
    }
}

pub fn gmcp(name: &str, data: &str) -> RuntimeAction {
    RuntimeAction::GmcpMessage {
        name: Arc::from(name),
        data: Some(Arc::from(data)),
    }
}

pub fn collect(updates: &[BufferUpdate], lines: &mut Vec<String>) {
    for update in updates {
        if let BufferUpdate::Append(line) = update {
            lines.push(line.text.clone());
        }
    }
}

/// Pumps the session's events into `lines` until `ready` holds for them, or
/// `MAP_WAIT` passes. Returns whether `ready` held.
pub async fn wait_until<S, F>(events: &mut S, lines: &mut Vec<String>, ready: F) -> bool
where
    S: Stream<Item = TaggedSessionEvent> + Unpin,
    F: Fn(&[String]) -> bool,
{
    wait_until_within(events, lines, ready, MAP_WAIT).await
}

/// [`wait_until`] with a limit of `wait` instead of `MAP_WAIT`.
pub async fn wait_until_within<S, F>(
    events: &mut S,
    lines: &mut Vec<String>,
    ready: F,
    wait: Duration,
) -> bool
where
    S: Stream<Item = TaggedSessionEvent> + Unpin,
    F: Fn(&[String]) -> bool,
{
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        if ready(lines) {
            return true;
        }
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        let remaining = deadline - now;
        match tokio::time::timeout(remaining.min(Duration::from_millis(100)), events.next()).await {
            Ok(Some(event)) => {
                if let SessionEvent::UpdateBuffer(updates) = event.event {
                    collect(&updates, lines);
                }
            }
            Ok(None) => return ready(lines),
            Err(_) => {}
        }
    }
}

/// [`wait_until`] for a condition on the maps rather than the printed lines.
pub async fn wait_for_map_state<S, F>(events: &mut S, lines: &mut Vec<String>, ready: F) -> bool
where
    S: Stream<Item = TaggedSessionEvent> + Unpin,
    F: Fn() -> bool,
{
    wait_until(events, lines, |_| ready()).await
}

/// One room of a `NukeFire.Map.Local` chart, in the game's coordinates.
#[derive(Debug, Clone, Copy)]
pub struct ChartRoom<'a> {
    pub vnum: u64,
    pub name: &'a str,
    pub zone: i64,
    pub terrain: &'a str,
    pub x: i64,
    pub y: i64,
    pub z: i64,
}

/// A walkable connection the chart shows from one room to another.
#[derive(Debug, Clone, Copy)]
pub struct ChartLink<'a> {
    pub from: u64,
    pub to: u64,
    pub direction: &'a str,
}

/// The `Room.Info` the game sends for the room the player stands in.
pub fn room_info(room: &ChartRoom<'_>, area: &str) -> String {
    json!({
        "num": room.vnum, "name": room.name, "area": area,
        "zone": room.zone, "terrain": room.terrain, "exits": {},
        "coords": { "x": room.x, "y": room.y, "z": room.z }
    })
    .to_string()
}

/// The `NukeFire.Map.Local` chart around `center`; `chart` holds its other rooms.
pub fn map_local(
    center: &ChartRoom<'_>,
    chart: &[ChartRoom<'_>],
    links: &[ChartLink<'_>],
) -> String {
    let room = |room: &ChartRoom<'_>, current: bool| {
        json!({
            "vnum": room.vnum, "name": room.name, "zone": room.zone,
            "terrain": room.terrain, "x": room.x, "y": room.y, "z": room.z,
            "current": current, "route": false, "destination": false
        })
    };
    let rooms: Vec<_> = std::iter::once(room(center, true))
        .chain(chart.iter().map(|other| room(other, false)))
        .collect();
    let links: Vec<_> = links
        .iter()
        .map(|link| {
            json!({
                "from": link.from, "to": link.to, "direction": link.direction,
                "bidirectional": true, "closed": false, "locked": false, "route": false
            })
        })
        .collect();
    json!({
        "version": 1, "source": "bigmap+gps", "center": center.vnum,
        "zone": center.zone, "plane": 0,
        "rooms": rooms,
        "links": links,
        "gps": {
            "active": false, "type": "none", "target": -1,
            "description": "", "steps": 0, "route_raw": ""
        },
        "truncated": false
    })
    .to_string()
}
