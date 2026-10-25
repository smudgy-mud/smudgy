//! Loading the maps a session shows. The maps on this device show at once;
//! the cloud maps follow as the map service serves them, a few Libraries at
//! a time, and are shown together every [`PUBLISH_EVERY`]. A map the service
//! can't serve yet is retried on its terms ([`super::retry`]); one that
//! stops retrying waits for [`Inner::retry_loading`].
//!
//! A load runs as a background task, so a session that stops waiting for it
//! keeps its loading. The task holds the mapper weakly and ends with it.

use std::{
    collections::{HashMap, HashSet},
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use chrono::{DateTime, Utc};
use log::warn;
use parking_lot::Mutex;
use tokio::{
    sync::{Notify, watch},
    task::JoinSet,
    time::Instant,
};

use super::{
    AreaCache, AreaLoadStat, Inner, LoadMapsSummary, ReplayMode,
    fetcher::Library,
    local_projection,
    retry::{self, Failures},
};
use crate::{Area, AreaId, AreaWithDetails, CloudError, CloudResult, backends::MapperBackend};

/// How long a session waits for its maps before starting with those loaded.
pub(super) const INITIAL_LOAD_WAIT: Duration = Duration::from_secs(20);
/// How long fetched maps gather before they're shown together.
const PUBLISH_EVERY: Duration = Duration::from_millis(250);
/// The longest a wait for every map, or for one, lasts past the session's
/// start: a map the service keeps answering busy keeps loading after it.
pub(super) const READY_WAIT_LIMIT: Duration = retry::RETRY_WINDOW;

type Backend = Arc<dyn MapperBackend + Send + Sync>;

/// The cloud maps the latest load means to show, and those that stopped
/// trying.
#[derive(Default)]
struct Inventory {
    /// As the map service listed them or, while it can't, as it last did.
    rows: HashMap<AreaId, Area>,
    /// The rows stand in for a list the map service couldn't give.
    remembered: bool,
    /// Why each map that stopped trying couldn't load.
    stopped: HashMap<AreaId, CloudError>,
}

/// How many arrivals wait for a slow listener before the oldest are dropped.
const ARRIVALS_KEPT: usize = 64;

/// The mapper's loads: the latest one's progress and its task.
pub(super) struct Loads {
    progress: watch::Sender<LoadMapsSummary>,
    inventory: Mutex<Inventory>,
    /// Maps newly shown by a load or a sync, in batches.
    arrivals: tokio::sync::broadcast::Sender<Arc<[AreaId]>>,
    task: Mutex<Option<tokio::task::AbortHandle>>,
    /// When the first load began: the start of the session's wait for maps.
    first_started: Mutex<Option<Instant>>,
    /// When the session stopped waiting for its maps, or will.
    session_deadline: Mutex<Option<Instant>>,
    /// The session's wait for its maps has ended: maps shown from now on
    /// arrive.
    session_started: AtomicBool,
    /// Brings the running load's retries forward, or restarts what stopped.
    retry_now: Arc<Notify>,
    /// How long failures other than 503s keep retrying on their own.
    pub(super) retry_window: Mutex<Duration>,
}

impl Loads {
    pub(super) fn new() -> Self {
        Self {
            progress: watch::channel(LoadMapsSummary::default()).0,
            inventory: Mutex::new(Inventory::default()),
            arrivals: tokio::sync::broadcast::channel(ARRIVALS_KEPT).0,
            task: Mutex::new(None),
            first_started: Mutex::new(None),
            session_deadline: Mutex::new(None),
            session_started: AtomicBool::new(false),
            retry_now: Arc::new(Notify::new()),
            retry_window: Mutex::new(retry::RETRY_WINDOW),
        }
    }

    pub(super) fn abort(&mut self) {
        if let Some(task) = self.task.get_mut().take() {
            task.abort();
        }
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<LoadMapsSummary> {
        self.progress.subscribe()
    }

    pub(super) fn subscribe_arrivals(&self) -> tokio::sync::broadcast::Receiver<Arc<[AreaId]>> {
        self.arrivals.subscribe()
    }

    /// Announces maps newly shown.
    pub(super) fn note_arrivals(&self, area_ids: Vec<AreaId>) {
        if !area_ids.is_empty() {
            // Nobody listening drops them, as it should.
            let _ = self.arrivals.send(Arc::from(area_ids));
        }
    }

    fn set_rows(&self, rows: impl IntoIterator<Item = Area>, remembered: bool) {
        let mut inventory = self.inventory.lock();
        inventory.rows = rows.into_iter().map(|row| (row.id, row)).collect();
        inventory.remembered = remembered;
        inventory.stopped.clear();
    }

    fn stopped_reason(&self, area_id: AreaId) -> Option<CloudError> {
        self.inventory.lock().stopped.get(&area_id).cloned()
    }

    fn note_stopped(&self, area_id: AreaId, err: CloudError) {
        self.inventory.lock().stopped.insert(area_id, err);
    }

    fn clear_stopped(&self, area_id: AreaId) {
        self.inventory.lock().stopped.remove(&area_id);
    }

    /// Every listed map stops trying with the list's `err`, or, with
    /// `None`, tries again.
    fn stop_all(&self, err: Option<&CloudError>) {
        let mut inventory = self.inventory.lock();
        inventory.stopped = match err {
            Some(err) => inventory.rows.keys().map(|id| (*id, err.clone())).collect(),
            None => HashMap::new(),
        };
    }

    fn forget(&self, area_id: AreaId) {
        let mut inventory = self.inventory.lock();
        inventory.rows.remove(&area_id);
        inventory.stopped.remove(&area_id);
    }

    /// The names the cloud maps have, as listed or, while the map service
    /// can't list them, as it last did.
    fn names(&self) -> Arc<HashSet<String>> {
        let inventory = self.inventory.lock();
        Arc::new(
            inventory
                .rows
                .values()
                .map(|row| row.name.clone())
                .collect(),
        )
    }

    fn running(&self) -> bool {
        self.task
            .lock()
            .as_ref()
            .is_some_and(|task| !task.is_finished())
    }
}

/// Whether the session's wait for its maps is over.
fn session_can_start(summary: &LoadMapsSummary) -> bool {
    let unreachable = summary
        .cloud_unavailable
        .as_ref()
        .is_some_and(|err| !matches!(err, CloudError::ServiceUnavailable { .. }));
    summary.settled || (summary.first_pass && unreachable)
}

/// What one load has done so far.
struct Tally {
    load: u64,
    list_duration: Duration,
    stats: Vec<AreaLoadStat>,
    loaded: HashSet<AreaId>,
    /// Every map the load means to show.
    expected: HashSet<AreaId>,
    /// Whether `expected` names the cloud maps: the cloud list answered, or
    /// a remembered inventory stands in for it.
    cloud_counted: bool,
    cloud_unavailable: Option<CloudError>,
    failure: Option<CloudError>,
    first_pass: bool,
    settled: bool,
    stopped: usize,
    duplicates: Vec<String>,
    fatal: Option<CloudError>,
}

impl Tally {
    fn new(load: u64) -> Self {
        Self {
            load,
            list_duration: Duration::ZERO,
            stats: Vec::new(),
            loaded: HashSet::new(),
            expected: HashSet::new(),
            cloud_counted: false,
            cloud_unavailable: None,
            failure: None,
            first_pass: false,
            settled: false,
            stopped: 0,
            duplicates: Vec::new(),
            fatal: None,
        }
    }

    fn summary(&self) -> LoadMapsSummary {
        LoadMapsSummary {
            list_duration: self.list_duration,
            areas: self.stats.clone(),
            cloud_unavailable: self.cloud_unavailable.clone(),
            load: self.load,
            total: self.cloud_counted.then_some(self.expected.len()),
            first_pass: self.first_pass,
            settled: self.settled,
            stopped: self.stopped,
            failure: self.failure.clone(),
            duplicates: self.duplicates.clone(),
            fatal: self.fatal.clone(),
        }
    }

    fn note_loaded(
        &mut self,
        inner: &Inner,
        details: &AreaWithDetails,
        load_duration: Duration,
        shared: bool,
    ) {
        let id = details.area.id;
        self.expected.insert(id);
        if self.loaded.insert(id) {
            self.stats.push(AreaLoadStat {
                area_id: id,
                name: details.area.name.clone(),
                revision: details.area.rev,
                load_duration,
                source: inner.backend.last_area_source(&id),
                shared,
            });
        }
    }
}

/// The projection token of the view of a map shown, if one is shown.
type ShownView = Option<Option<String>>;

/// A fetched map: how long it took, and the view of it shown when it was
/// asked for.
struct Arrival {
    details: AreaWithDetails,
    took: Duration,
    shown_before: ShownView,
}

/// A fetch's answer.
type Fetched = (AreaId, CloudResult<Arrival>);

impl Inner {
    /// Starts loading every map, and returns which load that is. Unless
    /// `restart`, a load already running or one that already succeeded is
    /// kept; a restart drops the running load and lists again.
    pub(super) fn start_load(self: &Arc<Self>, restart: bool) -> u64 {
        let mut task = self.loads.task.lock();
        let current = self.loads.progress.borrow().clone();
        let running = task.as_ref().is_some_and(|task| !task.is_finished());
        if !restart && current.load > 0 && (running || current.fatal.is_none()) {
            return current.load;
        }
        if let Some(task) = task.take() {
            task.abort();
        }
        self.loads
            .first_started
            .lock()
            .get_or_insert_with(Instant::now);
        let load = current.load + 1;
        self.loads.progress.send_replace(LoadMapsSummary {
            load,
            ..LoadMapsSummary::default()
        });
        let weak = Arc::downgrade(self);
        *task = Some(tokio::spawn(run(weak, load)).abort_handle());
        load
    }

    /// Loads every map again and waits for each to have had a first try.
    pub(super) async fn load_all_areas(self: &Arc<Self>) -> CloudResult<LoadMapsSummary> {
        let load = self.start_load(true);
        self.wait_for_load(load, |summary| summary.first_pass, None)
            .await
    }

    /// The session's wait for its maps: starts or joins the first load and
    /// waits until nothing more will load without a retry, or until
    /// `timeout` after the first load began. A cloud list that failed for
    /// any reason but a busy service ends the wait at once: a service that
    /// can't be reached at all leaves the session as if signed out, with
    /// the maps on this device, while the load keeps trying.
    pub(super) async fn load_initial_areas(
        self: &Arc<Self>,
        timeout: Duration,
    ) -> CloudResult<LoadMapsSummary> {
        let load = self.start_load(false);
        let deadline = self
            .loads
            .first_started
            .lock()
            .map(|started| started + timeout);
        if let Some(deadline) = deadline {
            let mut session = self.loads.session_deadline.lock();
            *session = Some(session.map_or(deadline, |earlier| earlier.min(deadline)));
        }
        let started = self.wait_for_load(load, session_can_start, deadline).await;
        if started.is_ok() {
            self.loads.session_started.store(true, Ordering::Release);
        }
        started
    }

    /// Waits for load `load` to reach `until`, or for `deadline`, and
    /// returns its progress; a load that ended early is its error.
    async fn wait_for_load(
        &self,
        load: u64,
        until: impl Fn(&LoadMapsSummary) -> bool,
        deadline: Option<Instant>,
    ) -> CloudResult<LoadMapsSummary> {
        let mut progress = self.loads.subscribe();
        loop {
            {
                let summary = progress.borrow_and_update();
                if summary.load == load {
                    if let Some(fatal) = &summary.fatal {
                        return Err(fatal.clone());
                    }
                    if until(&summary) || deadline.is_some_and(|at| at <= Instant::now()) {
                        return Ok(summary.clone());
                    }
                } else if summary.load > load {
                    // A later load replaced this one; it answers instead.
                    return Ok(summary.clone());
                }
            }
            let changed = async {
                if progress.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            };
            match deadline {
                Some(at) => {
                    tokio::select! {
                        () = changed => {}
                        () = tokio::time::sleep_until(at) => {}
                    }
                }
                None => changed.await,
            }
        }
    }

    /// Waits until the maps are known well enough to tell whether a name is
    /// taken: the first load's every map has had a try and the cloud list
    /// answered. Returns the names the cloud list gave.
    ///
    /// # Errors
    /// The cloud list's failure; a timeout once the session stopped waiting
    /// for its maps before that; or what ended the load.
    pub(super) async fn wait_for_known_maps(&self) -> CloudResult<Arc<HashSet<String>>> {
        let mut progress = self.loads.subscribe();
        loop {
            let deadline = *self.loads.session_deadline.lock();
            {
                let summary = progress.borrow_and_update();
                if summary.load > 0 {
                    if let Some(fatal) = &summary.fatal {
                        return Err(fatal.clone());
                    }
                    if summary.first_pass {
                        // While the map service can't list the cloud maps,
                        // the last list it gave answers; with none, nothing
                        // can say a name is free.
                        let remembered = self.loads.inventory.lock().remembered;
                        return match &summary.cloud_unavailable {
                            Some(err) if !remembered => Err(err.clone()),
                            _ => Ok(self.loads.names()),
                        };
                    }
                    if deadline.is_some_and(|at| at <= Instant::now()) {
                        return Err(CloudError::NetworkError("map loading timed out".into()));
                    }
                }
            }
            let changed = async {
                if progress.changed().await.is_err() {
                    std::future::pending::<()>().await;
                }
            };
            match deadline {
                Some(at) => {
                    tokio::select! {
                        () = changed => {}
                        () = tokio::time::sleep_until(at) => {}
                    }
                }
                None => changed.await,
            }
        }
    }

    /// Announces maps newly shown, waking the sessions that listen.
    pub(super) fn note_arrivals(&self, area_ids: Vec<AreaId>) {
        // Maps shown before the session's wait for them ended are what
        // `ready()` covers; only later ones arrive.
        if !area_ids.is_empty() && self.loads.session_started.load(Ordering::Acquire) {
            self.loads.note_arrivals(area_ids);
            self.pending.notify_event_consumers();
        }
    }

    /// Waits until nothing more is coming: every map loaded, or the rest
    /// stopped trying; [`READY_WAIT_LIMIT`] at most.
    pub(super) async fn ready_all(self: &Arc<Self>) -> CloudResult<()> {
        let load = self.start_load(false);
        let deadline = Instant::now() + READY_WAIT_LIMIT;
        self.wait_for_load(load, |summary| summary.settled, Some(deadline))
            .await
            .map(|_| ())
    }

    /// Waits until `area_id` has loaded, stopped trying, or turned out not to
    /// be a cloud map; [`READY_WAIT_LIMIT`] at most.
    pub(super) async fn ready_for(&self, area_id: AreaId) {
        // Whichever way the wait ends, the map is as ready as it will be.
        let _ = tokio::time::timeout(READY_WAIT_LIMIT, self.wait_for_area(area_id)).await;
    }

    /// Why `area_id`, a cloud map still to load, stopped trying, if it did.
    pub(super) fn area_load_failure(&self, area_id: AreaId) -> Option<CloudError> {
        if self.atlas_cache.load().get_area(&area_id).is_some() {
            return None;
        }
        self.loads.stopped_reason(area_id)
    }

    /// Whether `area_id` is a cloud map still to load: listed, and not shown.
    pub(super) fn area_is_loading(&self, area_id: AreaId) -> bool {
        self.loads.inventory.lock().rows.contains_key(&area_id)
            && self.atlas_cache.load().get_area(&area_id).is_none()
    }

    /// A cloud map still to load as an area with its list row and nothing
    /// else, or `None` when `area_id` names none.
    pub(super) fn loading_area(&self, area_id: AreaId) -> Option<Arc<AreaCache>> {
        if self.atlas_cache.load().get_area(&area_id).is_some() {
            return None;
        }
        let row = self.loads.inventory.lock().rows.get(&area_id).cloned()?;
        Some(self.placeholder(row))
    }

    /// Every cloud map still to load that takes part here: not turned off,
    /// and not kept for another server.
    pub(super) fn loading_areas(&self) -> Vec<Arc<AreaCache>> {
        let atlas = self.atlas_cache.load();
        let rows: Vec<Area> = self
            .loads
            .inventory
            .lock()
            .rows
            .values()
            .filter(|row| atlas.get_area(&row.id).is_none())
            .cloned()
            .collect();
        rows.into_iter()
            .map(|row| self.placeholder(row))
            .filter(|area| atlas.includes_unloaded(area))
            .collect()
    }

    fn placeholder(&self, row: Area) -> Arc<AreaCache> {
        let viewer = self.pending.active_viewer().map(|(viewer, _)| viewer);
        let details = AreaWithDetails {
            area: row,
            format_version: crate::AREA_FORMAT_VERSION,
            properties: Vec::new(),
            rooms: Vec::new(),
            room_data: Vec::new(),
            labels: Vec::new(),
            shapes: Vec::new(),
            connections: Vec::new(),
            linked_areas: Vec::new(),
            sources: Vec::new(),
        };
        Arc::new(AreaCache::for_viewer(details, viewer, None))
    }

    /// Waits until `area_id` has loaded.
    ///
    /// # Errors
    /// Why the map stopped trying, or `AreaNotFound` once the cloud list
    /// answered without it.
    pub(super) async fn wait_for_area(&self, area_id: AreaId) -> CloudResult<()> {
        let mut progress = self.loads.subscribe();
        loop {
            if self.atlas_cache.load().get_area(&area_id).is_some() {
                return Ok(());
            }
            {
                let inventory = self.loads.inventory.lock();
                if let Some(err) = inventory.stopped.get(&area_id) {
                    return Err(err.clone());
                }
                if !inventory.rows.contains_key(&area_id) {
                    // Until a list names the cloud maps, any id may be one.
                    let summary = progress.borrow_and_update();
                    let listing = summary.load == 0
                        || (summary.total.is_none() && !summary.settled && summary.fatal.is_none());
                    if !listing {
                        return Err(CloudError::AreaNotFound(area_id));
                    }
                }
            }
            // A sync shows maps too, without a load's report: look again soon.
            tokio::select! {
                result = progress.changed() => {
                    if result.is_err() {
                        return Err(CloudError::AreaNotFound(area_id));
                    }
                }
                () = tokio::time::sleep(Duration::from_millis(250)) => {}
            }
        }
    }

    /// Tries again, now, every map still waiting to load and every one that
    /// stopped trying; with no load running, loads every map again.
    pub(super) fn retry_loading(self: &Arc<Self>) {
        if self.loads.running() {
            self.loads.retry_now.notify_one();
        } else {
            self.start_load(true);
        }
    }

    fn report(&self, tally: &Tally) {
        self.loads.progress.send_if_modified(|summary| {
            if summary.load != tally.load {
                return false;
            }
            *summary = tally.summary();
            true
        });
        self.pending.notify_event_consumers();
    }

    /// Shows the maps on this device: the local ones through the local
    /// projection, counted as loaded at once.
    fn show_device_maps(&self, tally: &mut Tally) {
        let _gate = self.mutation_gate.lock();
        if let Some(local) = self.backend.local_snapshot() {
            self.local_projection
                .lock()
                .known
                .extend(local.areas().map(|details| details.area.id));
        }
        self.count_local_maps(tally);
        local_projection::adopt_locked(self);
    }

    /// Counts every local map as loaded, those made since the load began
    /// with the rest: the local projection shows them all.
    fn count_local_maps(&self, tally: &mut Tally) {
        if let Some(local) = self.backend.local_snapshot() {
            for details in local.areas() {
                tally.note_loaded(self, details, Duration::ZERO, false);
            }
        }
    }

    fn shown_view(&self, area_id: AreaId) -> ShownView {
        self.atlas_cache
            .load()
            .get_area(&area_id)
            .map(|area| area.meta().projection_token.clone())
    }

    /// Shows fetched maps together, as one publication. A map published anew
    /// while its fetch was out, as a sync's refetch does, keeps that view:
    /// it is at least as new.
    ///
    /// # Errors
    /// The credential changed since the load began, or a recovered delete
    /// couldn't be aborted durably.
    fn show_fetched(
        &self,
        batch: Vec<Arrival>,
        auth_generation: u64,
        tally: &mut Tally,
    ) -> CloudResult<()> {
        if batch.is_empty() {
            return Ok(());
        }
        let _gate = self.mutation_gate.lock();
        if self.backend.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        // One current local generation under mutation coordination: the
        // fetches above may span several local commits.
        let current_local = self.backend.local_snapshot();
        let mut updates = Vec::with_capacity(batch.len());
        let mut arrived = Vec::new();
        for Arrival {
            details,
            took,
            shown_before,
        } in batch
        {
            let area_id = details.area.id;
            if self.local_projection.lock().known.contains(&area_id) {
                continue;
            }
            // Either signal flagging non-ownership means shared.
            let shared = details.area.owner_nickname.is_some()
                || details.area.access.is_some_and(|access| !access.is_owner);
            let shown = self.shown_view(area_id);
            if shown != shown_before && shown != Some(details.area.projection_token.clone()) {
                tally.note_loaded(self, &details, took, shared);
                continue;
            }
            self.pending.abort_recovered_delete(area_id)?;
            // A load buffers several GETs; an ACK may have advanced this
            // area's revision while another document was still loading.
            self.pending.note_confirmed_rev(area_id, details.area.rev);
            let (area, _) = self.project_confirmed(
                &details,
                ReplayMode::StopAtFailure,
                current_local.as_deref(),
            );
            self.pending.recovery_base_loaded(area_id);
            tally.note_loaded(self, &details, took, shared);
            if shown.is_none() {
                arrived.push(area_id);
            }
            updates.push((area_id, area));
        }
        self.publish_areas(&updates, &[]);
        self.note_arrivals(arrived);
        local_projection::adopt_locked(self);
        Ok(())
    }

    /// After a complete cloud list: stops showing the maps shown before it
    /// that neither it nor this device's list (`named`) names. Maps shown
    /// since the list began are spared, as are maps under delete recovery.
    fn drop_unlisted(
        &self,
        shown_before: &HashSet<AreaId>,
        named: &HashSet<AreaId>,
        auth_generation: u64,
    ) -> CloudResult<()> {
        let _gate = self.mutation_gate.lock();
        if self.backend.auth_generation() != auth_generation {
            return Err(CloudError::CredentialChanged);
        }
        let ephemeral = self.backend.ephemeral_area_ids();
        let recovering: HashSet<AreaId> = self.pending.recovery_area_ids().into_iter().collect();
        let known = self.local_projection.lock().known.clone();
        let gone: Vec<AreaId> = shown_before
            .iter()
            .filter(|id| {
                !named.contains(id)
                    && !known.contains(id)
                    && !ephemeral.contains(id)
                    && !recovering.contains(id)
            })
            .copied()
            .collect();
        if !gone.is_empty() {
            self.publish_areas(&[], &gone);
        }
        Ok(())
    }

    /// Resolves interrupted deletes the lists didn't name: a map still
    /// there aborts its delete, one authoritatively gone completes it.
    async fn recover_deletes(
        &self,
        named: &HashSet<AreaId>,
        auth_generation: u64,
    ) -> CloudResult<Vec<Arrival>> {
        let mut present = Vec::new();
        for area_id in self.pending.recovery_area_ids() {
            if self.is_local_projection(area_id)
                || named.contains(&area_id)
                || !self.pending.has_delete_intent(area_id)
            {
                continue;
            }
            let shown_before = self.shown_view(area_id);
            let started = Instant::now();
            let fetched = if self.pending.delete_intent_is_cloud(area_id) {
                self.backend
                    .get_area_at_generation(&area_id, auth_generation)
                    .await
            } else {
                self.backend_for_area(area_id).get_area(&area_id).await
            };
            match fetched {
                Ok(details) => present.push(Arrival {
                    details,
                    took: started.elapsed(),
                    shown_before,
                }),
                Err(
                    CloudError::NotFoundOrNoAccess
                    | CloudError::PermissionDenied(_)
                    | CloudError::AreaNotFound(_),
                ) => self.recover_unavailable_area(area_id)?,
                Err(CloudError::CredentialChanged) => return Err(CloudError::CredentialChanged),
                Err(error) => {
                    warn!("Could not yet reconcile interrupted delete for area {area_id}: {error}");
                }
            }
        }
        Ok(present)
    }

    /// The names of local maps made since `since` that a cloud map also bears.
    fn made_during_outage(
        &self,
        since: DateTime<Utc>,
        cloud_names: &HashSet<String>,
    ) -> Vec<String> {
        let Some(local) = self.backend.local_snapshot() else {
            return Vec::new();
        };
        let mut names: Vec<String> = local
            .areas()
            .filter(|details| {
                details.area.created_at >= since && cloud_names.contains(&details.area.name)
            })
            .map(|details| details.area.name.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

/// Fetches one cloud map through its Library's lane.
async fn fetch(
    weak: Weak<Inner>,
    fetcher: Arc<super::fetcher::Fetcher>,
    backend: Backend,
    area_id: AreaId,
    auth_generation: u64,
) -> Fetched {
    let Some(shown_before) = weak.upgrade().map(|inner| inner.shown_view(area_id)) else {
        // The mapper is gone, and the load with it.
        return (area_id, Err(CloudError::CredentialChanged));
    };
    let started = Instant::now();
    let result = fetcher.get(&*backend, area_id, auth_generation).await;
    (
        area_id,
        result.map(|details| Arrival {
            details,
            took: started.elapsed(),
            shown_before,
        }),
    )
}

/// Waits for `at`, or for a retry request.
async fn wait_until(retry: &Notify, at: Option<Instant>) {
    match at {
        Some(at) => {
            tokio::select! {
                () = retry.notified() => {}
                () = tokio::time::sleep_until(at) => {}
            }
        }
        None => retry.notified().await,
    }
}

/// Ends the load `tally` counts with `err`.
fn end(weak: &Weak<Inner>, tally: &mut Tally, err: CloudError) {
    tally.fatal = Some(err);
    tally.first_pass = true;
    tally.settled = true;
    if let Some(inner) = weak.upgrade() {
        inner.report(tally);
    }
}

/// One load, from the maps on this device to the last cloud map.
#[allow(clippy::too_many_lines)]
async fn run(weak: Weak<Inner>, load: u64) {
    let mut tally = Tally::new(load);
    let Some((backend, fetcher, auth_generation, window, retry_now)) =
        weak.upgrade().map(|inner| {
            (
                inner.backend.clone(),
                inner.fetcher.clone(),
                inner.backend.auth_generation(),
                *inner.loads.retry_window.lock(),
                inner.loads.retry_now.clone(),
            )
        })
    else {
        return;
    };

    // The maps on this device, at once: the local ones through the local
    // projection, the rest (the session's, or a backend's that doesn't
    // sync) read here. A device that can't list its maps ends the load.
    let device = match backend.list_device_areas().await {
        Ok(device) => device,
        Err(err) => return end(&weak, &mut tally, err),
    };
    let device_ids: HashSet<AreaId> = device.iter().map(|area| area.id).collect();
    {
        let Some(inner) = weak.upgrade() else { return };
        inner.show_device_maps(&mut tally);
        local_projection::adopt_and_wait(&inner).await;
        let known = inner.local_projection.lock().known.clone();
        let mut device_maps = Vec::with_capacity(device.len());
        for area in device.iter().filter(|area| !known.contains(&area.id)) {
            let shown_before = inner.shown_view(area.id);
            let started = Instant::now();
            match backend.get_area(&area.id).await {
                Ok(details) => device_maps.push(Arrival {
                    details,
                    took: started.elapsed(),
                    shown_before,
                }),
                Err(err) => warn!("Failed to load area {}: {err}", area.id),
            }
        }
        if let Err(err) = inner.show_fetched(device_maps, auth_generation, &mut tally) {
            drop(inner);
            return end(&weak, &mut tally, err);
        }
        inner.report(&tally);
    }
    let mut in_flight: JoinSet<Fetched> = JoinSet::new();
    let mut first_try: HashSet<AreaId> = HashSet::new();

    // The cloud list, retried until the service answers.
    let mut list_failures: Option<Failures> = None;
    let mut unavailable_since: Option<DateTime<Utc>> = None;
    let mut recovered = false;
    let (listed, shown_before) = loop {
        let Some(shown_before) = weak.upgrade().map(|inner| {
            inner
                .atlas_cache
                .load()
                .areas()
                .map(|area| *area.get_id())
                .collect::<HashSet<_>>()
        }) else {
            return;
        };
        let started = Instant::now();
        match backend.list_cloud_areas().await {
            Ok(listed) => {
                tally.list_duration = started.elapsed();
                break (listed, shown_before);
            }
            Err(CloudError::CredentialChanged) => {
                return end(&weak, &mut tally, CloudError::CredentialChanged);
            }
            Err(err) => {
                warn!("Cloud maps could not be listed: {err}");
                unavailable_since.get_or_insert_with(Utc::now);
                if !tally.cloud_counted
                    && let Some(remembered) = backend.remembered_inventory().await
                {
                    tally.expected.extend(remembered.iter().map(|row| row.id));
                    tally.cloud_counted = true;
                    if let Some(inner) = weak.upgrade() {
                        inner.fetcher.note_libraries(
                            remembered.iter().map(|row| (row.id, Library::of_area(row))),
                        );
                        inner.loads.set_rows(remembered, true);
                    }
                }
                tally.cloud_unavailable = Some(err.clone());
                if !recovered {
                    recovered = true;
                    let Some(inner) = weak.upgrade() else { return };
                    let named: HashSet<AreaId> = device_ids.iter().copied().collect();
                    match inner.recover_deletes(&named, auth_generation).await {
                        Ok(present) => {
                            if let Err(err) =
                                inner.show_fetched(present, auth_generation, &mut tally)
                            {
                                drop(inner);
                                return end(&weak, &mut tally, err);
                            }
                        }
                        Err(err) => {
                            drop(inner);
                            return end(&weak, &mut tally, err);
                        }
                    }
                }
                tally.first_pass = true;
                let now = Instant::now();
                let failures = Failures::after(list_failures, now);
                list_failures = Some(failures);
                let next = retry::next_retry(&err, failures, now, window);
                if next.is_none() {
                    tally.stopped = 1;
                    tally.settled = true;
                }
                if let Some(inner) = weak.upgrade() {
                    if next.is_none() {
                        inner.loads.stop_all(Some(&err));
                    }
                    inner.report(&tally);
                }
                wait_until(&retry_now, next).await;
                if next.is_none() {
                    list_failures = None;
                    if let Some(inner) = weak.upgrade() {
                        inner.loads.stop_all(None);
                    }
                }
                tally.stopped = 0;
                tally.settled = false;
            }
        }
    };

    // The cloud list answered.
    let listed_ids: HashSet<AreaId> = listed.iter().map(|area| area.id).collect();
    let cloud_names: HashSet<String> = listed.iter().map(|area| area.name.clone()).collect();
    {
        let Some(inner) = weak.upgrade() else { return };
        inner
            .fetcher
            .note_libraries(listed.iter().map(|area| (area.id, Library::of_area(area))));
        let known_now = inner.local_projection.lock().known.clone();
        inner.loads.set_rows(
            listed
                .iter()
                .filter(|area| !known_now.contains(&area.id) && !device_ids.contains(&area.id))
                .cloned(),
            false,
        );
        if let Some(since) = unavailable_since {
            tally.duplicates = inner.made_during_outage(since, &cloud_names);
        }
        let mut named = listed_ids.clone();
        named.extend(device_ids.iter().copied());
        if let Err(err) = inner.drop_unlisted(&shown_before, &named, auth_generation) {
            drop(inner);
            return end(&weak, &mut tally, err);
        }
        let known = inner.local_projection.lock().known.clone();
        inner.count_local_maps(&mut tally);
        tally
            .expected
            .retain(|id| device_ids.contains(id) || tally.loaded.contains(id));
        tally
            .expected
            .extend(listed_ids.iter().filter(|id| !known.contains(id)).copied());
        tally.cloud_counted = true;
        tally.cloud_unavailable = None;
        if !recovered {
            match inner.recover_deletes(&named, auth_generation).await {
                Ok(present) => {
                    if let Err(err) = inner.show_fetched(present, auth_generation, &mut tally) {
                        drop(inner);
                        return end(&weak, &mut tally, err);
                    }
                }
                Err(err) => {
                    drop(inner);
                    return end(&weak, &mut tally, err);
                }
            }
        }
        for area in &listed {
            if known.contains(&area.id) || device_ids.contains(&area.id) {
                continue;
            }
            first_try.insert(area.id);
            in_flight.spawn(fetch(
                weak.clone(),
                fetcher.clone(),
                backend.clone(),
                area.id,
                auth_generation,
            ));
        }
        inner.report(&tally);
    }

    // Every listed map, retried on the service's terms.
    let mut failures: HashMap<AreaId, Failures> = HashMap::new();
    let mut waiting: HashMap<AreaId, Instant> = HashMap::new();
    let mut stopped: HashSet<AreaId> = HashSet::new();
    let mut batch: Vec<Arrival> = Vec::new();
    let mut publish_at: Option<Instant> = None;
    // Whether every listed map has had its first try. The load's first pass
    // may have come earlier, with a cloud list that failed.
    let mut listed_tried = false;
    loop {
        let first_pass_done = !listed_tried && first_try.is_empty();
        let idle = in_flight.is_empty() && waiting.is_empty();
        if first_pass_done || idle || publish_at.is_some_and(|at| at <= Instant::now()) {
            let Some(inner) = weak.upgrade() else { return };
            if let Err(err) =
                inner.show_fetched(std::mem::take(&mut batch), auth_generation, &mut tally)
            {
                drop(inner);
                return end(&weak, &mut tally, err);
            }
            publish_at = None;
            if first_pass_done {
                listed_tried = true;
                tally.first_pass = true;
                local_projection::adopt_and_wait(&inner).await;
                inner.report_abandoned_relocations();
                // The load can race the sync engine; its next tick prunes
                // anything the fresh row set no longer covers.
                inner.request_sync();
            }
            if idle {
                tally.stopped = stopped.len();
                tally.settled = true;
                if stopped.is_empty() {
                    tally.failure = None;
                }
            }
            inner.report(&tally);
            drop(inner);
            if idle {
                if stopped.is_empty() {
                    return;
                }
                wait_until(&retry_now, None).await;
                for area_id in stopped.drain() {
                    failures.remove(&area_id);
                    if let Some(inner) = weak.upgrade() {
                        inner.loads.clear_stopped(area_id);
                    }
                    in_flight.spawn(fetch(
                        weak.clone(),
                        fetcher.clone(),
                        backend.clone(),
                        area_id,
                        auth_generation,
                    ));
                }
                tally.stopped = 0;
                tally.settled = false;
                continue;
            }
        }

        let next_due = waiting.values().min().copied();
        tokio::select! {
            Some(joined) = in_flight.join_next() => {
                let Ok((area_id, result)) = joined else { continue };
                first_try.remove(&area_id);
                match result {
                    Ok(arrival) => {
                        failures.remove(&area_id);
                        batch.push(arrival);
                        publish_at.get_or_insert_with(|| Instant::now() + PUBLISH_EVERY);
                    }
                    Err(CloudError::CredentialChanged) => {
                        return end(&weak, &mut tally, CloudError::CredentialChanged);
                    }
                    Err(
                        CloudError::NotFoundOrNoAccess
                        | CloudError::PermissionDenied(_)
                        | CloudError::AreaNotFound(_),
                    ) => {
                        // Gone since the list, or no longer readable.
                        tally.expected.remove(&area_id);
                        if let Some(inner) = weak.upgrade() {
                            inner.loads.forget(area_id);
                        }
                    }
                    Err(err) => {
                        warn!("Map {area_id} could not be loaded: {err}");
                        let now = Instant::now();
                        let run = Failures::after(failures.get(&area_id).copied(), now);
                        failures.insert(area_id, run);
                        if let Some(at) = retry::next_retry(&err, run, now, window) {
                            waiting.insert(area_id, at);
                        } else {
                            if let Some(inner) = weak.upgrade() {
                                inner.loads.note_stopped(area_id, err.clone());
                            }
                            stopped.insert(area_id);
                        }
                        tally.failure = Some(err);
                    }
                }
            }
            () = async {
                match publish_at {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {}
            () = async {
                match next_due {
                    Some(at) => tokio::time::sleep_until(at).await,
                    None => std::future::pending().await,
                }
            } => {
                let now = Instant::now();
                let due: Vec<AreaId> = waiting
                    .iter()
                    .filter(|(_, at)| **at <= now)
                    .map(|(id, _)| *id)
                    .collect();
                for area_id in due {
                    waiting.remove(&area_id);
                    in_flight.spawn(fetch(weak.clone(), fetcher.clone(), backend.clone(), area_id, auth_generation));
                }
            }
            () = retry_now.notified() => {
                // Everything still to load tries now, with a fresh run of
                // failures for what had stopped.
                for area_id in stopped.drain() {
                    failures.remove(&area_id);
                    if let Some(inner) = weak.upgrade() {
                        inner.loads.clear_stopped(area_id);
                    }
                    in_flight.spawn(fetch(weak.clone(), fetcher.clone(), backend.clone(), area_id, auth_generation));
                }
                for (area_id, _) in waiting.drain() {
                    in_flight.spawn(fetch(weak.clone(), fetcher.clone(), backend.clone(), area_id, auth_generation));
                }
            }
        }
    }
}
