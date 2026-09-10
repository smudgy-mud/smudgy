use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use super::SessionId;
use super::runtime::{
    Runtime, RuntimeStartPermit, RuntimeThreadJoinOutcome, RuntimeThreadPublicationError,
    RuntimeThreadPublicationFailure,
};

/// Cached, data-only identity for a script-visible session handle. It can be
/// carried with a destruction notice after the runtime leaves the registry.
#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub id: SessionId,
    pub profile_name: Arc<String>,
    pub profile_subtext: Arc<String>,
    pub connected: bool,
}

impl SessionSnapshot {
    #[must_use]
    pub fn to_json(&self, tombstone: bool) -> String {
        serde_json::json!({
            "id": u32::from(self.id),
            "profile": {
                "name": self.profile_name.as_str(),
                "subtext": self.profile_subtext.as_str(),
            },
            "connected": self.connected && !tombstone,
            "tombstone": tombstone,
        })
        .to_string()
    }
}

/// Per-session v8 inspector endpoints (set once a session's runtime is built, when
/// debugging is enabled). Kept separate from the `Runtime` entry because the bound
/// address isn't known until after the runtime thread constructs its script engine.
static INSPECTOR_ADDRESSES: OnceLock<Mutex<HashMap<SessionId, SocketAddr>>> = OnceLock::new();

fn lock_recover<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn inspector_addresses() -> &'static Mutex<HashMap<SessionId, SocketAddr>> {
    INSPECTOR_ADDRESSES.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Record the v8 inspector endpoint for a session.
///
pub fn set_inspector_address(session_id: SessionId, addr: SocketAddr) {
    lock_recover(inspector_addresses()).insert(session_id, addr);
}

/// Get the v8 inspector endpoint for a session, if one is listening (debug mode).
/// The UI's "Show Inspector" affordance spawns `smudgy_inspector <addr>` with this.
///
#[must_use]
pub fn get_inspector_address(session_id: SessionId) -> Option<SocketAddr> {
    lock_recover(inspector_addresses())
        .get(&session_id)
        .copied()
}

/// Shared map of active session runtimes, keyed by session id.
type SessionRegistry = Arc<Mutex<HashMap<SessionId, Arc<Runtime>>>>;

/// Global registry of all active sessions
static SESSION_REGISTRY: OnceLock<SessionRegistry> = OnceLock::new();
static BROADCAST_CHANNELS: OnceLock<
    Mutex<HashMap<String, smudgy_script::InMemoryBroadcastChannel>>,
> = OnceLock::new();

#[cfg(test)]
std::thread_local! {
    static TEST_PUBLICATION_UNWIND: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
pub(super) fn inject_next_publication_unwind() {
    TEST_PUBLICATION_UNWIND.set(true);
}

#[cfg(test)]
fn maybe_inject_publication_unwind() {
    TEST_PUBLICATION_UNWIND.with(|armed| {
        if armed.replace(false) {
            panic!("injected production registration unwind");
        }
    });
}

trait StartGate {
    type Permit;

    /// Move the worker to a second barrier that still forbids scripts.
    fn prepare_start(&self) -> Option<Self::Permit>;

    /// Admit script construction only after `created` is queued.
    fn commit_start(permit: Self::Permit) -> bool;

    /// Absorbingly close the first barrier.
    fn cancel_start(&self);

    /// Consume the exact pre-start worker join after both barriers close.
    fn join_cancelled(&self, session_id: SessionId) -> RuntimeThreadJoinOutcome;
}

impl StartGate for Runtime {
    type Permit = RuntimeStartPermit;

    fn prepare_start(&self) -> Option<Self::Permit> {
        Runtime::prepare_start(self)
    }

    fn commit_start(permit: Self::Permit) -> bool {
        Runtime::commit_start(permit)
    }

    fn cancel_start(&self) {
        Runtime::cancel_start(self);
    }

    fn join_cancelled(&self, session_id: SessionId) -> RuntimeThreadJoinOutcome {
        super::runtime::join_runtime_thread(session_id)
    }
}

trait PublicationEffect {
    /// Queue `created` to staged targets. `false` reports a contained unwind.
    fn publish_created(&mut self) -> bool;

    /// Compensate exactly the targets for which a `created` send was attempted.
    fn publish_rollback(&mut self) -> bool;
}

/// Armed rollback for the interval between map insertion and the second-phase
/// start commit. The exact `Arc` comparison prevents a stale unwind from
/// removing a later runtime should an invariant be violated elsewhere.
struct SessionPublication<'a, T: StartGate, P: PublicationEffect> {
    sessions: Arc<Mutex<HashMap<SessionId, Arc<T>>>>,
    inspectors: &'a Mutex<HashMap<SessionId, SocketAddr>>,
    session_id: SessionId,
    runtime: Arc<T>,
    inserted: bool,
    permit: Option<T::Permit>,
    effect: Option<P>,
    armed: bool,
}

impl<'a, T: StartGate, P: PublicationEffect> SessionPublication<'a, T, P> {
    fn new(
        sessions: Arc<Mutex<HashMap<SessionId, Arc<T>>>>,
        inspectors: &'a Mutex<HashMap<SessionId, SocketAddr>>,
        session_id: SessionId,
        runtime: Arc<T>,
    ) -> Self {
        Self {
            sessions,
            inspectors,
            session_id,
            runtime,
            inserted: false,
            permit: None,
            effect: None,
            armed: true,
        }
    }

    fn insert(&mut self) -> bool {
        let mut sessions = lock_recover(&self.sessions);
        if sessions.contains_key(&self.session_id) {
            return false;
        }
        sessions.insert(self.session_id, Arc::clone(&self.runtime));
        self.inserted = true;
        true
    }

    fn prepare_start(&mut self) -> bool {
        self.permit = self.runtime.prepare_start();
        self.permit.is_some()
    }

    fn publish_created(&mut self) -> bool {
        self.effect
            .as_mut()
            .is_some_and(PublicationEffect::publish_created)
    }

    fn commit_start(&mut self) -> bool {
        self.permit.take().is_some_and(T::commit_start)
    }

    fn disarm(&mut self) {
        self.armed = false;
    }

    fn rollback(&mut self) -> RuntimeThreadJoinOutcome {
        if !self.armed {
            return RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined {
                session_id: self.session_id,
            };
        }
        self.armed = false;

        let removed_exact = if self.inserted {
            let mut sessions = lock_recover(&self.sessions);
            let exact = sessions
                .get(&self.session_id)
                .is_some_and(|current| Arc::ptr_eq(current, &self.runtime));
            if exact {
                sessions.remove(&self.session_id);
                lock_recover(self.inspectors).remove(&self.session_id);
            }
            exact
        } else {
            false
        };

        if !self.inserted {
            return RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined {
                session_id: self.session_id,
            };
        }

        // Revoke both phases before compensation and exact join. Dropping a
        // prepared permit makes the worker return from its second recv;
        // cancelling the outer sender handles failures before preparation.
        self.runtime.cancel_start();
        drop(self.permit.take());
        let mut effect = self.effect.take();
        if removed_exact && let Some(effect) = effect.as_mut() {
            let rollback = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                effect.publish_rollback()
            }));
            if let Err(payload) = rollback {
                std::mem::forget(payload);
            }
        }
        let cleanup = if removed_exact {
            self.runtime.join_cancelled(self.session_id)
        } else {
            RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined {
                session_id: self.session_id,
            }
        };
        // An effect destructor is arbitrary generic code. It must not prevent
        // the exact worker join or replace its cleanup proof with another
        // unwind while the publication is rolling back.
        if let Some(effect) = effect {
            let dropped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                drop(effect);
            }));
            if let Err(payload) = dropped {
                std::mem::forget(payload);
            }
        }
        cleanup
    }
}

impl<T: StartGate, P: PublicationEffect> Drop for SessionPublication<'_, T, P> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.rollback();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SessionPublicationError<E> {
    DuplicateSession,
    Effect(E),
    Unwound,
    StartGateClosed,
    CreatedBroadcastUnwound,
    CommitGateClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionPublicationFailure<E> {
    primary: SessionPublicationError<E>,
    cleanup: RuntimeThreadJoinOutcome,
}

/// Publish one staged runtime through independently testable post-insert
/// effects. Before the second gate commits, returned errors and unwinds
/// synchronously remove the exact shell, revoke both gates, compensate any
/// attempted lifecycle publication, and join. Once committed, destruction of
/// staged state happens outside the transactional catch boundary.
fn publish_session<T, S, P, E>(
    sessions: Arc<Mutex<HashMap<SessionId, Arc<T>>>>,
    inspectors: &Mutex<HashMap<SessionId, SocketAddr>>,
    session_id: SessionId,
    runtime: Arc<T>,
    after_insert: impl FnOnce(&Arc<T>) -> Result<(), E>,
    take_snapshot: impl FnOnce(&Arc<T>) -> Result<S, E>,
    prepare_effect: impl FnOnce(&Arc<T>, &S) -> Result<P, E>,
) -> Result<(), SessionPublicationFailure<E>>
where
    T: StartGate,
    P: PublicationEffect,
{
    let mut publication =
        SessionPublication::<T, P>::new(sessions, inspectors, session_id, runtime);
    let attempt = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if !publication.insert() {
            return Err(SessionPublicationError::DuplicateSession);
        }
        after_insert(&publication.runtime).map_err(SessionPublicationError::Effect)?;
        let snapshot =
            take_snapshot(&publication.runtime).map_err(SessionPublicationError::Effect)?;
        publication.effect = Some(
            prepare_effect(&publication.runtime, &snapshot)
                .map_err(SessionPublicationError::Effect)?,
        );
        if !publication.prepare_start() {
            return Err(SessionPublicationError::StartGateClosed);
        }
        if !publication.publish_created() {
            return Err(SessionPublicationError::CreatedBroadcastUnwound);
        }
        if !publication.commit_start() {
            return Err(SessionPublicationError::CommitGateClosed);
        }
        publication.disarm();
        Ok(())
    }));
    let primary = match attempt {
        Ok(Ok(())) => return Ok(()),
        Ok(Err(primary)) => primary,
        Err(payload) => {
            // Panic payload destructors are arbitrary. The typed primary error
            // plus exact join proof survives without a second unwind.
            std::mem::forget(payload);
            SessionPublicationError::Unwound
        }
    };
    let cleanup = publication.rollback();
    Err(SessionPublicationFailure { primary, cleanup })
}

/// Get the global session registry
pub fn get_registry() -> SessionRegistry {
    SESSION_REGISTRY
        .get_or_init(|| Arc::new(Mutex::new(HashMap::new())))
        .clone()
}

/// Register a new session in the global registry.
///
/// Publication is transactional: an error or unwind after insertion but
/// before the second start gate commits removes the exact shell, compensates
/// any attempted `created` occurrence, and joins before any script can run.
///
/// # Panics
///
/// Panics after rollback if any pre-commit publication step fails. A panic
/// after commit is an ordinary live-runtime failure for the caller to unwind.
pub fn register_session(session_id: SessionId, runtime: Arc<Runtime>) {
    try_register_session(session_id, runtime).unwrap_or_else(|error| panic!("{error}"));
}

pub(crate) fn try_register_session(
    session_id: SessionId,
    runtime: Arc<Runtime>,
) -> Result<(), RuntimeThreadPublicationError> {
    let server = Arc::clone(&runtime.server_name);
    let registry = get_registry();
    let result: Result<(), SessionPublicationFailure<Infallible>> = publish_session(
        registry,
        inspector_addresses(),
        session_id,
        runtime,
        |_| {
            #[cfg(test)]
            maybe_inject_publication_unwind();
            Ok(())
        },
        |runtime| {
            Ok(SessionSnapshot {
                id: session_id,
                profile_name: Arc::clone(&runtime.profile_name),
                profile_subtext: Arc::clone(&runtime.profile_subtext),
                connected: runtime.connected.load(std::sync::atomic::Ordering::Acquire),
            })
        },
        |_, snapshot| Ok(prepare_lifecycle_publication(&server, snapshot)),
    );
    match result {
        Ok(()) => {
            log::info!("Registered session {session_id} in global registry");
            Ok(())
        }
        Err(SessionPublicationFailure { primary, cleanup }) => {
            let failure = match primary {
                SessionPublicationError::DuplicateSession => {
                    RuntimeThreadPublicationFailure::DuplicateSession
                }
                SessionPublicationError::Effect(never) => match never {},
                SessionPublicationError::Unwound => {
                    RuntimeThreadPublicationFailure::PublicationUnwound
                }
                SessionPublicationError::StartGateClosed => {
                    RuntimeThreadPublicationFailure::StartGateClosed
                }
                SessionPublicationError::CreatedBroadcastUnwound => {
                    RuntimeThreadPublicationFailure::CreatedBroadcastUnwound
                }
                SessionPublicationError::CommitGateClosed => {
                    RuntimeThreadPublicationFailure::CommitGateClosed
                }
            };
            Err(RuntimeThreadPublicationError::new(
                session_id, failure, cleanup,
            ))
        }
    }
}

/// Return the shared standard `BroadcastChannel` backend for one configured server entry.
///
#[must_use]
pub fn broadcast_channel_for_server(server: &str) -> smudgy_script::InMemoryBroadcastChannel {
    lock_recover(BROADCAST_CHANNELS.get_or_init(|| Mutex::new(HashMap::new())))
        .entry(server.to_string())
        .or_default()
        .clone()
}

/// Unregister a session from the global registry
///
pub fn unregister_session(session_id: SessionId) {
    let Some(mut snapshot) = snapshot(session_id) else {
        log::warn!("Attempted to unregister non-existent session {session_id}");
        return;
    };
    let Some(runtime) = get_runtime(session_id) else {
        return;
    };
    let server = Arc::clone(&runtime.server_name);
    if runtime
        .connected
        .swap(false, std::sync::atomic::Ordering::AcqRel)
    {
        snapshot.connected = false;
        broadcast_lifecycle(&server, "disconnected", &snapshot, false);
    }
    snapshot.connected = false;
    broadcast_lifecycle(&server, "destroyed", &snapshot, true);
    for other in get_runtimes_for_server(&server) {
        if other.session_id != session_id {
            for key in super::runtime::input::purge_session_input_interop(
                &other.input_word_sets,
                &other.pane_input_callbacks,
                session_id,
            ) {
                let _ = other
                    .tx
                    .send(super::runtime::RuntimeAction::InputWordSetsChanged { key });
            }
        }
    }
    let registry = get_registry();
    let mut sessions = lock_recover(&registry);
    lock_recover(inspector_addresses()).remove(&session_id);
    if sessions.remove(&session_id).is_some() {
        log::info!("Unregistered session {session_id} from global registry");
    } else {
        log::warn!("Attempted to unregister non-existent session {session_id}");
    }
}

/// Broadcast one non-replaying lifecycle occurrence to every currently
/// registered session on the same server entry.
pub fn broadcast_lifecycle(server: &str, kind: &str, snapshot: &SessionSnapshot, tombstone: bool) {
    let broadcast =
        prepare_lifecycle_broadcast(get_runtimes_for_server(server), kind, snapshot, tombstone);
    if !broadcast.publish_all() {
        log::error!("session lifecycle {kind} broadcast unwound and was contained");
    }
}

struct LifecycleBroadcast {
    targets: Vec<Arc<Runtime>>,
    canonical: Arc<str>,
    payload: Arc<str>,
    source: SessionSnapshot,
}

impl LifecycleBroadcast {
    fn publish_one(&self, runtime: &Runtime) {
        let _ = runtime
            .tx
            .send(super::runtime::RuntimeAction::InteropEvent(Arc::new(
                super::runtime::InteropEventBody {
                    canonical: Arc::clone(&self.canonical),
                    stamped: Arc::clone(&self.canonical),
                    payload: Arc::clone(&self.payload),
                    source: self.source.clone(),
                    depth: 0,
                },
            )));
    }

    fn publish_all(&self) -> bool {
        for runtime in &self.targets {
            let published = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.publish_one(runtime);
            }));
            if let Err(payload) = published {
                std::mem::forget(payload);
                return false;
            }
        }
        true
    }
}

fn prepare_lifecycle_broadcast(
    targets: Vec<Arc<Runtime>>,
    kind: &str,
    snapshot: &SessionSnapshot,
    tombstone: bool,
) -> LifecycleBroadcast {
    let canonical: Arc<str> = Arc::from(format!("sessions:{kind}"));
    let payload: Arc<str> = Arc::from(snapshot.to_json(tombstone));
    LifecycleBroadcast {
        targets,
        canonical,
        payload,
        source: snapshot.clone(),
    }
}

struct LifecyclePublication {
    created: LifecycleBroadcast,
    destroyed: LifecycleBroadcast,
    attempted: usize,
}

impl PublicationEffect for LifecyclePublication {
    fn publish_created(&mut self) -> bool {
        for (index, runtime) in self.created.targets.iter().enumerate() {
            // Count before entering the send so rollback errs on the side of a
            // tombstone if the send unwinds after enqueueing but before return.
            self.attempted = index + 1;
            let published = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.created.publish_one(runtime);
            }));
            if let Err(payload) = published {
                std::mem::forget(payload);
                return false;
            }
        }
        true
    }

    fn publish_rollback(&mut self) -> bool {
        for runtime in self.destroyed.targets.iter().take(self.attempted) {
            let published = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.destroyed.publish_one(runtime);
            }));
            if let Err(payload) = published {
                std::mem::forget(payload);
                return false;
            }
        }
        true
    }
}

fn prepare_lifecycle_publication(server: &str, snapshot: &SessionSnapshot) -> LifecyclePublication {
    let targets = get_runtimes_for_server(server);
    LifecyclePublication {
        created: prepare_lifecycle_broadcast(targets.clone(), "created", snapshot, false),
        destroyed: prepare_lifecycle_broadcast(targets, "destroyed", snapshot, true),
        attempted: 0,
    }
}

/// Active sessions on one configured server entry.
///
#[must_use]
pub fn get_session_ids_for_server(server: &str) -> Vec<SessionId> {
    let registry = get_registry();
    let sessions = lock_recover(&registry);
    let mut ids: Vec<SessionId> = sessions
        .iter()
        .filter_map(|(id, runtime)| (runtime.server_name.as_str() == server).then_some(*id))
        .collect();
    ids.sort_unstable();
    ids
}

/// Runtime handles on one configured server entry, cloned out before callers
/// route actions so the global registry lock is never held across a send.
///
#[must_use]
pub fn get_runtimes_for_server(server: &str) -> Vec<Arc<Runtime>> {
    let registry = get_registry();
    let sessions = lock_recover(&registry);
    sessions
        .values()
        .filter(|runtime| runtime.server_name.as_str() == server)
        .cloned()
        .collect()
}

#[must_use]
pub fn snapshot(session_id: SessionId) -> Option<SessionSnapshot> {
    let runtime = get_runtime(session_id)?;
    Some(SessionSnapshot {
        id: session_id,
        profile_name: Arc::clone(&runtime.profile_name),
        profile_subtext: Arc::clone(&runtime.profile_subtext),
        connected: runtime.connected.load(std::sync::atomic::Ordering::Acquire),
    })
}

/// Get a specific runtime by session ID
///
#[must_use]
pub fn get_runtime(session_id: SessionId) -> Option<Arc<Runtime>> {
    let registry = get_registry();
    let sessions = lock_recover(&registry);
    sessions.get(&session_id).cloned()
}

/// Get the number of active sessions
///
#[must_use]
pub fn session_count() -> usize {
    let registry = get_registry();
    let sessions = lock_recover(&registry);
    sessions.len()
}

#[cfg(test)]
mod tests {
    use std::panic::AssertUnwindSafe;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle};
    use std::time::Duration;

    use super::*;

    const DEADLINE: Duration = Duration::from_secs(2);

    #[derive(Default)]
    struct TestJoins {
        handles: Mutex<HashMap<SessionId, JoinHandle<()>>>,
    }

    impl TestJoins {
        fn insert(&self, session_id: SessionId, handle: JoinHandle<()>) {
            assert!(
                lock_recover(&self.handles)
                    .insert(session_id, handle)
                    .is_none()
            );
        }

        fn join_exact(&self, session_id: SessionId) -> RuntimeThreadJoinOutcome {
            let handle = lock_recover(&self.handles).remove(&session_id);
            let Some(handle) = handle else {
                return RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined { session_id };
            };
            if handle.join().is_ok() {
                RuntimeThreadJoinOutcome::Clean { session_id }
            } else {
                RuntimeThreadJoinOutcome::Panicked { session_id }
            }
        }

        fn join_all(&self) -> Vec<RuntimeThreadJoinOutcome> {
            let handles = std::mem::take(&mut *lock_recover(&self.handles));
            let mut joined = handles
                .into_iter()
                .map(|(session_id, handle)| {
                    if handle.join().is_ok() {
                        RuntimeThreadJoinOutcome::Clean { session_id }
                    } else {
                        RuntimeThreadJoinOutcome::Panicked { session_id }
                    }
                })
                .collect::<Vec<_>>();
            joined.sort_by_key(|outcome| outcome.session_id());
            joined
        }

        fn contains(&self, session_id: SessionId) -> bool {
            lock_recover(&self.handles).contains_key(&session_id)
        }
    }

    struct TestPermit {
        commit: mpsc::Sender<()>,
        fail: bool,
    }

    struct TestRuntime {
        start: Mutex<Option<mpsc::Sender<mpsc::Receiver<()>>>>,
        joins: Arc<TestJoins>,
        evaluations: Arc<AtomicUsize>,
        fail_commit: AtomicBool,
    }

    impl StartGate for TestRuntime {
        type Permit = TestPermit;

        fn prepare_start(&self) -> Option<Self::Permit> {
            let start = lock_recover(&self.start).take()?;
            let (commit, committed) = mpsc::channel();
            start.send(committed).ok()?;
            Some(TestPermit {
                commit,
                fail: self.fail_commit.load(Ordering::Acquire),
            })
        }

        fn commit_start(permit: Self::Permit) -> bool {
            !permit.fail && permit.commit.send(()).is_ok()
        }

        fn cancel_start(&self) {
            drop(lock_recover(&self.start).take());
        }

        fn join_cancelled(&self, session_id: SessionId) -> RuntimeThreadJoinOutcome {
            self.joins.join_exact(session_id)
        }
    }

    struct TestEffect {
        events: Arc<Mutex<Vec<&'static str>>>,
        attempted: bool,
        fail_created: bool,
        panic_created: bool,
        panic_drop: bool,
    }

    impl PublicationEffect for TestEffect {
        fn publish_created(&mut self) -> bool {
            self.attempted = true;
            lock_recover(&self.events).push("created");
            assert!(!self.panic_created, "injected created publish unwind");
            !self.fail_created
        }

        fn publish_rollback(&mut self) -> bool {
            if self.attempted {
                lock_recover(&self.events).push("destroyed");
            }
            true
        }
    }

    impl Drop for TestEffect {
        fn drop(&mut self) {
            assert!(!self.panic_drop, "injected publication effect drop unwind");
        }
    }

    fn staged_runtime(
        joins: Arc<TestJoins>,
        session_id: SessionId,
        events: Arc<Mutex<Vec<&'static str>>>,
        fail_commit: bool,
    ) -> (Arc<TestRuntime>, mpsc::Receiver<()>) {
        let (start, started) = mpsc::channel::<mpsc::Receiver<()>>();
        let (finished, completion) = mpsc::sync_channel(1);
        let evaluations = Arc::new(AtomicUsize::new(0));
        let worker_evaluations = Arc::clone(&evaluations);
        joins.insert(
            session_id,
            thread::spawn(move || {
                if let Ok(committed) = started.recv()
                    && committed.recv().is_ok()
                {
                    worker_evaluations.fetch_add(1, Ordering::AcqRel);
                    lock_recover(&events).push("worker");
                }
                let _ = finished.send(());
            }),
        );
        (
            Arc::new(TestRuntime {
                start: Mutex::new(Some(start)),
                joins,
                evaluations,
                fail_commit: AtomicBool::new(fail_commit),
            }),
            completion,
        )
    }

    fn effect(events: Arc<Mutex<Vec<&'static str>>>) -> TestEffect {
        TestEffect {
            events,
            attempted: false,
            fail_created: false,
            panic_created: false,
            panic_drop: false,
        }
    }

    #[derive(Clone, Copy, PartialEq, Eq)]
    enum Stage {
        AfterInsert,
        Snapshot,
        BroadcastPreparation,
    }

    #[derive(Clone, Copy)]
    enum Fault {
        Error(Stage),
        Panic(Stage),
    }

    fn inject(fault: Fault, stage: Stage) -> Result<(), &'static str> {
        match fault {
            Fault::Error(target) if target == stage => Err("injected publication failure"),
            Fault::Panic(target) if target == stage => panic!("injected publication unwind"),
            Fault::Error(_) | Fault::Panic(_) => Ok(()),
        }
    }

    fn publish_with_fault(
        sessions: Arc<Mutex<HashMap<SessionId, Arc<TestRuntime>>>>,
        inspectors: &Mutex<HashMap<SessionId, SocketAddr>>,
        session_id: SessionId,
        runtime: Arc<TestRuntime>,
        events: Arc<Mutex<Vec<&'static str>>>,
        fault: Fault,
    ) -> Result<(), SessionPublicationFailure<&'static str>> {
        publish_session(
            sessions,
            inspectors,
            session_id,
            runtime,
            |_| inject(fault, Stage::AfterInsert),
            |_| {
                inject(fault, Stage::Snapshot)?;
                Ok(())
            },
            |_, &()| {
                inject(fault, Stage::BroadcastPreparation)?;
                Ok(effect(events))
            },
        )
    }

    fn assert_rolled_back(
        sessions: &Arc<Mutex<HashMap<SessionId, Arc<TestRuntime>>>>,
        inspectors: &Mutex<HashMap<SessionId, SocketAddr>>,
        runtime: &TestRuntime,
        session_id: SessionId,
        completion: mpsc::Receiver<()>,
    ) {
        completion
            .recv_timeout(DEADLINE)
            .expect("cancelled worker finishes");
        assert!(!lock_recover(sessions).contains_key(&session_id));
        assert!(!lock_recover(inspectors).contains_key(&session_id));
        assert_eq!(runtime.evaluations.load(Ordering::Acquire), 0);
        assert!(
            runtime.joins.join_all().is_empty(),
            "exact join was consumed"
        );
    }

    #[test]
    fn returned_failures_and_unwinds_rollback_all_staging_phases() {
        for (offset, stage) in [
            Stage::AfterInsert,
            Stage::Snapshot,
            Stage::BroadcastPreparation,
        ]
        .into_iter()
        .enumerate()
        {
            for (panic, fault) in [(false, Fault::Error(stage)), (true, Fault::Panic(stage))] {
                let sessions = Arc::new(Mutex::new(HashMap::new()));
                let inspectors = Mutex::new(HashMap::new());
                let joins = Arc::new(TestJoins::default());
                let events = Arc::new(Mutex::new(Vec::new()));
                let session_id = SessionId::from(
                    91_000 + u32::try_from(offset * 2 + usize::from(panic)).unwrap(),
                );
                let (runtime, completion) =
                    staged_runtime(joins, session_id, Arc::clone(&events), false);
                let failure = publish_with_fault(
                    Arc::clone(&sessions),
                    &inspectors,
                    session_id,
                    Arc::clone(&runtime),
                    Arc::clone(&events),
                    fault,
                )
                .expect_err("fault rejects publication");
                assert_eq!(
                    failure.primary,
                    if panic {
                        SessionPublicationError::Unwound
                    } else {
                        SessionPublicationError::Effect("injected publication failure")
                    }
                );
                assert_eq!(
                    failure.cleanup,
                    RuntimeThreadJoinOutcome::Clean { session_id }
                );
                assert!(lock_recover(&events).is_empty());
                assert_rolled_back(&sessions, &inspectors, &runtime, session_id, completion);
            }
        }
    }

    fn poison<T>(mutex: &Mutex<T>) {
        let poisoned = std::panic::catch_unwind(AssertUnwindSafe(|| {
            let _guard = mutex.lock().unwrap();
            panic!("inject mutex poison");
        }));
        assert!(poisoned.is_err());
    }

    #[test]
    fn rollback_recovers_poisoned_session_inspector_and_start_gate_locks() {
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let inspectors = Mutex::new(HashMap::new());
        let joins = Arc::new(TestJoins::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let session_id = SessionId::from(91_020);
        let (runtime, completion) = staged_runtime(joins, session_id, Arc::clone(&events), false);
        poison(&sessions);
        poison(&inspectors);
        poison(&runtime.start);
        lock_recover(&inspectors)
            .insert(session_id, "127.0.0.1:9229".parse().expect("test address"));

        let failure = publish_with_fault(
            Arc::clone(&sessions),
            &inspectors,
            session_id,
            Arc::clone(&runtime),
            events,
            Fault::Error(Stage::Snapshot),
        )
        .expect_err("fault rejects publication");
        assert_eq!(
            failure.cleanup,
            RuntimeThreadJoinOutcome::Clean { session_id }
        );
        assert_rolled_back(&sessions, &inspectors, &runtime, session_id, completion);
    }

    fn publish_plain(
        sessions: Arc<Mutex<HashMap<SessionId, Arc<TestRuntime>>>>,
        inspectors: &Mutex<HashMap<SessionId, SocketAddr>>,
        session_id: SessionId,
        runtime: Arc<TestRuntime>,
        events: Arc<Mutex<Vec<&'static str>>>,
    ) -> Result<(), SessionPublicationFailure<&'static str>> {
        publish_session(
            sessions,
            inspectors,
            session_id,
            runtime,
            |_| Ok(()),
            |_| Ok(()),
            |_, &()| Ok(effect(events)),
        )
    }

    #[test]
    fn both_gate_failures_are_finite_joined_and_ordered() {
        for (offset, fail_commit) in [false, true].into_iter().enumerate() {
            let sessions = Arc::new(Mutex::new(HashMap::new()));
            let inspectors = Mutex::new(HashMap::new());
            let joins = Arc::new(TestJoins::default());
            let events = Arc::new(Mutex::new(Vec::new()));
            let session_id = SessionId::from(91_030 + u32::try_from(offset).unwrap());
            let (runtime, completion) =
                staged_runtime(joins, session_id, Arc::clone(&events), fail_commit);
            if !fail_commit {
                drop(lock_recover(&runtime.start).take());
            }

            let failure = publish_plain(
                Arc::clone(&sessions),
                &inspectors,
                session_id,
                Arc::clone(&runtime),
                Arc::clone(&events),
            )
            .expect_err("closed gate rejects publication");
            assert_eq!(
                failure.primary,
                if fail_commit {
                    SessionPublicationError::CommitGateClosed
                } else {
                    SessionPublicationError::StartGateClosed
                }
            );
            assert_eq!(
                failure.cleanup,
                RuntimeThreadJoinOutcome::Clean { session_id }
            );
            assert_eq!(
                &*lock_recover(&events),
                if fail_commit {
                    &["created", "destroyed"][..]
                } else {
                    &[][..]
                }
            );
            assert_rolled_back(&sessions, &inspectors, &runtime, session_id, completion);
        }
    }

    #[test]
    fn created_publish_unwind_is_compensated_before_exact_join() {
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let inspectors = Mutex::new(HashMap::new());
        let joins = Arc::new(TestJoins::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let session_id = SessionId::from(91_040);
        let (runtime, completion) = staged_runtime(joins, session_id, Arc::clone(&events), false);

        let failure = publish_session(
            Arc::clone(&sessions),
            &inspectors,
            session_id,
            Arc::clone(&runtime),
            |_| Ok::<(), &'static str>(()),
            |_| Ok::<(), &'static str>(()),
            |_, &()| {
                Ok(TestEffect {
                    events: Arc::clone(&events),
                    attempted: false,
                    fail_created: false,
                    panic_created: true,
                    panic_drop: false,
                })
            },
        )
        .expect_err("created unwind rejects publication");
        assert_eq!(failure.primary, SessionPublicationError::Unwound);
        assert_eq!(
            failure.cleanup,
            RuntimeThreadJoinOutcome::Clean { session_id }
        );
        assert_eq!(&*lock_recover(&events), &["created", "destroyed"]);
        assert_rolled_back(&sessions, &inspectors, &runtime, session_id, completion);
    }

    #[test]
    fn two_phase_success_queues_created_before_worker_and_retains_shell() {
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let inspectors = Mutex::new(HashMap::new());
        let joins = Arc::new(TestJoins::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let session_id = SessionId::from(91_050);
        let (runtime, completion) =
            staged_runtime(Arc::clone(&joins), session_id, Arc::clone(&events), false);

        publish_plain(
            Arc::clone(&sessions),
            &inspectors,
            session_id,
            Arc::clone(&runtime),
            Arc::clone(&events),
        )
        .expect("publication succeeds");
        completion.recv_timeout(DEADLINE).expect("worker finishes");
        assert_eq!(runtime.evaluations.load(Ordering::Acquire), 1);
        assert_eq!(&*lock_recover(&events), &["created", "worker"]);
        assert!(
            lock_recover(&sessions)
                .get(&session_id)
                .is_some_and(|current| Arc::ptr_eq(current, &runtime))
        );
        assert_eq!(
            joins.join_exact(session_id),
            RuntimeThreadJoinOutcome::Clean { session_id }
        );
        lock_recover(&sessions).remove(&session_id);
    }

    #[test]
    fn contained_created_publish_failure_is_typed_and_compensated() {
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let inspectors = Mutex::new(HashMap::new());
        let joins = Arc::new(TestJoins::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let session_id = SessionId::from(91_052);
        let (runtime, completion) = staged_runtime(joins, session_id, Arc::clone(&events), false);

        let failure = publish_session(
            Arc::clone(&sessions),
            &inspectors,
            session_id,
            Arc::clone(&runtime),
            |_| Ok::<(), &'static str>(()),
            |_| Ok::<(), &'static str>(()),
            |_, &()| {
                Ok(TestEffect {
                    events: Arc::clone(&events),
                    attempted: false,
                    fail_created: true,
                    panic_created: false,
                    panic_drop: false,
                })
            },
        )
        .expect_err("contained send failure rejects publication");
        assert_eq!(
            failure.primary,
            SessionPublicationError::CreatedBroadcastUnwound
        );
        assert_eq!(
            failure.cleanup,
            RuntimeThreadJoinOutcome::Clean { session_id }
        );
        assert_eq!(&*lock_recover(&events), &["created", "destroyed"]);
        assert_rolled_back(&sessions, &inspectors, &runtime, session_id, completion);
    }

    #[test]
    fn post_commit_effect_drop_unwind_leaves_live_publication_for_ordinary_cleanup() {
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let inspectors = Mutex::new(HashMap::new());
        let joins = Arc::new(TestJoins::default());
        let events = Arc::new(Mutex::new(Vec::new()));
        let session_id = SessionId::from(91_055);
        let (runtime, completion) =
            staged_runtime(Arc::clone(&joins), session_id, Arc::clone(&events), false);

        let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
            publish_session(
                Arc::clone(&sessions),
                &inspectors,
                session_id,
                Arc::clone(&runtime),
                |_| Ok::<(), &'static str>(()),
                |_| Ok::<(), &'static str>(()),
                |_, &()| {
                    Ok(TestEffect {
                        events: Arc::clone(&events),
                        attempted: false,
                        fail_created: false,
                        panic_created: false,
                        panic_drop: true,
                    })
                },
            )
        }));
        assert!(
            result.is_err(),
            "post-commit unwind escapes as an ordinary panic"
        );
        completion.recv_timeout(DEADLINE).expect("worker finishes");
        assert_eq!(runtime.evaluations.load(Ordering::Acquire), 1);
        assert_eq!(&*lock_recover(&events), &["created", "worker"]);
        assert!(
            lock_recover(&sessions)
                .get(&session_id)
                .is_some_and(|current| Arc::ptr_eq(current, &runtime)),
            "committed runtime remains registered for ordinary shutdown"
        );
        assert!(
            joins.contains(session_id),
            "exact join authority remains owned"
        );
        assert_eq!(
            joins.join_exact(session_id),
            RuntimeThreadJoinOutcome::Clean { session_id }
        );
        lock_recover(&sessions).remove(&session_id);
    }

    #[test]
    fn duplicate_and_stale_guard_never_join_or_remove_an_unproven_runtime() {
        let sessions = Arc::new(Mutex::new(HashMap::new()));
        let inspectors = Mutex::new(HashMap::new());
        let events = Arc::new(Mutex::new(Vec::new()));
        let session_id = SessionId::from(91_060);
        let old_joins = Arc::new(TestJoins::default());
        let replacement_joins = Arc::new(TestJoins::default());
        let (old, old_completion) = staged_runtime(
            Arc::clone(&old_joins),
            session_id,
            Arc::clone(&events),
            false,
        );
        let (replacement, replacement_completion) = staged_runtime(
            Arc::clone(&replacement_joins),
            session_id,
            Arc::clone(&events),
            false,
        );

        lock_recover(&sessions).insert(session_id, Arc::clone(&replacement));
        let duplicate = publish_plain(
            Arc::clone(&sessions),
            &inspectors,
            session_id,
            Arc::clone(&replacement),
            Arc::clone(&events),
        )
        .expect_err("duplicate rejects publication");
        assert_eq!(duplicate.primary, SessionPublicationError::DuplicateSession);
        assert_eq!(
            duplicate.cleanup,
            RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined { session_id }
        );
        assert!(!replacement_completion.try_iter().next().is_some());

        let mut stale = SessionPublication::<TestRuntime, TestEffect>::new(
            Arc::clone(&sessions),
            &inspectors,
            session_id,
            Arc::clone(&old),
        );
        stale.inserted = true;
        let address: SocketAddr = "127.0.0.1:9230".parse().expect("test address");
        lock_recover(&inspectors).insert(session_id, address);
        assert_eq!(
            stale.rollback(),
            RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined { session_id }
        );
        assert!(
            lock_recover(&sessions)
                .get(&session_id)
                .is_some_and(|current| Arc::ptr_eq(current, &replacement))
        );
        assert_eq!(lock_recover(&inspectors).get(&session_id), Some(&address));
        old_completion
            .recv_timeout(DEADLINE)
            .expect("stale guard still cancels its own worker");
        assert!(
            old_joins.contains(session_id),
            "stale guard did not join by id"
        );

        replacement.cancel_start();
        assert_eq!(
            old_joins.join_exact(session_id),
            RuntimeThreadJoinOutcome::Clean { session_id }
        );
        assert_eq!(
            replacement_joins.join_exact(session_id),
            RuntimeThreadJoinOutcome::Clean { session_id }
        );
        replacement_completion
            .recv_timeout(DEADLINE)
            .expect("replacement worker finishes");
    }
}
