//! Application-owned physical Web Audio lifecycle coordination.
//!
//! The unique coordinator lives outside iced. The daemon receives only a
//! bounded command capability, so neither core nor a session can own the
//! process service, application gate, registration map, or retirement proof.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::marker::PhantomData;
use std::panic::AssertUnwindSafe;
use std::rc::Rc;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::{self, JoinHandle};

use futures::executor::block_on;
use smudgy_audio::{
    AudioSessionId, MixerControlError, MixerGainState, MixerMasterGainAuthority,
    MixerOutputFailure, MixerOutputFailureReceiver, MixerSessionRegistrar,
    MixerSessionRetirementError, MixerShutdown, SystemMixerService, SystemMixerUnavailable,
};
use smudgy_audio_web::{
    ApplicationAudioOwner, ApplicationAudioRegistrar, AudioHostLimits, PackageAudioControlError,
    PackageAudioControlKey, PackageAudioGainState, PackageAudioScopeError, SessionAudioControlKey,
    SessionAudioPolicyError, SessionAudioRegistration, SessionAudioRegistrationError,
    SessionAudioScope, UnavailableAudioOutputCause, UnavailableSessionAudioRegistration,
    UnavailableSessionAudioRetirement,
};
use smudgy_core::session::runtime::{
    RuntimeAction, RuntimeThreadJoinOutcome, RuntimeThreadPublicationFailure,
};
use smudgy_core::session::{AudioSessionSpawnError, SessionId, registry};

/// Smudgy's one fixed logical and physical output rate.
pub const PHYSICAL_SAMPLE_RATE: u32 = 48_000;

const COORDINATOR_QUEUE_CAPACITY: usize = 128;

#[derive(Clone, Copy, Default)]
struct WorkerStartMode {
    fail_lifecycle: bool,
    fail_coordinator: bool,
}

type RuntimeShutdown = Arc<dyn Fn(SessionId) -> bool + Send + Sync>;
type RuntimeJoin = Arc<dyn Fn(SessionId) -> RuntimeThreadJoinOutcome + Send + Sync>;
type RuntimeJoinAll = Arc<dyn Fn() -> Vec<RuntimeThreadJoinOutcome> + Send + Sync>;
type IoQuiesce = Arc<dyn Fn() + Send + Sync>;

fn request_runtime_shutdown(session_id: SessionId) -> bool {
    registry::get_runtime(session_id)
        .is_some_and(|runtime| runtime.tx().send(RuntimeAction::Shutdown).is_ok())
}

fn join_runtime(session_id: SessionId) -> RuntimeThreadJoinOutcome {
    smudgy_core::session::runtime::join_runtime_thread(session_id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplicationAudioOpenError {
    ApplicationSealed,
    DuplicateSession,
    CoordinatorStopped,
    Mixer(MixerControlError),
    Registration(SessionAudioRegistrationError),
    RegistrationRollback {
        registration: SessionAudioRegistrationError,
        retirement: MixerSessionRetirementError,
    },
}

/// Why one exact session uses hosted emulated output instead of the physical
/// process mixer. This is session-local unless it wraps the retained global
/// system-output cause.
#[derive(Clone, Debug)]
pub enum SessionAudioUnavailable {
    System(SystemMixerUnavailable),
    PhysicalCapacity,
    Infrastructure(Arc<str>),
    Policy(Arc<str>),
}

impl fmt::Display for SessionAudioUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::System(cause) => cause.fmt(formatter),
            Self::PhysicalCapacity => formatter.write_str(
                "the physical session mixer is full; this session uses emulated silent output",
            ),
            Self::Infrastructure(cause) => write!(
                formatter,
                "the application audio coordinator is unavailable; this session uses emulated silent output: {cause}"
            ),
            Self::Policy(cause) => write!(
                formatter,
                "the live audio policy could not be staged; this session uses emulated silent output: {cause}"
            ),
        }
    }
}

impl fmt::Display for ApplicationAudioOpenError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ApplicationSealed => formatter.write_str("application audio is shutting down"),
            Self::DuplicateSession => formatter.write_str("the session already owns audio"),
            Self::CoordinatorStopped => {
                formatter.write_str("the application audio coordinator stopped")
            }
            Self::Mixer(error) => write!(
                formatter,
                "the shared mixer rejected the session: {error:?}"
            ),
            Self::Registration(error) => {
                write!(
                    formatter,
                    "the Web Audio host rejected the session: {error:?}"
                )
            }
            Self::RegistrationRollback {
                registration,
                retirement,
            } => write!(
                formatter,
                "the Web Audio host rejected the session ({registration:?}) and mixer rollback failed ({retirement:?})"
            ),
        }
    }
}

impl std::error::Error for ApplicationAudioOpenError {}

/// Typed failure to apply one live application/session mixer control.
// S5b2b deliberately wires this private production route before S5b4 adds the
// visible consumers. Keep the boundary compiled without pretending a startup
// mutation is a UI use.
#[allow(dead_code)]
#[derive(Clone, Debug)]
pub enum ApplicationAudioControlError {
    /// This launch has no physical mixer. The exact retained start cause is
    /// returned and no applied mixer state exists.
    NotApplicable(SessionAudioUnavailable),
    /// The application coordinator's bounded input queue is currently full.
    CoordinatorSaturated,
    /// The application coordinator has stopped or its response path failed.
    CoordinatorStopped,
    /// Application audio has begun its absorbing shutdown sequence.
    ApplicationSealed,
    /// No live or closing registration has this public session id.
    UnknownSession,
    /// This public session id is currently undergoing exact retirement.
    SessionClosing,
    /// The key names an older registration than the currently live session.
    StaleSession,
    /// No active sandbox-root audio scope has this versionless identity.
    UnknownPackage,
    /// The key names an older active lease for this sandbox root.
    StalePackage,
    /// The lower mixer's independently bounded owner queue is currently full.
    MixerQueueSaturated,
    /// The lower mixer owner stopped without a retained output-failure cause.
    MixerWorkerStopped,
    /// The exact first-writer process-output failure retained by the mixer.
    OutputFailed(MixerOutputFailure),
    /// The requested linear value was not canonical, finite, and in range.
    InvalidGain,
    /// An unexpected lower mixer rejection retained without reclassification.
    MixerRejected(MixerControlError),
    /// A persisted sandbox-root policy could not be staged before runtime
    /// publication.
    PackagePolicy(PackageAudioScopeError),
}

impl fmt::Display for ApplicationAudioControlError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotApplicable(cause) => write!(
                formatter,
                "physical audio controls are unavailable until restart: {cause}"
            ),
            Self::CoordinatorSaturated => {
                formatter.write_str("the audio coordinator command queue is full")
            }
            Self::CoordinatorStopped => formatter.write_str("the audio coordinator stopped"),
            Self::ApplicationSealed => formatter.write_str("application audio is shutting down"),
            Self::UnknownSession => formatter.write_str("the audio session does not exist"),
            Self::SessionClosing => formatter.write_str("the audio session is closing"),
            Self::StaleSession => {
                formatter.write_str("the audio session control belongs to an older generation")
            }
            Self::UnknownPackage => {
                formatter.write_str("the sandbox package audio scope is not active")
            }
            Self::StalePackage => formatter
                .write_str("the sandbox package control belongs to an older isolate generation"),
            Self::MixerQueueSaturated => formatter.write_str("the mixer control queue is full"),
            Self::MixerWorkerStopped => formatter.write_str("the mixer owner stopped"),
            Self::OutputFailed(failure) => {
                write!(formatter, "the process audio output failed: {failure:?}")
            }
            Self::InvalidGain => formatter.write_str("the requested gain is invalid"),
            Self::MixerRejected(error) => {
                write!(formatter, "the mixer rejected the control: {error:?}")
            }
            Self::PackagePolicy(error) => {
                write!(
                    formatter,
                    "the sandbox package audio policy was rejected: {error:?}"
                )
            }
        }
    }
}

impl std::error::Error for ApplicationAudioControlError {}

#[allow(dead_code)]
#[derive(Clone, Copy, Debug)]
enum ApplicationAudioGainUpdate {
    Linear(f32),
    Muted(bool),
    State { linear: f32, muted: bool },
}

#[derive(Clone)]
struct StagedPackageGain {
    owner: Arc<str>,
    name: Arc<str>,
    linear: f32,
    muted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAudioCloseDisposition {
    Requested,
    AlreadyClosing,
    UnknownSession,
    InvalidRuntimeProof,
    CoordinatorStopped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeShutdownRequest {
    /// A shutdown action was queued to the live runtime.
    Requested,
    /// No live sender remained, or exact pre-start cleanup already joined it.
    AlreadyClosed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionAudioCloseResult {
    pub session_id: SessionId,
    pub shutdown: RuntimeShutdownRequest,
    pub runtime: RuntimeThreadJoinOutcome,
    pub publication_failure: Option<RuntimeThreadPublicationFailure>,
    pub retirement: SessionAudioRetirementResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionAudioRetirementResult {
    Physical(Result<(), MixerSessionRetirementError>),
    MixerFree(UnavailableSessionAudioRetirement),
}

impl SessionAudioRetirementResult {
    #[must_use]
    pub fn is_clean_for(self, session_id: SessionId) -> bool {
        match self {
            Self::Physical(result) => result.is_ok(),
            Self::MixerFree(receipt) => receipt.session_id() == u64::from(u32::from(session_id)),
        }
    }
}

impl SessionAudioCloseResult {
    #[must_use]
    pub fn is_clean(self) -> bool {
        // The shutdown disposition is diagnostic: a runtime may already have
        // stopped and left the live registry. The still-owned exact join is
        // the authoritative lifecycle proof.
        self.publication_failure.is_none()
            && self.runtime
                == RuntimeThreadJoinOutcome::Clean {
                    session_id: self.session_id,
                }
            && self.retirement.is_clean_for(self.session_id)
    }
}

#[derive(Debug)]
pub struct ApplicationAudioShutdownReport {
    pub sessions: Vec<SessionAudioCloseResult>,
    pub unowned_runtime_joins: Vec<RuntimeThreadJoinOutcome>,
    pub io_quiesce_attempted: bool,
    pub io_quiesce_clean: bool,
    pub coordinator_joined: bool,
    pub lifecycle_worker_joined: bool,
    pub lifecycle_transport_clean: bool,
    pub output: ApplicationAudioOutputShutdown,
}

#[derive(Debug)]
pub enum ApplicationAudioOutputShutdown {
    Physical(MixerShutdown),
    Unavailable(SystemMixerUnavailable),
}

impl ApplicationAudioOutputShutdown {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        match self {
            // Operational output death remains in `failure` for reporting,
            // but process success is a cleanup claim: a safely joined and
            // retired output is clean even when it failed while running.
            Self::Physical(shutdown) => shutdown.clean,
            Self::Unavailable(cause) => cause.cleanup_proven(),
        }
    }
}

impl ApplicationAudioShutdownReport {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.coordinator_joined
            && self.lifecycle_worker_joined
            && self.lifecycle_transport_clean
            && self.io_quiesce_attempted
            && self.io_quiesce_clean
            && self.sessions.iter().all(|result| result.is_clean())
            && self.unowned_runtime_joins.is_empty()
            && self.output.is_clean()
    }
}

impl fmt::Display for ApplicationAudioShutdownReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "session results={:?}; unowned runtime joins={:?}; I/O quiesce attempted={}; I/O quiesce clean={}; coordinator joined={}; lifecycle worker joined={}; lifecycle transport clean={}; output shutdown={:?}",
            self.sessions,
            self.unowned_runtime_joins,
            self.io_quiesce_attempted,
            self.io_quiesce_clean,
            self.coordinator_joined,
            self.lifecycle_worker_joined,
            self.lifecycle_transport_clean,
            self.output
        )
    }
}

struct LifecycleJob {
    session_id: SessionId,
    shutdown: RuntimeShutdownRequest,
    runtime: Option<RuntimeThreadJoinOutcome>,
    publication_failure: Option<RuntimeThreadPublicationFailure>,
    registration: CoordinatedSessionAudio,
}

enum CoordinatedSessionAudio {
    Physical {
        registration: SessionAudioRegistration,
        emulated_cause: Option<SessionAudioUnavailable>,
    },
    Unavailable {
        registration: UnavailableSessionAudioRegistration,
        cause: SessionAudioUnavailable,
    },
}

impl CoordinatedSessionAudio {
    fn scope(&self) -> SessionAudioScope {
        match self {
            Self::Physical { registration, .. } => registration.scope(),
            Self::Unavailable { registration, .. } => registration.scope(),
        }
    }

    fn seal(&mut self) -> bool {
        match self {
            Self::Physical { registration, .. } => registration.seal(),
            Self::Unavailable { registration, .. } => registration.seal(),
        }
    }

    fn control_key(&self) -> SessionAudioControlKey {
        match self {
            Self::Physical { registration, .. } => registration.control_key(),
            Self::Unavailable { registration, .. } => registration.control_key(),
        }
    }

    fn update_gain(
        &self,
        update: ApplicationAudioGainUpdate,
    ) -> Result<MixerGainState, MixerControlError> {
        match self {
            Self::Physical { registration, .. } => match update {
                ApplicationAudioGainUpdate::Linear(linear) => registration.set_gain_linear(linear),
                ApplicationAudioGainUpdate::Muted(muted) => registration.set_gain_muted(muted),
                ApplicationAudioGainUpdate::State { linear, muted } => {
                    registration.set_gain_state(linear, muted)
                }
            },
            Self::Unavailable { .. } => {
                unreachable!("mixer-free registration cannot receive a gain update")
            }
        }
    }

    fn gain_output_failure(&self) -> Option<MixerOutputFailure> {
        match self {
            Self::Physical { registration, .. } => registration.gain_output_failure(),
            Self::Unavailable { .. } => None,
        }
    }

    fn unavailable_cause(&self) -> Option<&SessionAudioUnavailable> {
        match self {
            Self::Physical { emulated_cause, .. } => emulated_cause.as_ref(),
            Self::Unavailable { cause, .. } => Some(cause),
        }
    }

    fn force_emulated(&mut self, cause: SessionAudioUnavailable) {
        match self {
            Self::Physical {
                registration,
                emulated_cause,
            } => {
                registration.force_emulated_output();
                *emulated_cause = Some(cause);
            }
            Self::Unavailable {
                cause: existing, ..
            } => *existing = cause,
        }
    }

    fn package_control_key(
        &self,
        owner: &str,
        name: &str,
    ) -> Result<PackageAudioControlKey, PackageAudioControlError> {
        match self {
            Self::Physical { registration, .. } => registration.package_control_key(owner, name),
            Self::Unavailable { .. } => {
                unreachable!("mixer-free registration has no package gain state")
            }
        }
    }

    fn update_package_gain(
        &self,
        key: &PackageAudioControlKey,
        update: ApplicationAudioGainUpdate,
    ) -> Result<PackageAudioGainState, PackageAudioControlError> {
        match self {
            Self::Physical { registration, .. } => match update {
                ApplicationAudioGainUpdate::Linear(linear) => {
                    registration.set_package_gain_linear(key, linear)
                }
                ApplicationAudioGainUpdate::Muted(muted) => {
                    registration.set_package_gain_muted(key, muted)
                }
                ApplicationAudioGainUpdate::State { linear, muted } => {
                    registration.set_package_gain_state(key, linear, muted)
                }
            },
            Self::Unavailable { .. } => {
                unreachable!("mixer-free registration has no package gain state")
            }
        }
    }

    fn retire(self) -> SessionAudioRetirementResult {
        match self {
            Self::Physical { registration, .. } => {
                SessionAudioRetirementResult::Physical(block_on(registration.retire()))
            }
            Self::Unavailable { registration, .. } => {
                SessionAudioRetirementResult::MixerFree(registration.retire())
            }
        }
    }
}

enum LifecycleCommand {
    Close(LifecycleJob),
    Shutdown,
}

fn finish_job(job: LifecycleJob, runtime_join: &RuntimeJoin) -> SessionAudioCloseResult {
    // Load-bearing order: the exact runtime cannot touch its scope after this
    // join before mixer retirement starts.
    let runtime = job.runtime.unwrap_or_else(|| runtime_join(job.session_id));
    let retirement = job.registration.retire();
    SessionAudioCloseResult {
        session_id: job.session_id,
        shutdown: job.shutdown,
        runtime,
        publication_failure: job.publication_failure,
        retirement,
    }
}

struct CoordinatorState {
    open: bool,
    registrations: BTreeMap<SessionId, Arc<Mutex<Option<CoordinatedSessionAudio>>>>,
    closing: BTreeSet<SessionId>,
    submitted: BTreeSet<SessionId>,
    /// Exact sealed jobs retained when no lifecycle receiver exists. Ordinary
    /// close only enqueues here; outer shutdown performs the potentially slow
    /// runtime join and registration retirement.
    deferred: Vec<LifecycleJob>,
    completed: Vec<SessionAudioCloseResult>,
    completions: mpsc::Receiver<SessionAudioCloseResult>,
    io_quiesce_attempted: bool,
    io_quiesce_clean: bool,
    lifecycle_transport_clean: bool,
    /// Whether a lifecycle receiver was successfully published. When worker
    /// construction itself failed, close retains the proof-bearing job here
    /// for outer-shutdown drain instead of blocking the caller.
    lifecycle_expected: bool,
}

struct OpenedSessionAudio {
    scope: SessionAudioScope,
    control_key: SessionAudioControlKey,
    unavailable_cause: Option<SessionAudioUnavailable>,
}

enum CoordinatorCommand {
    Open {
        session_id: SessionId,
        reply: mpsc::SyncSender<Result<OpenedSessionAudio, ApplicationAudioOpenError>>,
    },
    Close {
        session_id: SessionId,
        reply: mpsc::SyncSender<SessionAudioCloseDisposition>,
    },
    ClosePrejoined {
        session_id: SessionId,
        failure: RuntimeThreadPublicationFailure,
        cleanup: RuntimeThreadJoinOutcome,
        reply: mpsc::SyncSender<SessionAudioCloseDisposition>,
    },
    #[allow(dead_code)]
    UpdateMasterGain {
        update: ApplicationAudioGainUpdate,
        reply: mpsc::SyncSender<Result<MixerGainState, ApplicationAudioControlError>>,
    },
    StageMasterPolicy {
        linear: f32,
        muted: bool,
        reply: mpsc::SyncSender<Result<MixerGainState, ApplicationAudioControlError>>,
    },
    #[allow(dead_code)]
    UpdateSessionGain {
        session_id: SessionId,
        key: SessionAudioControlKey,
        update: ApplicationAudioGainUpdate,
        reply: mpsc::SyncSender<Result<MixerGainState, ApplicationAudioControlError>>,
    },
    StageSessionPolicy {
        session_id: SessionId,
        key: SessionAudioControlKey,
        linear: f32,
        muted: bool,
        previous_linear: f32,
        previous_muted: bool,
        packages: Vec<StagedPackageGain>,
        reply: mpsc::SyncSender<Result<(), ApplicationAudioControlError>>,
    },
    ForceSessionEmulated {
        session_id: SessionId,
        key: SessionAudioControlKey,
        cause: SessionAudioUnavailable,
        reply: mpsc::SyncSender<Result<(), ApplicationAudioControlError>>,
    },
    #[allow(dead_code)]
    ResolvePackageGain {
        session_id: SessionId,
        session_key: SessionAudioControlKey,
        owner: Arc<str>,
        name: Arc<str>,
        reply: mpsc::SyncSender<Result<PackageAudioControlKey, ApplicationAudioControlError>>,
    },
    #[allow(dead_code)]
    UpdatePackageGain {
        session_id: SessionId,
        session_key: SessionAudioControlKey,
        package_key: PackageAudioControlKey,
        update: ApplicationAudioGainUpdate,
        reply: mpsc::SyncSender<Result<PackageAudioGainState, ApplicationAudioControlError>>,
    },
    Finish {
        reply: mpsc::SyncSender<CoordinatorFinish>,
    },
}

struct CoordinatorFinish {
    sessions: Vec<SessionAudioCloseResult>,
    io_quiesce_attempted: bool,
    io_quiesce_clean: bool,
    lifecycle_transport_clean: bool,
}

/// Cloneable, UI-thread-affine bounded capability passed into iced.
#[derive(Clone)]
pub struct ApplicationAudioController {
    commands: mpsc::SyncSender<CoordinatorCommand>,
    application: ApplicationAudioRegistrar,
    state: Arc<Mutex<CoordinatorState>>,
    lifecycle: mpsc::Sender<LifecycleCommand>,
    runtime_shutdown: RuntimeShutdown,
    runtime_join: RuntimeJoin,
    forced_unavailable: Arc<Mutex<Option<SessionAudioUnavailable>>>,
    _ui_thread: PhantomData<Rc<()>>,
}

impl fmt::Debug for ApplicationAudioController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationAudioController")
            .finish_non_exhaustive()
    }
}

impl ApplicationAudioController {
    fn pending_from_opened(
        &self,
        session_id: SessionId,
        opened: OpenedSessionAudio,
    ) -> PendingSessionAudio {
        PendingSessionAudio {
            controller: self.clone(),
            session_id,
            control_key: opened.control_key,
            scope: opened.scope,
            unavailable_cause: opened.unavailable_cause,
            committed: false,
        }
    }

    pub fn force_new_sessions_unavailable(&self, cause: SessionAudioUnavailable) {
        *lock_forced_unavailable(&self.forced_unavailable) = Some(cause);
    }

    fn force_session_emulated(
        &self,
        session_id: SessionId,
        key: SessionAudioControlKey,
        cause: SessionAudioUnavailable,
    ) -> Result<(), ApplicationAudioControlError> {
        let (reply, response) = mpsc::sync_channel(1);
        let command = CoordinatorCommand::ForceSessionEmulated {
            session_id,
            key,
            cause: cause.clone(),
            reply,
        };
        if self.commands.send(command).is_ok()
            && let Ok(result) = response.recv()
        {
            return result;
        }
        force_session_emulated(&self.state, session_id, key, cause)
    }

    #[allow(dead_code)]
    fn request_gain(
        &self,
        command: CoordinatorCommand,
        response: mpsc::Receiver<Result<MixerGainState, ApplicationAudioControlError>>,
    ) -> Result<MixerGainState, ApplicationAudioControlError> {
        match self.commands.try_send(command) {
            Ok(()) => response
                .recv()
                .unwrap_or(Err(ApplicationAudioControlError::CoordinatorStopped)),
            Err(mpsc::TrySendError::Full(_)) => {
                Err(ApplicationAudioControlError::CoordinatorSaturated)
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err(ApplicationAudioControlError::CoordinatorStopped)
            }
        }
    }

    #[allow(dead_code)]
    fn resolve_package_gain(
        &self,
        session_id: SessionId,
        session_key: SessionAudioControlKey,
        owner: Arc<str>,
        name: Arc<str>,
    ) -> Result<PackageAudioControlKey, ApplicationAudioControlError> {
        let (reply, response) = mpsc::sync_channel(1);
        match self
            .commands
            .try_send(CoordinatorCommand::ResolvePackageGain {
                session_id,
                session_key,
                owner,
                name,
                reply,
            }) {
            Ok(()) => response
                .recv()
                .unwrap_or(Err(ApplicationAudioControlError::CoordinatorStopped)),
            Err(mpsc::TrySendError::Full(_)) => {
                Err(ApplicationAudioControlError::CoordinatorSaturated)
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err(ApplicationAudioControlError::CoordinatorStopped)
            }
        }
    }

    #[allow(dead_code)]
    fn update_package_gain(
        &self,
        session_id: SessionId,
        session_key: SessionAudioControlKey,
        package_key: PackageAudioControlKey,
        update: ApplicationAudioGainUpdate,
    ) -> Result<PackageAudioGainState, ApplicationAudioControlError> {
        let (reply, response) = mpsc::sync_channel(1);
        match self
            .commands
            .try_send(CoordinatorCommand::UpdatePackageGain {
                session_id,
                session_key,
                package_key,
                update,
                reply,
            }) {
            Ok(()) => response
                .recv()
                .unwrap_or(Err(ApplicationAudioControlError::CoordinatorStopped)),
            Err(mpsc::TrySendError::Full(_)) => {
                Err(ApplicationAudioControlError::CoordinatorSaturated)
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                Err(ApplicationAudioControlError::CoordinatorStopped)
            }
        }
    }

    /// Applies the process-master remembered linear gain in coordinator order.
    #[allow(dead_code)]
    pub fn set_master_linear(
        &self,
        linear: f32,
    ) -> Result<MixerGainState, ApplicationAudioControlError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.request_gain(
            CoordinatorCommand::UpdateMasterGain {
                update: ApplicationAudioGainUpdate::Linear(linear),
                reply,
            },
            response,
        )
    }

    /// Applies the process-master mute state in coordinator order.
    #[allow(dead_code)]
    pub fn set_master_muted(
        &self,
        muted: bool,
    ) -> Result<MixerGainState, ApplicationAudioControlError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.request_gain(
            CoordinatorCommand::UpdateMasterGain {
                update: ApplicationAudioGainUpdate::Muted(muted),
                reply,
            },
            response,
        )
    }

    /// Applies a complete process-master policy as one mixer-owner update.
    pub fn set_master_state(
        &self,
        linear: f32,
        muted: bool,
    ) -> Result<MixerGainState, ApplicationAudioControlError> {
        let (reply, response) = mpsc::sync_channel(1);
        self.request_gain(
            CoordinatorCommand::StageMasterPolicy {
                linear,
                muted,
                reply,
            },
            response,
        )
    }

    pub fn begin_session(
        &self,
        session_id: SessionId,
    ) -> Result<PendingSessionAudio, ApplicationAudioOpenError> {
        // The controller is UI-thread affine, so this absent-before-send
        // snapshot uniquely identifies the one request that may have inserted
        // before a coordinator response channel failed. It also prevents the
        // recovery path below from adopting an unrelated already-live
        // generation and returning a second Pending whose Drop could close it.
        {
            let state = lock_state(&self.state);
            if !state.open {
                return Err(ApplicationAudioOpenError::ApplicationSealed);
            }
            if state.registrations.contains_key(&session_id) || state.closing.contains(&session_id)
            {
                return Err(ApplicationAudioOpenError::DuplicateSession);
            }
        }
        if let Some(cause) = lock_forced_unavailable(&self.forced_unavailable).clone() {
            let opened =
                open_unavailable_registration(&self.state, &self.application, session_id, cause)?;
            return Ok(self.pending_from_opened(session_id, opened));
        }
        let (reply, response) = mpsc::sync_channel(1);
        let result = if self
            .commands
            .send(CoordinatorCommand::Open { session_id, reply })
            .is_ok()
        {
            response
                .recv()
                .unwrap_or(Err(ApplicationAudioOpenError::CoordinatorStopped))
        } else {
            Err(ApplicationAudioOpenError::CoordinatorStopped)
        };
        match result {
            Ok(opened) => Ok(self.pending_from_opened(session_id, opened)),
            Err(ApplicationAudioOpenError::ApplicationSealed) => {
                Err(ApplicationAudioOpenError::ApplicationSealed)
            }
            Err(ApplicationAudioOpenError::DuplicateSession) => {
                Err(ApplicationAudioOpenError::DuplicateSession)
            }
            Err(error) => {
                if let Some(opened) = opened_registration(&self.state, session_id) {
                    return Ok(self.pending_from_opened(session_id, opened));
                }
                let cause = SessionAudioUnavailable::Infrastructure(
                    format!("{error}; physical session setup was bypassed").into(),
                );
                let opened = open_unavailable_registration(
                    &self.state,
                    &self.application,
                    session_id,
                    cause,
                )?;
                Ok(self.pending_from_opened(session_id, opened))
            }
        }
    }

    pub fn close_session(&self, session_id: SessionId) -> SessionAudioCloseDisposition {
        {
            let mut state = lock_state(&self.state);
            if state.closing.contains(&session_id) {
                return SessionAudioCloseDisposition::AlreadyClosing;
            }
            if !state.registrations.contains_key(&session_id) {
                return SessionAudioCloseDisposition::UnknownSession;
            }
            // Linearize close before leaving iced, but leave the potentially
            // blocking registration seal and runtime gates off the UI thread.
            state.closing.insert(session_id);
        }
        let (reply, _response) = mpsc::sync_channel(1);
        let command = CoordinatorCommand::Close { session_id, reply };
        match self.commands.try_send(command) {
            Ok(()) => SessionAudioCloseDisposition::Requested,
            Err(mpsc::TrySendError::Full(command)) => {
                let commands = self.commands.clone();
                let state = Arc::clone(&self.state);
                let lifecycle = self.lifecycle.clone();
                let runtime_shutdown = Arc::clone(&self.runtime_shutdown);
                let runtime_join = Arc::clone(&self.runtime_join);
                match thread::Builder::new()
                    .name("smudgy-audio-close-admission".into())
                    .spawn(move || {
                        if commands.send(command).is_err()
                            && let Ok(job) = take_close_job(&state, &runtime_shutdown, session_id)
                        {
                            submit_job(&state, &lifecycle, &runtime_join, job);
                        }
                    }) {
                    Ok(_) => SessionAudioCloseDisposition::Requested,
                    Err(_) => {
                        lock_state(&self.state).closing.remove(&session_id);
                        SessionAudioCloseDisposition::CoordinatorStopped
                    }
                }
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {
                let state = Arc::clone(&self.state);
                let lifecycle = self.lifecycle.clone();
                let runtime_shutdown = Arc::clone(&self.runtime_shutdown);
                let runtime_join = Arc::clone(&self.runtime_join);
                match thread::Builder::new()
                    .name("smudgy-audio-close-fallback".into())
                    .spawn(move || {
                        if let Ok(job) = take_close_job(&state, &runtime_shutdown, session_id) {
                            submit_job(&state, &lifecycle, &runtime_join, job);
                        }
                    }) {
                    Ok(_) => SessionAudioCloseDisposition::Requested,
                    Err(_) => {
                        lock_state(&self.state).closing.remove(&session_id);
                        SessionAudioCloseDisposition::CoordinatorStopped
                    }
                }
            }
        }
    }

    fn close_prejoined_runtime(
        &self,
        session_id: SessionId,
        failure: RuntimeThreadPublicationFailure,
        cleanup: RuntimeThreadJoinOutcome,
    ) -> SessionAudioCloseDisposition {
        let (reply, response) = mpsc::sync_channel(1);
        if self
            .commands
            .send(CoordinatorCommand::ClosePrejoined {
                session_id,
                failure,
                cleanup,
                reply,
            })
            .is_err()
        {
            return self.close_prejoined_direct(session_id, failure, cleanup);
        }
        response
            .recv()
            .unwrap_or_else(|_| self.close_prejoined_direct(session_id, failure, cleanup))
    }

    fn close_prejoined_direct(
        &self,
        session_id: SessionId,
        failure: RuntimeThreadPublicationFailure,
        cleanup: RuntimeThreadJoinOutcome,
    ) -> SessionAudioCloseDisposition {
        match take_prejoined_close_job(&self.state, session_id, failure, cleanup) {
            Ok(job) => {
                submit_job(&self.state, &self.lifecycle, &self.runtime_join, job);
                SessionAudioCloseDisposition::Requested
            }
            Err(disposition) => disposition,
        }
    }
}

/// Opaque application-side control for one exact session registration.
///
/// It contains only an application coordinator capability and an opaque
/// registration key. It exposes neither mixer handles nor script authority.
#[derive(Clone)]
#[allow(dead_code)]
pub struct ApplicationAudioSessionController {
    controller: ApplicationAudioController,
    session_id: SessionId,
    key: SessionAudioControlKey,
    unavailable_cause: Option<SessionAudioUnavailable>,
}

impl fmt::Debug for ApplicationAudioSessionController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationAudioSessionController")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

#[allow(dead_code)]
impl ApplicationAudioSessionController {
    #[must_use]
    pub fn unavailable_cause(&self) -> Option<&SessionAudioUnavailable> {
        self.unavailable_cause.as_ref()
    }

    /// Forces this exact live generation onto hosted emulated output. The
    /// lower scope latch is generation-bound; stale controls cannot affect a
    /// replacement session.
    pub(crate) fn force_emulated(
        &self,
        cause: SessionAudioUnavailable,
    ) -> Result<(), ApplicationAudioControlError> {
        self.controller
            .force_session_emulated(self.session_id, self.key, cause)
    }

    fn update(
        &self,
        update: ApplicationAudioGainUpdate,
    ) -> Result<MixerGainState, ApplicationAudioControlError> {
        if let Some(cause) = self.unavailable_cause.as_ref() {
            return Err(ApplicationAudioControlError::NotApplicable(cause.clone()));
        }
        let (reply, response) = mpsc::sync_channel(1);
        self.controller.request_gain(
            CoordinatorCommand::UpdateSessionGain {
                session_id: self.session_id,
                key: self.key,
                update,
                reply,
            },
            response,
        )
    }

    /// Applies this exact session's remembered linear gain in coordinator order.
    pub fn set_linear(&self, linear: f32) -> Result<MixerGainState, ApplicationAudioControlError> {
        self.update(ApplicationAudioGainUpdate::Linear(linear))
    }

    /// Applies this exact session's mute state in coordinator order.
    pub fn set_muted(&self, muted: bool) -> Result<MixerGainState, ApplicationAudioControlError> {
        self.update(ApplicationAudioGainUpdate::Muted(muted))
    }

    /// Atomically replaces remembered linear gain and mute for this session.
    pub fn set_state(
        &self,
        linear: f32,
        muted: bool,
    ) -> Result<MixerGainState, ApplicationAudioControlError> {
        self.update(ApplicationAudioGainUpdate::State { linear, muted })
    }

    /// Resolves one active versionless sandbox root under this exact session.
    ///
    /// Trusted packages share Main and therefore never produce this controller.
    pub fn package(
        &self,
        owner: impl Into<Arc<str>>,
        name: impl Into<Arc<str>>,
    ) -> Result<ApplicationAudioPackageController, ApplicationAudioControlError> {
        if let Some(cause) = self.unavailable_cause.as_ref() {
            return Err(ApplicationAudioControlError::NotApplicable(cause.clone()));
        }
        let package_key = self.controller.resolve_package_gain(
            self.session_id,
            self.key,
            owner.into(),
            name.into(),
        )?;
        Ok(ApplicationAudioPackageController {
            controller: self.controller.clone(),
            session_id: self.session_id,
            session_key: self.key,
            package_key,
        })
    }

    pub(crate) fn stage_policy(
        &self,
        linear: f32,
        muted: bool,
        previous_linear: f32,
        previous_muted: bool,
        packages: impl IntoIterator<Item = (Arc<str>, Arc<str>, f32, bool)>,
    ) -> Result<(), ApplicationAudioControlError> {
        if let Some(cause) = self.unavailable_cause.as_ref() {
            return Err(ApplicationAudioControlError::NotApplicable(cause.clone()));
        }
        let (reply, response) = mpsc::sync_channel(1);
        let packages = packages
            .into_iter()
            .map(|(owner, name, linear, muted)| StagedPackageGain {
                owner,
                name,
                linear,
                muted,
            })
            .collect();
        self.controller
            .commands
            .send(CoordinatorCommand::StageSessionPolicy {
                session_id: self.session_id,
                key: self.key,
                linear,
                muted,
                previous_linear,
                previous_muted,
                packages,
                reply,
            })
            .map_err(|_| ApplicationAudioControlError::CoordinatorStopped)?;
        response
            .recv()
            .unwrap_or(Err(ApplicationAudioControlError::CoordinatorStopped))
    }
}

/// Opaque application-side controller for one exact active sandbox-root lease.
///
/// It contains no output factory, mixer bus, isolate handle, or script-visible
/// capability. A full engine reload or session replacement stales this exact
/// key; callers resolve a new controller while remembered gain persists.
#[derive(Clone)]
#[allow(dead_code)]
pub struct ApplicationAudioPackageController {
    controller: ApplicationAudioController,
    session_id: SessionId,
    session_key: SessionAudioControlKey,
    package_key: PackageAudioControlKey,
}

impl fmt::Debug for ApplicationAudioPackageController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApplicationAudioPackageController")
            .field("session_id", &self.session_id)
            .finish_non_exhaustive()
    }
}

#[allow(dead_code)]
impl ApplicationAudioPackageController {
    fn update(
        &self,
        update: ApplicationAudioGainUpdate,
    ) -> Result<PackageAudioGainState, ApplicationAudioControlError> {
        self.controller.update_package_gain(
            self.session_id,
            self.session_key,
            self.package_key.clone(),
            update,
        )
    }

    /// Applies this root's remembered linear gain in coordinator order.
    pub fn set_linear(
        &self,
        linear: f32,
    ) -> Result<PackageAudioGainState, ApplicationAudioControlError> {
        self.update(ApplicationAudioGainUpdate::Linear(linear))
    }

    /// Applies this root's mute without discarding remembered linear gain.
    pub fn set_muted(
        &self,
        muted: bool,
    ) -> Result<PackageAudioGainState, ApplicationAudioControlError> {
        self.update(ApplicationAudioGainUpdate::Muted(muted))
    }

    /// Atomically replaces remembered linear gain and mute for this exact root.
    pub fn set_state(
        &self,
        linear: f32,
        muted: bool,
    ) -> Result<PackageAudioGainState, ApplicationAudioControlError> {
        self.update(ApplicationAudioGainUpdate::State { linear, muted })
    }
}

/// Rollback guard spanning audio registration through exact runtime spawn and
/// UI publication. Any returned error or contained unwind closes the staged
/// registration; only the final `commit` disarms it.
pub struct PendingSessionAudio {
    controller: ApplicationAudioController,
    session_id: SessionId,
    control_key: SessionAudioControlKey,
    scope: SessionAudioScope,
    unavailable_cause: Option<SessionAudioUnavailable>,
    committed: bool,
}

pub(crate) enum AbortedSessionSpawnError {
    Runtime(AudioSessionSpawnError),
    Publication {
        session_id: SessionId,
        failure: RuntimeThreadPublicationFailure,
    },
}

impl PendingSessionAudio {
    #[must_use]
    pub fn scope(&self) -> SessionAudioScope {
        self.scope.clone()
    }

    #[must_use]
    pub fn unavailable_cause(&self) -> Option<&SessionAudioUnavailable> {
        self.unavailable_cause.as_ref()
    }

    pub fn commit(mut self) -> ApplicationAudioSessionController {
        self.committed = true;
        ApplicationAudioSessionController {
            controller: self.controller.clone(),
            session_id: self.session_id,
            key: self.control_key,
            unavailable_cause: self.unavailable_cause.clone(),
        }
    }

    pub(crate) fn force_emulated(
        &mut self,
        cause: SessionAudioUnavailable,
    ) -> Result<(), ApplicationAudioControlError> {
        self.scope.force_emulated_output();
        let result = self.controller.force_session_emulated(
            self.session_id,
            self.control_key,
            cause.clone(),
        );
        self.unavailable_cause = Some(cause);
        result
    }

    /// Apply the complete persisted policy before a runtime can receive this
    /// pending scope. In unavailable mode the policy remains a next-start
    /// preference and no live mixer state is claimed.
    pub(crate) fn stage_policy(
        &self,
        linear: f32,
        muted: bool,
        packages: impl IntoIterator<Item = (Arc<str>, Arc<str>, f32, bool)>,
    ) -> Result<(), ApplicationAudioControlError> {
        let (reply, response) = mpsc::sync_channel(1);
        let packages = packages
            .into_iter()
            .map(|(owner, name, linear, muted)| StagedPackageGain {
                owner,
                name,
                linear,
                muted,
            })
            .collect();
        match self
            .controller
            .commands
            .send(CoordinatorCommand::StageSessionPolicy {
                session_id: self.session_id,
                key: self.control_key,
                linear,
                muted,
                previous_linear: 1.0,
                previous_muted: false,
                packages,
                reply,
            }) {
            Ok(()) => response
                .recv()
                .unwrap_or(Err(ApplicationAudioControlError::CoordinatorStopped)),
            Err(_) => Err(ApplicationAudioControlError::CoordinatorStopped),
        }
    }

    pub fn abort(mut self) -> SessionAudioCloseDisposition {
        self.committed = true;
        self.controller.close_session(self.session_id)
    }

    pub(crate) fn abort_spawn_error(
        self,
        error: AudioSessionSpawnError,
    ) -> AbortedSessionSpawnError {
        match error {
            AudioSessionSpawnError::RuntimePublication(error) => {
                let (session_id, failure, cleanup) = error.into_parts();
                let _ = self.abort_prejoined_parts(session_id, failure, cleanup);
                AbortedSessionSpawnError::Publication {
                    session_id,
                    failure,
                }
            }
            other => {
                let _ = self.abort();
                AbortedSessionSpawnError::Runtime(other)
            }
        }
    }

    fn abort_prejoined_parts(
        mut self,
        session_id: SessionId,
        failure: RuntimeThreadPublicationFailure,
        cleanup: RuntimeThreadJoinOutcome,
    ) -> SessionAudioCloseDisposition {
        if session_id != self.session_id || cleanup.session_id() != self.session_id {
            // Leave the guard armed: returning the typed rejection invokes the
            // ordinary close path from Drop, so a forged/mismatched proof can
            // never suppress exact lifecycle cleanup.
            return SessionAudioCloseDisposition::InvalidRuntimeProof;
        }
        self.committed = true;
        self.controller
            .close_prejoined_runtime(session_id, failure, cleanup)
    }
}

impl Drop for PendingSessionAudio {
    fn drop(&mut self) {
        if !self.committed {
            let disposition = self.controller.close_session(self.session_id);
            if !matches!(
                disposition,
                SessionAudioCloseDisposition::Requested
                    | SessionAudioCloseDisposition::AlreadyClosing
            ) {
                log::error!(
                    "staged audio rollback for session {} failed: {disposition:?}",
                    self.session_id
                );
            }
        }
    }
}

fn lock_state(state: &Mutex<CoordinatorState>) -> std::sync::MutexGuard<'_, CoordinatorState> {
    state
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_forced_unavailable(
    cause: &Mutex<Option<SessionAudioUnavailable>>,
) -> std::sync::MutexGuard<'_, Option<SessionAudioUnavailable>> {
    cause
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn lock_registration(
    registration: &Mutex<Option<CoordinatedSessionAudio>>,
) -> std::sync::MutexGuard<'_, Option<CoordinatedSessionAudio>> {
    registration
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn registration_failure(
    failure: smudgy_audio_web::SessionAudioRegistrationFailure,
) -> ApplicationAudioOpenError {
    let registration = failure.error();
    match block_on(failure.into_owner().retire()) {
        Ok(()) => ApplicationAudioOpenError::Registration(registration),
        Err(retirement) => ApplicationAudioOpenError::RegistrationRollback {
            registration,
            retirement,
        },
    }
}

#[derive(Clone)]
enum SessionRegistrationSource {
    Physical {
        sessions: MixerSessionRegistrar,
        master: MixerMasterGainAuthority,
    },
    Unavailable(SystemMixerUnavailable),
}

impl SessionRegistrationSource {
    fn register(
        &self,
        application: &ApplicationAudioRegistrar,
        session_id: SessionId,
    ) -> Result<CoordinatedSessionAudio, ApplicationAudioOpenError> {
        let id = AudioSessionId(u64::from(u32::from(session_id)));
        match self {
            Self::Physical { sessions, master } => {
                let owner = match sessions.add_session(id) {
                    Ok(owner) => owner,
                    Err(MixerControlError::OwnerStopped) => {
                        let Some(failure) = master.output_failure() else {
                            return Err(ApplicationAudioOpenError::Mixer(
                                MixerControlError::OwnerStopped,
                            ));
                        };
                        let cause = SystemMixerUnavailable::from(
                            smudgy_audio::SystemMixerStartError::DriverFailed(failure),
                        );
                        let session_cause = SessionAudioUnavailable::System(cause);
                        return application
                            .register_unavailable_session(
                                id,
                                UnavailableAudioOutputCause::new(session_cause.to_string()),
                            )
                            .map(|registration| CoordinatedSessionAudio::Unavailable {
                                registration,
                                cause: session_cause,
                            })
                            .map_err(ApplicationAudioOpenError::Registration);
                    }
                    Err(MixerControlError::SessionCapacity) => {
                        let cause = SessionAudioUnavailable::PhysicalCapacity;
                        return application
                            .register_unavailable_session(
                                id,
                                UnavailableAudioOutputCause::new(cause.to_string()),
                            )
                            .map(|registration| CoordinatedSessionAudio::Unavailable {
                                registration,
                                cause,
                            })
                            .map_err(ApplicationAudioOpenError::Registration);
                    }
                    Err(error) => return Err(ApplicationAudioOpenError::Mixer(error)),
                };
                application
                    .register_session(owner)
                    .map(|registration| CoordinatedSessionAudio::Physical {
                        registration,
                        emulated_cause: None,
                    })
                    .map_err(registration_failure)
            }
            Self::Unavailable(cause) => application
                .register_unavailable_session(
                    id,
                    UnavailableAudioOutputCause::new(cause.to_string()),
                )
                .map(|registration| CoordinatedSessionAudio::Unavailable {
                    registration,
                    cause: SessionAudioUnavailable::System(cause.clone()),
                })
                .map_err(ApplicationAudioOpenError::Registration),
        }
    }

    fn update_master_gain(
        &self,
        update: ApplicationAudioGainUpdate,
    ) -> Result<MixerGainState, ApplicationAudioControlError> {
        match self {
            Self::Physical { master, .. } => {
                let result = match update {
                    ApplicationAudioGainUpdate::Linear(linear) => master.set_linear(linear),
                    ApplicationAudioGainUpdate::Muted(muted) => master.set_muted(muted),
                    ApplicationAudioGainUpdate::State { linear, muted } => {
                        master.set_state(linear, muted)
                    }
                };
                result.map_err(|error| map_gain_error(error, master.output_failure()))
            }
            Self::Unavailable(cause) => Err(ApplicationAudioControlError::NotApplicable(
                SessionAudioUnavailable::System(cause.clone()),
            )),
        }
    }

    fn stage_master_policy(
        &self,
        linear: f32,
        muted: bool,
    ) -> Result<MixerGainState, ApplicationAudioControlError> {
        match self {
            Self::Physical { master, .. } => master
                .set_state(linear, muted)
                .map_err(|error| map_gain_error(error, master.output_failure())),
            Self::Unavailable(cause) => Err(ApplicationAudioControlError::NotApplicable(
                SessionAudioUnavailable::System(cause.clone()),
            )),
        }
    }
}

fn open_registration(
    state: &Mutex<CoordinatorState>,
    source: &SessionRegistrationSource,
    application: &ApplicationAudioRegistrar,
    session_id: SessionId,
) -> Result<OpenedSessionAudio, ApplicationAudioOpenError> {
    {
        let state = lock_state(state);
        if !state.open {
            return Err(ApplicationAudioOpenError::ApplicationSealed);
        }
        if state.registrations.contains_key(&session_id) || state.closing.contains(&session_id) {
            return Err(ApplicationAudioOpenError::DuplicateSession);
        }
    }
    let registration = source.register(application, session_id)?;
    let scope = registration.scope();
    let control_key = registration.control_key();
    let unavailable_cause = registration.unavailable_cause().cloned();
    let mut state = lock_state(state);
    if !state.open {
        drop(state);
        return Err(match registration.retire() {
            SessionAudioRetirementResult::Physical(Err(retirement)) => {
                ApplicationAudioOpenError::RegistrationRollback {
                    registration: SessionAudioRegistrationError::ApplicationSealed,
                    retirement,
                }
            }
            SessionAudioRetirementResult::Physical(Ok(()))
            | SessionAudioRetirementResult::MixerFree(_) => {
                ApplicationAudioOpenError::ApplicationSealed
            }
        });
    }
    if state.registrations.contains_key(&session_id) || state.closing.contains(&session_id) {
        drop(state);
        let _ = registration.retire();
        return Err(ApplicationAudioOpenError::DuplicateSession);
    }
    state
        .registrations
        .insert(session_id, Arc::new(Mutex::new(Some(registration))));
    Ok(OpenedSessionAudio {
        scope,
        control_key,
        unavailable_cause,
    })
}

fn opened_registration(
    state: &Mutex<CoordinatorState>,
    session_id: SessionId,
) -> Option<OpenedSessionAudio> {
    let state = lock_state(state);
    let registration = state.registrations.get(&session_id)?;
    let registration = lock_registration(registration);
    let registration = registration.as_ref()?;
    Some(OpenedSessionAudio {
        scope: registration.scope(),
        control_key: registration.control_key(),
        unavailable_cause: registration.unavailable_cause().cloned(),
    })
}

fn open_unavailable_registration(
    state: &Mutex<CoordinatorState>,
    application: &ApplicationAudioRegistrar,
    session_id: SessionId,
    cause: SessionAudioUnavailable,
) -> Result<OpenedSessionAudio, ApplicationAudioOpenError> {
    {
        let state = lock_state(state);
        if !state.open {
            return Err(ApplicationAudioOpenError::ApplicationSealed);
        }
        if state.registrations.contains_key(&session_id) || state.closing.contains(&session_id) {
            return Err(ApplicationAudioOpenError::DuplicateSession);
        }
    }
    let id = AudioSessionId(u64::from(u32::from(session_id)));
    let registration = application
        .register_unavailable_session(id, UnavailableAudioOutputCause::new(cause.to_string()))
        .map_err(ApplicationAudioOpenError::Registration)?;
    let registration = CoordinatedSessionAudio::Unavailable {
        registration,
        cause,
    };
    let scope = registration.scope();
    let control_key = registration.control_key();
    let unavailable_cause = registration.unavailable_cause().cloned();
    let mut state = lock_state(state);
    if !state.open {
        drop(state);
        let _ = registration.retire();
        return Err(ApplicationAudioOpenError::ApplicationSealed);
    }
    if state.registrations.contains_key(&session_id) || state.closing.contains(&session_id) {
        drop(state);
        let _ = registration.retire();
        return Err(ApplicationAudioOpenError::DuplicateSession);
    }
    state
        .registrations
        .insert(session_id, Arc::new(Mutex::new(Some(registration))));
    Ok(OpenedSessionAudio {
        scope,
        control_key,
        unavailable_cause,
    })
}

fn map_gain_error(
    error: MixerControlError,
    output_failure: Option<MixerOutputFailure>,
) -> ApplicationAudioControlError {
    match error {
        MixerControlError::Saturated => ApplicationAudioControlError::MixerQueueSaturated,
        MixerControlError::OwnerStopped => output_failure.map_or(
            ApplicationAudioControlError::MixerWorkerStopped,
            ApplicationAudioControlError::OutputFailed,
        ),
        MixerControlError::InvalidGain => ApplicationAudioControlError::InvalidGain,
        other => ApplicationAudioControlError::MixerRejected(other),
    }
}

fn update_session_gain(
    state: &Mutex<CoordinatorState>,
    _source: &SessionRegistrationSource,
    session_id: SessionId,
    key: SessionAudioControlKey,
    update: ApplicationAudioGainUpdate,
) -> Result<MixerGainState, ApplicationAudioControlError> {
    let registration = {
        let state = lock_state(state);
        if !state.open {
            return Err(ApplicationAudioControlError::ApplicationSealed);
        }
        let Some(registration) = state.registrations.get(&session_id) else {
            return Err(if state.closing.contains(&session_id) {
                ApplicationAudioControlError::SessionClosing
            } else {
                ApplicationAudioControlError::UnknownSession
            });
        };
        let registration_guard = lock_registration(registration);
        let Some(registration_value) = registration_guard.as_ref() else {
            return Err(ApplicationAudioControlError::SessionClosing);
        };
        if registration_value.control_key() != key {
            return Err(ApplicationAudioControlError::StaleSession);
        }
        Arc::clone(registration)
    };

    // The lower bounded request can wait for the mixer owner. Keep the exact
    // registration cell on this coordinator stack so the shared map lock does
    // not cross that wait and no raw mixer authority is published.
    let registration = lock_registration(&registration);
    let Some(registration) = registration.as_ref() else {
        return Err(ApplicationAudioControlError::SessionClosing);
    };
    if let Some(cause) = registration.unavailable_cause() {
        return Err(ApplicationAudioControlError::NotApplicable(cause.clone()));
    }
    let result = registration.update_gain(update);
    let output_failure = if matches!(result, Err(MixerControlError::OwnerStopped)) {
        registration.gain_output_failure()
    } else {
        None
    };
    result.map_err(|error| map_gain_error(error, output_failure))
}

fn stage_session_policy(
    state: &Mutex<CoordinatorState>,
    _source: &SessionRegistrationSource,
    session_id: SessionId,
    key: SessionAudioControlKey,
    linear: f32,
    muted: bool,
    previous_linear: f32,
    previous_muted: bool,
    packages: &[StagedPackageGain],
) -> Result<(), ApplicationAudioControlError> {
    let registration = {
        let state = lock_state(state);
        if !state.open {
            return Err(ApplicationAudioControlError::ApplicationSealed);
        }
        let registration = state
            .registrations
            .get(&session_id)
            .ok_or(ApplicationAudioControlError::UnknownSession)?;
        Arc::clone(registration)
    };
    let registration = lock_registration(&registration);
    let registration = registration
        .as_ref()
        .ok_or(ApplicationAudioControlError::SessionClosing)?;
    if registration.control_key() != key {
        return Err(ApplicationAudioControlError::StaleSession);
    }
    if let Some(cause) = registration.unavailable_cause() {
        return Err(ApplicationAudioControlError::NotApplicable(cause.clone()));
    }
    match registration {
        CoordinatedSessionAudio::Physical { registration, .. } => registration
            .stage_gain_policy(
                linear,
                muted,
                previous_linear,
                previous_muted,
                packages.iter().map(|package| {
                    (
                        Arc::clone(&package.owner),
                        Arc::clone(&package.name),
                        package.linear,
                        package.muted,
                    )
                }),
            )
            .map_err(|error| match error {
                SessionAudioPolicyError::Mixer(error) => {
                    map_gain_error(error, registration.gain_output_failure())
                }
                SessionAudioPolicyError::Package(error) => {
                    ApplicationAudioControlError::PackagePolicy(error)
                }
                _ => ApplicationAudioControlError::MixerWorkerStopped,
            }),
        CoordinatedSessionAudio::Unavailable { cause, .. } => {
            Err(ApplicationAudioControlError::NotApplicable(cause.clone()))
        }
    }
}

fn force_session_emulated(
    state: &Mutex<CoordinatorState>,
    session_id: SessionId,
    key: SessionAudioControlKey,
    cause: SessionAudioUnavailable,
) -> Result<(), ApplicationAudioControlError> {
    let registration = {
        let state = lock_state(state);
        if !state.open {
            return Err(ApplicationAudioControlError::ApplicationSealed);
        }
        let registration = state
            .registrations
            .get(&session_id)
            .ok_or(ApplicationAudioControlError::UnknownSession)?;
        Arc::clone(registration)
    };
    let mut registration = lock_registration(&registration);
    let registration = registration
        .as_mut()
        .ok_or(ApplicationAudioControlError::SessionClosing)?;
    if registration.control_key() != key {
        return Err(ApplicationAudioControlError::StaleSession);
    }
    registration.force_emulated(cause);
    Ok(())
}

fn map_package_gain_error(
    error: PackageAudioControlError,
    output_failure: Option<MixerOutputFailure>,
) -> ApplicationAudioControlError {
    match error {
        PackageAudioControlError::UnknownPackage => ApplicationAudioControlError::UnknownPackage,
        PackageAudioControlError::StaleSession => ApplicationAudioControlError::StaleSession,
        PackageAudioControlError::StalePackage => ApplicationAudioControlError::StalePackage,
        PackageAudioControlError::SessionClosed => ApplicationAudioControlError::SessionClosing,
        PackageAudioControlError::InvalidGain => ApplicationAudioControlError::InvalidGain,
        PackageAudioControlError::OutputFailed => output_failure.map_or(
            ApplicationAudioControlError::MixerWorkerStopped,
            ApplicationAudioControlError::OutputFailed,
        ),
        _ => ApplicationAudioControlError::MixerWorkerStopped,
    }
}

fn resolve_package_gain(
    state: &Mutex<CoordinatorState>,
    _source: &SessionRegistrationSource,
    session_id: SessionId,
    session_key: SessionAudioControlKey,
    owner: &str,
    name: &str,
) -> Result<PackageAudioControlKey, ApplicationAudioControlError> {
    let state = lock_state(state);
    if !state.open {
        return Err(ApplicationAudioControlError::ApplicationSealed);
    }
    let Some(registration) = state.registrations.get(&session_id) else {
        return Err(if state.closing.contains(&session_id) {
            ApplicationAudioControlError::SessionClosing
        } else {
            ApplicationAudioControlError::UnknownSession
        });
    };
    let registration = lock_registration(registration);
    let Some(registration) = registration.as_ref() else {
        return Err(ApplicationAudioControlError::SessionClosing);
    };
    if registration.control_key() != session_key {
        return Err(ApplicationAudioControlError::StaleSession);
    }
    if let Some(cause) = registration.unavailable_cause() {
        return Err(ApplicationAudioControlError::NotApplicable(cause.clone()));
    }
    let key = registration
        .package_control_key(owner, name)
        .map_err(|error| map_package_gain_error(error, registration.gain_output_failure()))?;
    if let Some(failure) = registration.gain_output_failure() {
        return Err(ApplicationAudioControlError::OutputFailed(failure));
    }
    Ok(key)
}

fn update_package_gain(
    state: &Mutex<CoordinatorState>,
    _source: &SessionRegistrationSource,
    session_id: SessionId,
    session_key: SessionAudioControlKey,
    package_key: &PackageAudioControlKey,
    update: ApplicationAudioGainUpdate,
) -> Result<PackageAudioGainState, ApplicationAudioControlError> {
    let state = lock_state(state);
    if !state.open {
        return Err(ApplicationAudioControlError::ApplicationSealed);
    }
    let Some(registration) = state.registrations.get(&session_id) else {
        return Err(if state.closing.contains(&session_id) {
            ApplicationAudioControlError::SessionClosing
        } else {
            ApplicationAudioControlError::UnknownSession
        });
    };
    let registration = lock_registration(registration);
    let Some(registration) = registration.as_ref() else {
        return Err(ApplicationAudioControlError::SessionClosing);
    };
    if registration.control_key() != session_key {
        return Err(ApplicationAudioControlError::StaleSession);
    }
    if let Some(cause) = registration.unavailable_cause() {
        return Err(ApplicationAudioControlError::NotApplicable(cause.clone()));
    }
    let result = registration.update_package_gain(package_key, update);
    result.map_err(|error| map_package_gain_error(error, registration.gain_output_failure()))
}

fn update_master_gain(
    state: &Mutex<CoordinatorState>,
    source: &SessionRegistrationSource,
    update: ApplicationAudioGainUpdate,
) -> Result<MixerGainState, ApplicationAudioControlError> {
    if !lock_state(state).open {
        return Err(ApplicationAudioControlError::ApplicationSealed);
    }
    source.update_master_gain(update)
}

fn remove_registration(
    state: &Mutex<CoordinatorState>,
    session_id: SessionId,
) -> Result<CoordinatedSessionAudio, SessionAudioCloseDisposition> {
    let registration = {
        let mut state = lock_state(state);
        let Some(registration) = state.registrations.remove(&session_id) else {
            return Err(if state.closing.contains(&session_id) {
                SessionAudioCloseDisposition::AlreadyClosing
            } else {
                SessionAudioCloseDisposition::UnknownSession
            });
        };
        state.closing.insert(session_id);
        registration
    };
    let mut registration = lock_registration(&registration);
    let Some(registration) = registration.take() else {
        drop(registration);
        let mut state = lock_state(state);
        state.closing.remove(&session_id);
        state.lifecycle_transport_clean = false;
        return Err(SessionAudioCloseDisposition::CoordinatorStopped);
    };
    Ok(registration)
}

fn take_close_job(
    state: &Mutex<CoordinatorState>,
    runtime_shutdown: &RuntimeShutdown,
    session_id: SessionId,
) -> Result<LifecycleJob, SessionAudioCloseDisposition> {
    let mut registration = remove_registration(state, session_id)?;
    registration.seal();
    Ok(LifecycleJob {
        session_id,
        shutdown: if runtime_shutdown(session_id) {
            RuntimeShutdownRequest::Requested
        } else {
            RuntimeShutdownRequest::AlreadyClosed
        },
        runtime: None,
        publication_failure: None,
        registration,
    })
}

fn take_prejoined_close_job(
    state: &Mutex<CoordinatorState>,
    session_id: SessionId,
    failure: RuntimeThreadPublicationFailure,
    cleanup: RuntimeThreadJoinOutcome,
) -> Result<LifecycleJob, SessionAudioCloseDisposition> {
    if cleanup.session_id() != session_id {
        return Err(SessionAudioCloseDisposition::InvalidRuntimeProof);
    }
    let mut registration = remove_registration(state, session_id)?;
    registration.seal();
    Ok(LifecycleJob {
        session_id,
        shutdown: RuntimeShutdownRequest::AlreadyClosed,
        runtime: Some(cleanup),
        publication_failure: Some(failure),
        registration,
    })
}

fn record_result(state: &Mutex<CoordinatorState>, result: SessionAudioCloseResult) {
    let mut state = lock_state(state);
    state.closing.remove(&result.session_id);
    state.submitted.remove(&result.session_id);
    state.completed.push(result);
}

fn submit_job(
    state: &Mutex<CoordinatorState>,
    lifecycle: &mpsc::Sender<LifecycleCommand>,
    _runtime_join: &RuntimeJoin,
    job: LifecycleJob,
) {
    {
        let mut state = lock_state(state);
        state.submitted.insert(job.session_id);
        if !state.lifecycle_expected {
            state.deferred.push(job);
            return;
        }
    }
    if let Err(mpsc::SendError(LifecycleCommand::Close(job))) =
        lifecycle.send(LifecycleCommand::Close(job))
    {
        let mut state_guard = lock_state(state);
        state_guard.lifecycle_transport_clean = false;
        // Preserve nonblocking Close even after an unexpected worker loss.
        // Application shutdown owns the exact deferred join/retirement proof.
        state_guard.deferred.push(job);
    }
}

fn finish_deferred(state: &Mutex<CoordinatorState>, runtime_join: &RuntimeJoin) {
    let jobs = {
        let mut state = lock_state(state);
        std::mem::take(&mut state.deferred)
    };
    for job in jobs {
        record_result(state, finish_job(job, runtime_join));
    }
}

fn drain_completed(state: &Mutex<CoordinatorState>) {
    let mut state = lock_state(state);
    while let Ok(result) = state.completions.try_recv() {
        state.closing.remove(&result.session_id);
        state.submitted.remove(&result.session_id);
        state.completed.push(result);
    }
}

fn wait_for_all(state: &Mutex<CoordinatorState>) {
    loop {
        drain_completed(state);
        let mut state = lock_state(state);
        if state.closing.is_empty() {
            return;
        }
        match state.completions.recv() {
            Ok(result) => {
                state.closing.remove(&result.session_id);
                state.submitted.remove(&result.session_id);
                state.completed.push(result);
            }
            Err(_) => {
                state.lifecycle_transport_clean = false;
                return;
            }
        }
    }
}

fn quiesce_io_once(state: &Mutex<CoordinatorState>, io_quiesce: &IoQuiesce) {
    {
        let mut state = lock_state(state);
        if state.io_quiesce_attempted {
            return;
        }
        // Record the attempt before entering code that may unwind. Recovery
        // must never retry process-global I/O shutdown.
        state.io_quiesce_attempted = true;
    }
    if let Err(payload) = std::panic::catch_unwind(AssertUnwindSafe(|| io_quiesce())) {
        // A panic payload may itself have a hostile destructor. The shutdown
        // report carries the failure without allowing a second unwind to lose
        // registration authority.
        std::mem::forget(payload);
        lock_state(state).io_quiesce_clean = false;
    }
}

#[allow(clippy::too_many_arguments)]
fn run_coordinator(
    commands: mpsc::Receiver<CoordinatorCommand>,
    state: Arc<Mutex<CoordinatorState>>,
    source: SessionRegistrationSource,
    application: ApplicationAudioRegistrar,
    runtime_shutdown: RuntimeShutdown,
    runtime_join: RuntimeJoin,
    io_quiesce: IoQuiesce,
    lifecycle: mpsc::Sender<LifecycleCommand>,
) {
    while let Ok(command) = commands.recv() {
        drain_completed(&state);
        match command {
            CoordinatorCommand::Open { session_id, reply } => {
                let _ = reply.send(open_registration(&state, &source, &application, session_id));
            }
            CoordinatorCommand::Close { session_id, reply } => {
                let result = take_close_job(&state, &runtime_shutdown, session_id);
                let disposition = match result {
                    Ok(job) => {
                        submit_job(&state, &lifecycle, &runtime_join, job);
                        SessionAudioCloseDisposition::Requested
                    }
                    Err(disposition) => disposition,
                };
                let _ = reply.send(disposition);
            }
            CoordinatorCommand::ClosePrejoined {
                session_id,
                failure,
                cleanup,
                reply,
            } => {
                let result = take_prejoined_close_job(&state, session_id, failure, cleanup);
                let disposition = match result {
                    Ok(job) => {
                        submit_job(&state, &lifecycle, &runtime_join, job);
                        SessionAudioCloseDisposition::Requested
                    }
                    Err(disposition) => disposition,
                };
                let _ = reply.send(disposition);
            }
            CoordinatorCommand::UpdateMasterGain { update, reply } => {
                let _ = reply.send(update_master_gain(&state, &source, update));
            }
            CoordinatorCommand::StageMasterPolicy {
                linear,
                muted,
                reply,
            } => {
                let result = if lock_state(&state).open {
                    source.stage_master_policy(linear, muted)
                } else {
                    Err(ApplicationAudioControlError::ApplicationSealed)
                };
                let _ = reply.send(result);
            }
            CoordinatorCommand::UpdateSessionGain {
                session_id,
                key,
                update,
                reply,
            } => {
                let _ = reply.send(update_session_gain(
                    &state, &source, session_id, key, update,
                ));
            }
            CoordinatorCommand::StageSessionPolicy {
                session_id,
                key,
                linear,
                muted,
                previous_linear,
                previous_muted,
                packages,
                reply,
            } => {
                let _ = reply.send(stage_session_policy(
                    &state,
                    &source,
                    session_id,
                    key,
                    linear,
                    muted,
                    previous_linear,
                    previous_muted,
                    &packages,
                ));
            }
            CoordinatorCommand::ForceSessionEmulated {
                session_id,
                key,
                cause,
                reply,
            } => {
                let _ = reply.send(force_session_emulated(&state, session_id, key, cause));
            }
            CoordinatorCommand::ResolvePackageGain {
                session_id,
                session_key,
                owner,
                name,
                reply,
            } => {
                let _ = reply.send(resolve_package_gain(
                    &state,
                    &source,
                    session_id,
                    session_key,
                    &owner,
                    &name,
                ));
            }
            CoordinatorCommand::UpdatePackageGain {
                session_id,
                session_key,
                package_key,
                update,
                reply,
            } => {
                let _ = reply.send(update_package_gain(
                    &state,
                    &source,
                    session_id,
                    session_key,
                    &package_key,
                    update,
                ));
            }
            CoordinatorCommand::Finish { reply } => {
                let ids = {
                    let mut state = lock_state(&state);
                    state.open = false;
                    state.registrations.keys().copied().collect::<Vec<_>>()
                };
                // Signal and seal every session first. Only after the whole set
                // is non-enterable may process I/O quiesce and exact joins run.
                let mut jobs = Vec::with_capacity(ids.len());
                for session_id in ids {
                    if let Ok(job) = take_close_job(&state, &runtime_shutdown, session_id) {
                        jobs.push(job);
                    }
                }
                quiesce_io_once(&state, &io_quiesce);
                for job in jobs {
                    submit_job(&state, &lifecycle, &runtime_join, job);
                }
                finish_deferred(&state, &runtime_join);
                wait_for_all(&state);
                let mut state = lock_state(&state);
                state.completed.sort_by_key(|result| result.session_id);
                let finish = CoordinatorFinish {
                    sessions: std::mem::take(&mut state.completed),
                    io_quiesce_attempted: state.io_quiesce_attempted,
                    io_quiesce_clean: state.io_quiesce_clean,
                    lifecycle_transport_clean: state.lifecycle_transport_clean,
                };
                drop(state);
                let _ = reply.send(finish);
                return;
            }
        }
    }
}

pub(crate) trait ProcessMixer: Sized {
    fn take_output_failure_events(&mut self) -> Option<MixerOutputFailureReceiver>;
    fn session_registrar(&self) -> MixerSessionRegistrar;
    fn master_gain_authority(&self) -> MixerMasterGainAuthority;
    fn shutdown(self) -> MixerShutdown;
}

impl ProcessMixer for SystemMixerService {
    fn take_output_failure_events(&mut self) -> Option<MixerOutputFailureReceiver> {
        SystemMixerService::take_output_failure_events(self)
    }
    fn session_registrar(&self) -> MixerSessionRegistrar {
        self.session_registrar()
    }
    fn master_gain_authority(&self) -> MixerMasterGainAuthority {
        self.master_gain_authority()
    }
    fn shutdown(self) -> MixerShutdown {
        self.shutdown()
    }
}

#[cfg(test)]
impl ProcessMixer for smudgy_audio::MixerService {
    fn take_output_failure_events(&mut self) -> Option<MixerOutputFailureReceiver> {
        smudgy_audio::MixerService::take_output_failure_events(self)
    }
    fn session_registrar(&self) -> MixerSessionRegistrar {
        self.session_registrar()
    }
    fn master_gain_authority(&self) -> MixerMasterGainAuthority {
        self.master_gain_authority()
    }
    fn shutdown(self) -> MixerShutdown {
        self.shutdown()
    }
}

#[derive(Clone, Debug)]
pub enum ApplicationAudioAvailability {
    Physical,
    Unavailable(SystemMixerUnavailable),
    /// The application host remains live, but its process-audio workers could
    /// not be published. Sessions still receive hosted emulated Web Audio.
    InfrastructureUnavailable(Arc<str>),
}

/// Unique outer application authority, retained on `run`'s stack beyond iced.
pub struct ApplicationAudio<S = SystemMixerService> {
    service: Option<S>,
    output_failure_events: Option<MixerOutputFailureReceiver>,
    availability: ApplicationAudioAvailability,
    unavailable_output: Option<SystemMixerUnavailable>,
    owner: Option<ApplicationAudioOwner>,
    controller: ApplicationAudioController,
    state: Arc<Mutex<CoordinatorState>>,
    lifecycle: mpsc::Sender<LifecycleCommand>,
    runtime_shutdown: RuntimeShutdown,
    runtime_join: RuntimeJoin,
    runtime_join_all: RuntimeJoinAll,
    io_quiesce: IoQuiesce,
    coordinator: Option<JoinHandle<()>>,
    lifecycle_worker: Option<JoinHandle<()>>,
    coordinator_expected: bool,
    lifecycle_worker_expected: bool,
}

impl ApplicationAudio<SystemMixerService> {
    /// Starts the application audio host. Worker-construction failures degrade
    /// the retained host to same-owner emulated output; they never remove Web
    /// Audio from session runtimes.
    pub fn start() -> Self {
        let runtime_shutdown: RuntimeShutdown = Arc::new(request_runtime_shutdown);
        let runtime_join: RuntimeJoin = Arc::new(join_runtime);
        let runtime_join_all: RuntimeJoinAll =
            Arc::new(smudgy_core::session::runtime::join_all_runtime_threads);
        let io_quiesce: IoQuiesce = Arc::new(smudgy_core::session::connection::shutdown_io_runtime);
        match SystemMixerService::start(PHYSICAL_SAMPLE_RATE) {
            Ok(service) => match Self::with_service_and_runtime(
                service,
                production_limits(),
                runtime_shutdown,
                runtime_join,
                runtime_join_all,
                io_quiesce,
            ) {
                Ok(application) => application,
                Err((_source, _service)) => {
                    unreachable!("application audio construction retains worker failures")
                }
            },
            Err(error) => {
                let cause = SystemMixerUnavailable::from(error);
                match Self::with_unavailable_and_runtime(
                    cause.clone(),
                    production_limits(),
                    runtime_shutdown,
                    runtime_join,
                    runtime_join_all,
                    io_quiesce,
                ) {
                    Ok(application) => application,
                    Err(_source) => {
                        unreachable!("application audio construction retains worker failures")
                    }
                }
            }
        }
    }
}

impl<S: ProcessMixer> ApplicationAudio<S> {
    fn with_service_and_runtime(
        mut service: S,
        limits: AudioHostLimits,
        runtime_shutdown: RuntimeShutdown,
        runtime_join: RuntimeJoin,
        runtime_join_all: RuntimeJoinAll,
        io_quiesce: IoQuiesce,
    ) -> Result<Self, (std::io::Error, S)> {
        let output_failure_events = service.take_output_failure_events();
        let source = SessionRegistrationSource::Physical {
            sessions: service.session_registrar(),
            master: service.master_gain_authority(),
        };
        Self::with_source_and_runtime(
            Some(service),
            output_failure_events,
            source,
            ApplicationAudioAvailability::Physical,
            limits,
            runtime_shutdown,
            runtime_join,
            runtime_join_all,
            io_quiesce,
            WorkerStartMode::default(),
        )
        .map_err(|(error, service)| {
            (
                error,
                service.expect("physical coordinator construction returns its service"),
            )
        })
    }

    fn with_unavailable_and_runtime(
        cause: SystemMixerUnavailable,
        limits: AudioHostLimits,
        runtime_shutdown: RuntimeShutdown,
        runtime_join: RuntimeJoin,
        runtime_join_all: RuntimeJoinAll,
        io_quiesce: IoQuiesce,
    ) -> Result<Self, std::io::Error> {
        Self::with_source_and_runtime(
            None,
            None,
            SessionRegistrationSource::Unavailable(cause.clone()),
            ApplicationAudioAvailability::Unavailable(cause),
            limits,
            runtime_shutdown,
            runtime_join,
            runtime_join_all,
            io_quiesce,
            WorkerStartMode::default(),
        )
        .map_err(|(error, service)| {
            debug_assert!(service.is_none());
            error
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn with_source_and_runtime(
        service: Option<S>,
        output_failure_events: Option<MixerOutputFailureReceiver>,
        source: SessionRegistrationSource,
        mut availability: ApplicationAudioAvailability,
        limits: AudioHostLimits,
        runtime_shutdown: RuntimeShutdown,
        runtime_join: RuntimeJoin,
        runtime_join_all: RuntimeJoinAll,
        io_quiesce: IoQuiesce,
        worker_mode: WorkerStartMode,
    ) -> Result<Self, (std::io::Error, Option<S>)> {
        let unavailable_output = match &availability {
            ApplicationAudioAvailability::Unavailable(cause) => Some(cause.clone()),
            ApplicationAudioAvailability::Physical
            | ApplicationAudioAvailability::InfrastructureUnavailable(_) => None,
        };
        let owner = ApplicationAudioOwner::new(limits);
        let application = owner.registrar();
        let controller_application = application.clone();
        let forced_unavailable = Arc::new(Mutex::new(None));
        let (commands_tx, commands_rx) = mpsc::sync_channel(COORDINATOR_QUEUE_CAPACITY);
        // Mixer-free registrations are not subject to the physical mixer's
        // 32-input admission bound. The recovery map remains proportional to
        // live SessionStore entries, while the host independently enforces its
        // existing online-context quota. An unbounded transport keeps iced
        // Close responsive when an earlier exact runtime join is deliberately
        // slow.
        let (lifecycle_tx, lifecycle_rx) = mpsc::channel();
        let (completed_tx, completions) = mpsc::channel();
        let state = Arc::new(Mutex::new(CoordinatorState {
            open: true,
            registrations: BTreeMap::new(),
            closing: BTreeSet::new(),
            submitted: BTreeSet::new(),
            completed: Vec::new(),
            deferred: Vec::new(),
            completions,
            io_quiesce_attempted: false,
            io_quiesce_clean: true,
            lifecycle_transport_clean: true,
            lifecycle_expected: false,
        }));
        let mut startup_failures = Vec::new();
        let lifecycle_runtime_join = Arc::clone(&runtime_join);
        let lifecycle_worker = if worker_mode.fail_lifecycle {
            drop(lifecycle_rx);
            drop(completed_tx);
            startup_failures.push("lifecycle worker could not start: injected failure".to_string());
            None
        } else {
            match thread::Builder::new()
                .name("smudgy-audio-lifecycle".to_string())
                .spawn(move || {
                    while let Ok(command) = lifecycle_rx.recv() {
                        match command {
                            LifecycleCommand::Close(job) => {
                                if completed_tx
                                    .send(finish_job(job, &lifecycle_runtime_join))
                                    .is_err()
                                {
                                    break;
                                }
                            }
                            LifecycleCommand::Shutdown => break,
                        }
                    }
                }) {
                Ok(worker) => {
                    lock_state(&state).lifecycle_expected = true;
                    Some(worker)
                }
                Err(error) => {
                    startup_failures.push(format!("lifecycle worker could not start: {error}"));
                    None
                }
            }
        };
        let coordinator_state = Arc::clone(&state);
        let coordinator_shutdown = Arc::clone(&runtime_shutdown);
        let coordinator_join = Arc::clone(&runtime_join);
        let coordinator_io = Arc::clone(&io_quiesce);
        let coordinator_lifecycle = lifecycle_tx.clone();
        let coordinator_source = source.clone();
        let coordinator_application = application.clone();
        let coordinator = if worker_mode.fail_coordinator {
            drop(commands_rx);
            startup_failures
                .push("coordinator worker could not start: injected failure".to_string());
            None
        } else {
            match thread::Builder::new()
                .name("smudgy-audio-coordinator".to_string())
                .spawn(move || {
                    run_coordinator(
                        commands_rx,
                        coordinator_state,
                        coordinator_source,
                        coordinator_application,
                        coordinator_shutdown,
                        coordinator_join,
                        coordinator_io,
                        coordinator_lifecycle,
                    );
                }) {
                Ok(worker) => Some(worker),
                Err(error) => {
                    startup_failures.push(format!("coordinator worker could not start: {error}"));
                    None
                }
            }
        };
        if !startup_failures.is_empty() {
            let cause: Arc<str> = startup_failures.join("; ").into();
            *lock_forced_unavailable(&forced_unavailable) =
                Some(SessionAudioUnavailable::Infrastructure(Arc::clone(&cause)));
            availability = ApplicationAudioAvailability::InfrastructureUnavailable(cause);
        }
        let coordinator_expected = coordinator.is_some();
        let lifecycle_worker_expected = lifecycle_worker.is_some();
        Ok(Self {
            service,
            output_failure_events,
            availability,
            unavailable_output,
            owner: Some(owner),
            controller: ApplicationAudioController {
                commands: commands_tx,
                application: controller_application,
                state: Arc::clone(&state),
                lifecycle: lifecycle_tx.clone(),
                runtime_shutdown: Arc::clone(&runtime_shutdown),
                runtime_join: Arc::clone(&runtime_join),
                forced_unavailable,
                _ui_thread: PhantomData,
            },
            state,
            lifecycle: lifecycle_tx,
            runtime_shutdown,
            runtime_join,
            runtime_join_all,
            io_quiesce,
            coordinator,
            lifecycle_worker,
            coordinator_expected,
            lifecycle_worker_expected,
        })
    }

    #[must_use]
    pub fn controller(&self) -> ApplicationAudioController {
        self.controller.clone()
    }

    /// Takes the unique stream of terminal physical-output failures.
    ///
    /// Recoverable endpoint interruptions remain internal to the mixer. A
    /// terminal failure that happened before this call remains buffered.
    pub fn take_output_failure_events(&mut self) -> Option<MixerOutputFailureReceiver> {
        self.output_failure_events.take()
    }

    #[must_use]
    pub fn availability(&self) -> ApplicationAudioAvailability {
        self.availability.clone()
    }

    fn recover_coordinator_failure(&self) -> CoordinatorFinish {
        let coordinator_failed = self.coordinator_expected;
        let ids = {
            let mut state = lock_state(&self.state);
            state.open = false;
            // A closing id without a durably submitted lifecycle job can only
            // result from an unexpected coordinator unwind. It has no
            // completion that recovery may safely wait for.
            let orphaned = state
                .closing
                .difference(&state.submitted)
                .copied()
                .collect::<Vec<_>>();
            if !orphaned.is_empty() {
                if coordinator_failed {
                    state.lifecycle_transport_clean = false;
                }
                for session_id in orphaned {
                    state.closing.remove(&session_id);
                }
            }
            state.registrations.keys().copied().collect::<Vec<_>>()
        };
        let mut jobs = Vec::new();
        for id in ids {
            if let Ok(job) = take_close_job(&self.state, &self.runtime_shutdown, id) {
                jobs.push(job);
            }
        }
        quiesce_io_once(&self.state, &self.io_quiesce);
        for job in jobs {
            submit_job(&self.state, &self.lifecycle, &self.runtime_join, job);
        }
        finish_deferred(&self.state, &self.runtime_join);
        wait_for_all(&self.state);
        let mut state = lock_state(&self.state);
        if coordinator_failed {
            state.lifecycle_transport_clean = false;
        }
        state.completed.sort_by_key(|result| result.session_id);
        let lifecycle_transport_clean = state.lifecycle_transport_clean;
        CoordinatorFinish {
            sessions: std::mem::take(&mut state.completed),
            io_quiesce_attempted: state.io_quiesce_attempted,
            io_quiesce_clean: state.io_quiesce_clean,
            lifecycle_transport_clean,
        }
    }

    pub fn shutdown(mut self) -> ApplicationAudioShutdownReport {
        if let Some(owner) = self.owner.as_mut() {
            owner.seal();
        }
        let (reply, response) = mpsc::sync_channel(1);
        let finish = if self
            .controller
            .commands
            .send(CoordinatorCommand::Finish { reply })
            .is_ok()
        {
            response.recv().ok()
        } else {
            None
        };
        let coordinator_joined = self
            .coordinator
            .take()
            .map_or(!self.coordinator_expected, |worker| worker.join().is_ok());
        let finish = finish.unwrap_or_else(|| self.recover_coordinator_failure());
        let _ = self.lifecycle.send(LifecycleCommand::Shutdown);
        let lifecycle_worker_joined = self
            .lifecycle_worker
            .take()
            .map_or(!self.lifecycle_worker_expected, |worker| {
                worker.join().is_ok()
            });
        let unowned_runtime_joins = (self.runtime_join_all)();
        let output = match self.service.take() {
            Some(service) => ApplicationAudioOutputShutdown::Physical(service.shutdown()),
            None => {
                ApplicationAudioOutputShutdown::Unavailable(self.unavailable_output.take().expect(
                    "mixer-free application audio retains its unavailable cause until shutdown",
                ))
            }
        };
        self.owner.take();
        ApplicationAudioShutdownReport {
            sessions: finish.sessions,
            unowned_runtime_joins,
            io_quiesce_attempted: finish.io_quiesce_attempted,
            io_quiesce_clean: finish.io_quiesce_clean,
            coordinator_joined,
            lifecycle_worker_joined,
            lifecycle_transport_clean: finish.lifecycle_transport_clean,
            output,
        }
    }

    #[cfg(test)]
    pub(crate) fn seal_application_for_test(&mut self) {
        self.owner
            .as_mut()
            .expect("test application owner is live")
            .seal();
    }
}

#[cfg(test)]
pub(crate) fn test_application_audio_with_core_runtime() -> (
    ApplicationAudio<smudgy_audio::MixerService>,
    smudgy_audio::test_support::TestDriverProbe,
) {
    let (service, probe) = smudgy_audio::test_support::start_test_mixer(
        PHYSICAL_SAMPLE_RATE,
        smudgy_audio::test_support::TestDriverConfig::default(),
    )
    .expect("test mixer starts");
    let application = ApplicationAudio::with_service_and_runtime(
        service,
        production_limits(),
        Arc::new(request_runtime_shutdown),
        Arc::new(join_runtime),
        Arc::new(smudgy_core::session::runtime::join_all_runtime_threads),
        Arc::new(|| {}),
    )
    .unwrap_or_else(|(error, _)| panic!("test lifecycle workers start: {error}"));
    (application, probe)
}

#[cfg(test)]
static CORE_RUNTIME_TEST_LOCK: Mutex<()> = Mutex::new(());

#[cfg(test)]
pub(crate) fn lock_core_runtime_test() -> std::sync::MutexGuard<'static, ()> {
    CORE_RUNTIME_TEST_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
pub(crate) struct TestAudioRenderer {
    stop: Arc<std::sync::atomic::AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

#[cfg(test)]
impl TestAudioRenderer {
    pub(crate) fn start(probe: smudgy_audio::test_support::TestDriverProbe) -> Self {
        use std::sync::atomic::Ordering;

        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let render_stop = Arc::clone(&stop);
        let worker = thread::spawn(move || {
            while !render_stop.load(Ordering::Acquire) {
                let mut output = [0.0; 256];
                let _ = probe.render(&mut output, 2);
                thread::yield_now();
            }
        });
        Self {
            stop,
            worker: Some(worker),
        }
    }
}

#[cfg(test)]
impl Drop for TestAudioRenderer {
    fn drop(&mut self) {
        use std::sync::atomic::Ordering;

        self.stop.store(true, Ordering::Release);
        self.worker
            .take()
            .expect("test renderer joins exactly once")
            .join()
            .expect("test renderer stays healthy");
    }
}

fn production_limits() -> AudioHostLimits {
    AudioHostLimits::unlimited()
        .max_online_contexts(Some(64))
        .max_live_audio_bytes(Some(64 * 1024 * 1024))
        .max_graph_nodes(Some(4_096))
        .max_graph_connections(Some(4_096))
        .max_scheduled_sources(Some(1_024))
        .max_automation_events(Some(4_096))
        .max_queued_control_commands(Some(1_024))
        .max_queued_events(Some(1_024))
        .max_decode_jobs(Some(4))
        .max_streaming_jobs(Some(8))
        .max_offline_render_jobs(Some(2))
}

#[cfg(test)]
mod tests {
    use std::panic::AssertUnwindSafe;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::time::{Duration, Instant};

    use futures::StreamExt;
    use smudgy_audio::test_support::{TestDriverConfig, TestDriverProbe, start_test_mixer};
    use smudgy_audio::{MixerOutputFailure, MixerService, MixerStartError, SystemOutputError};

    use super::*;

    fn test_application(
        events: Arc<Mutex<Vec<String>>>,
    ) -> (ApplicationAudio<MixerService>, TestDriverProbe) {
        let join_events = Arc::clone(&events);
        test_application_with_join(
            events,
            Arc::new(move |id| {
                join_events.lock().unwrap().push(format!("join:{id}"));
                RuntimeThreadJoinOutcome::Clean { session_id: id }
            }),
        )
    }

    fn test_application_with_join(
        events: Arc<Mutex<Vec<String>>>,
        runtime_join: RuntimeJoin,
    ) -> (ApplicationAudio<MixerService>, TestDriverProbe) {
        let io_events = Arc::clone(&events);
        test_application_with_hooks(
            events,
            runtime_join,
            Arc::new(move || io_events.lock().unwrap().push("io".to_string())),
        )
    }

    fn test_application_with_hooks(
        events: Arc<Mutex<Vec<String>>>,
        runtime_join: RuntimeJoin,
        io_quiesce: IoQuiesce,
    ) -> (ApplicationAudio<MixerService>, TestDriverProbe) {
        let (service, probe) =
            start_test_mixer(PHYSICAL_SAMPLE_RATE, TestDriverConfig::default()).unwrap();
        let shutdown_events = events;
        let application = ApplicationAudio::with_service_and_runtime(
            service,
            production_limits(),
            Arc::new(move |id| {
                shutdown_events
                    .lock()
                    .unwrap()
                    .push(format!("shutdown:{id}"));
                true
            }),
            runtime_join,
            Arc::new(Vec::new),
            io_quiesce,
        )
        .unwrap_or_else(|(error, _)| panic!("test lifecycle workers start: {error}"));
        (application, probe)
    }

    fn test_application_with_worker_mode(
        worker_mode: WorkerStartMode,
        runtime_join: RuntimeJoin,
    ) -> ApplicationAudio<MixerService> {
        let (mut service, _probe) =
            start_test_mixer(PHYSICAL_SAMPLE_RATE, TestDriverConfig::default()).unwrap();
        let output_failure_events = service.take_output_failure_events();
        let source = SessionRegistrationSource::Physical {
            sessions: service.session_registrar(),
            master: service.master_gain_authority(),
        };
        ApplicationAudio::with_source_and_runtime(
            Some(service),
            output_failure_events,
            source,
            ApplicationAudioAvailability::Physical,
            production_limits(),
            Arc::new(|_| true),
            runtime_join,
            Arc::new(Vec::new),
            Arc::new(|| {}),
            worker_mode,
        )
        .unwrap_or_else(|(error, _)| panic!("retained-host construction succeeds: {error}"))
    }

    fn test_unavailable_application(
        cause: SystemMixerUnavailable,
    ) -> ApplicationAudio<MixerService> {
        ApplicationAudio::<MixerService>::with_unavailable_and_runtime(
            cause,
            production_limits(),
            Arc::new(|_| false),
            Arc::new(|session_id| RuntimeThreadJoinOutcome::Clean { session_id }),
            Arc::new(Vec::new),
            Arc::new(|| {}),
        )
        .expect("mixer-free lifecycle workers start")
    }

    fn clean_unavailable_cause() -> SystemMixerUnavailable {
        SystemMixerUnavailable::from(MixerStartError::<SystemOutputError>::InvalidSampleRate)
    }

    #[test]
    fn missing_lifecycle_worker_keeps_close_nonblocking_and_shutdown_proves_deferred_retirement() {
        let (join_started_tx, join_started_rx) = mpsc::sync_channel(1);
        let release = Arc::new(std::sync::Barrier::new(2));
        let join_release = Arc::clone(&release);
        let application = test_application_with_worker_mode(
            WorkerStartMode {
                fail_lifecycle: true,
                fail_coordinator: false,
            },
            Arc::new(move |session_id| {
                join_started_tx.send(session_id).unwrap();
                join_release.wait();
                RuntimeThreadJoinOutcome::Clean { session_id }
            }),
        );
        assert!(matches!(
            application.availability(),
            ApplicationAudioAvailability::InfrastructureUnavailable(_)
        ));
        let controller = application.controller();
        controller
            .begin_session(SessionId::from(41))
            .expect("worker failure uses same-host emulated registration")
            .commit();

        let started = Instant::now();
        assert_eq!(
            controller.close_session(SessionId::from(41)),
            SessionAudioCloseDisposition::Requested
        );
        assert!(
            started.elapsed() < Duration::from_millis(250),
            "close waited for the deliberately blocked runtime join"
        );
        assert!(matches!(
            join_started_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));

        let release_worker = thread::spawn(move || {
            assert_eq!(join_started_rx.recv().unwrap(), SessionId::from(41));
            release.wait();
        });
        let report = application.shutdown();
        release_worker.join().unwrap();
        assert_eq!(report.sessions.len(), 1);
        assert!(report.sessions[0].is_clean());
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn missing_coordinator_uses_same_host_and_duplicate_begin_cannot_adopt_live_generation() {
        let application = test_application_with_worker_mode(
            WorkerStartMode {
                fail_lifecycle: false,
                fail_coordinator: true,
            },
            Arc::new(|session_id| RuntimeThreadJoinOutcome::Clean { session_id }),
        );
        let controller = application.controller();
        let first = controller
            .begin_session(SessionId::from(42))
            .expect("direct same-host emulated begin succeeds");
        let first_key = first.control_key;
        let first = first.commit();
        assert!(matches!(
            controller.begin_session(SessionId::from(42)),
            Err(ApplicationAudioOpenError::DuplicateSession)
        ));
        assert_eq!(first.key, first_key);
        assert!(matches!(
            first.set_muted(true),
            Err(ApplicationAudioControlError::NotApplicable(
                SessionAudioUnavailable::Infrastructure(_)
            ))
        ));

        let report = application.shutdown();
        assert_eq!(report.sessions.len(), 1);
        assert!(report.sessions[0].is_clean());
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn unavailable_coordinator_opens_siblings_rolls_back_and_shuts_down_without_mixer_proof() {
        let cause = clean_unavailable_cause();
        let application = test_unavailable_application(cause.clone());
        assert!(matches!(
            application.availability(),
            ApplicationAudioAvailability::Unavailable(ref retained)
                if matches!(retained.error(), MixerStartError::InvalidSampleRate)
        ));
        let controller = application.controller();

        let first_id = SessionId::from(70);
        let first = controller.begin_session(first_id).unwrap();
        assert_eq!(first.scope().session_id(), 70);
        first.commit();
        let second_id = SessionId::from(71);
        let second = controller.begin_session(second_id).unwrap();
        assert_eq!(second.scope().session_id(), 71);
        second.commit();
        assert_eq!(
            controller.close_session(first_id),
            SessionAudioCloseDisposition::Requested
        );

        // Closing one mixer-free generation does not disturb a live sibling,
        // and a fresh staged registration can still be rolled back exactly.
        let rollback_id = SessionId::from(72);
        let rollback = controller.begin_session(rollback_id).unwrap();
        assert_eq!(rollback.scope().session_id(), 72);
        assert_eq!(rollback.abort(), SessionAudioCloseDisposition::Requested);

        let report = application.shutdown();
        assert_eq!(report.sessions.len(), 3);
        for result in &report.sessions {
            assert_eq!(
                result.runtime,
                RuntimeThreadJoinOutcome::Clean {
                    session_id: result.session_id
                }
            );
            match result.retirement {
                SessionAudioRetirementResult::MixerFree(receipt) => {
                    assert_eq!(
                        receipt.session_id(),
                        u64::from(u32::from(result.session_id))
                    );
                    assert_ne!(receipt.generation(), 0);
                }
                SessionAudioRetirementResult::Physical(_) => {
                    panic!("unavailable session fabricated mixer retirement")
                }
            }
            assert!(result.is_clean());
        }
        assert!(matches!(
            report.output,
            ApplicationAudioOutputShutdown::Unavailable(ref retained)
                if matches!(retained.error(), MixerStartError::InvalidSampleRate)
        ));
        assert!(
            report.is_clean(),
            "clean unavailability is not shutdown failure"
        );
    }

    #[test]
    fn unavailable_controls_return_retained_non_applicability_without_applied_state() {
        let cause = clean_unavailable_cause();
        let application = test_unavailable_application(cause);
        let controller = application.controller();
        let session = controller
            .begin_session(SessionId::from(74))
            .unwrap()
            .commit();

        for result in [
            controller.set_master_linear(0.5),
            controller.set_master_muted(true),
            session.set_linear(0.25),
            session.set_muted(true),
        ] {
            assert!(matches!(
                result,
                Err(ApplicationAudioControlError::NotApplicable(ref retained))
                    if matches!(
                        retained,
                        SessionAudioUnavailable::System(cause)
                            if matches!(cause.error(), MixerStartError::InvalidSampleRate)
                    )
            ));
        }
        assert!(matches!(
            session.package("sandbox", "earcon"),
            Err(ApplicationAudioControlError::NotApplicable(ref retained))
                if matches!(
                    retained,
                    SessionAudioUnavailable::System(cause)
                        if matches!(cause.error(), MixerStartError::InvalidSampleRate)
                )
        ));

        let report = application.shutdown();
        assert_eq!(report.sessions.len(), 1);
        assert!(matches!(
            report.sessions[0].retirement,
            SessionAudioRetirementResult::MixerFree(_)
        ));
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn physical_master_and_two_exact_sessions_route_applied_snapshots_in_order() {
        let (application, probe) = test_application(Arc::new(Mutex::new(Vec::new())));
        let controller = application.controller();
        let first = controller
            .begin_session(SessionId::from(75))
            .unwrap()
            .commit();
        let second = controller
            .begin_session(SessionId::from(76))
            .unwrap()
            .commit();

        let master = controller.set_master_linear(0.5).unwrap();
        assert_eq!(master.linear(), 0.5);
        assert!(!master.is_muted());
        let first_state = first.set_linear(0.25).unwrap();
        assert_eq!(first_state.linear(), 0.25);
        assert_eq!(first_state.effective_linear(), 0.25);
        let muted = first.set_muted(true).unwrap();
        assert_eq!(muted.linear(), 0.25);
        assert_eq!(muted.effective_linear(), 0.0);

        // A sibling keeps its own remembered state while the first is muted.
        let second_state = second.set_linear(0.75).unwrap();
        assert_eq!(second_state.linear(), 0.75);
        assert!(!second_state.is_muted());
        let master_muted = controller.set_master_muted(true).unwrap();
        assert_eq!(master_muted.linear(), 0.5);
        assert!(master_muted.is_muted());
        assert_eq!(master_muted.effective_linear(), 0.0);
        assert!(matches!(
            second.set_linear(f32::NAN),
            Err(ApplicationAudioControlError::InvalidGain)
        ));

        let report = shutdown_while_rendering(application, probe);
        assert_eq!(report.sessions.len(), 2);
        assert!(report.sessions.iter().all(|result| result.is_clean()));
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn package_controller_routes_exact_root_generation_and_preserves_reload_state() {
        let (application, probe) = test_application(Arc::new(Mutex::new(Vec::new())));
        let controller = application.controller();
        let pending = controller.begin_session(SessionId::from(79)).unwrap();
        let scope = pending.scope();
        let (_, binding) = scope
            .extension_options_for_sandbox_root("Owner", "Earcon")
            .unwrap();
        let mut binding = binding.expect("physical sandbox gets an exact binding");
        binding.commit().unwrap();
        let session = pending.commit();
        let package = session
            .package("owner", "EARCON")
            .expect("folded active root resolves through the coordinator");
        assert_eq!(package.set_linear(0.4).unwrap().linear(), 0.4);
        assert_eq!(package.set_muted(true).unwrap().effective_linear(), 0.0);

        // A successful full-engine replacement publishes a new exact lease
        // over the same remembered versionless state.
        let (_, replacement_binding) = scope
            .extension_options_for_sandbox_root("OWNER", "earcon")
            .unwrap();
        let mut replacement_binding = replacement_binding.unwrap();
        replacement_binding.commit().unwrap();
        assert!(matches!(
            package.set_muted(false),
            Err(ApplicationAudioControlError::StalePackage)
        ));
        let replacement = session.package("owner", "earcon").unwrap();
        let restored = replacement.set_muted(false).unwrap();
        assert_eq!(restored.linear(), 0.4);
        assert!(!restored.is_muted());

        // Dropping the predecessor lease cannot deactivate its replacement.
        drop(binding);
        assert_eq!(replacement.set_linear(0.25).unwrap().linear(), 0.25);
        drop(replacement_binding);
        assert!(matches!(
            replacement.set_muted(true),
            Err(ApplicationAudioControlError::StalePackage)
        ));

        let report = shutdown_while_rendering(application, probe);
        assert_eq!(report.sessions.len(), 1);
        assert!(report.sessions[0].is_clean());
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn exact_session_control_reports_closing_unknown_then_stale_after_reuse() {
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let release = Arc::new(std::sync::Barrier::new(2));
        let join_release = Arc::clone(&release);
        let block_first = Arc::new(AtomicBool::new(true));
        let join_block_first = Arc::clone(&block_first);
        let (application, probe) = test_application_with_join(
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(move |session_id| {
                if session_id == SessionId::from(77)
                    && join_block_first.swap(false, Ordering::AcqRel)
                {
                    let _ = entered_tx.send(());
                    join_release.wait();
                }
                RuntimeThreadJoinOutcome::Clean { session_id }
            }),
        );
        let renderer = TestAudioRenderer::start(probe);
        let controller = application.controller();
        let old_pending = controller.begin_session(SessionId::from(77)).unwrap();
        let (_, old_package_binding) = old_pending
            .scope()
            .extension_options_for_sandbox_root("stale", "root")
            .unwrap();
        let mut old_package_binding = old_package_binding.unwrap();
        old_package_binding.commit().unwrap();
        let old = old_pending.commit();
        let old_package = old.package("stale", "root").unwrap();
        assert_eq!(
            controller.close_session(SessionId::from(77)),
            SessionAudioCloseDisposition::Requested
        );
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        assert!(matches!(
            old.set_linear(0.5),
            Err(ApplicationAudioControlError::SessionClosing)
        ));
        assert!(matches!(
            old_package.set_linear(0.5),
            Err(ApplicationAudioControlError::SessionClosing)
        ));
        release.wait();
        drop(old_package_binding);

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            match old.set_linear(0.5) {
                Err(ApplicationAudioControlError::UnknownSession) => break,
                Err(ApplicationAudioControlError::SessionClosing) => {
                    assert!(Instant::now() < deadline, "session close did not settle");
                    thread::yield_now();
                }
                other => panic!("unexpected post-close control outcome: {other:?}"),
            }
        }

        let replacement_pending = controller
            .begin_session(SessionId::from(77))
            .expect("exact retirement permits same-id replacement");
        let (_, replacement_package_binding) = replacement_pending
            .scope()
            .extension_options_for_sandbox_root("stale", "root")
            .unwrap();
        let mut replacement_package_binding = replacement_package_binding.unwrap();
        replacement_package_binding.commit().unwrap();
        let replacement = replacement_pending.commit();
        let replacement_package = replacement.package("stale", "root").unwrap();
        assert!(matches!(
            old.set_muted(true),
            Err(ApplicationAudioControlError::StaleSession)
        ));
        assert_eq!(replacement.set_linear(0.625).unwrap().linear(), 0.625);
        assert!(matches!(
            old_package.set_muted(true),
            Err(ApplicationAudioControlError::StaleSession)
        ));
        assert_eq!(replacement_package.set_linear(0.75).unwrap().linear(), 0.75);
        drop(replacement_package_binding);

        let report = application.shutdown();
        drop(renderer);
        assert_eq!(report.sessions.len(), 2);
        assert!(report.sessions.iter().all(|result| result.is_clean()));
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn coordinator_queue_and_lower_queue_outcomes_remain_distinct() {
        let (commands, receiver) = mpsc::sync_channel(1);
        let (occupied_reply, _occupied_response) = mpsc::sync_channel(1);
        commands
            .try_send(CoordinatorCommand::UpdateMasterGain {
                update: ApplicationAudioGainUpdate::Muted(false),
                reply: occupied_reply,
            })
            .unwrap();
        let owner = ApplicationAudioOwner::new(production_limits());
        let (_completed_tx, completions) = mpsc::channel();
        let (lifecycle, _lifecycle_rx) = mpsc::channel();
        let controller = ApplicationAudioController {
            commands,
            application: owner.registrar(),
            state: Arc::new(Mutex::new(CoordinatorState {
                open: true,
                registrations: BTreeMap::new(),
                closing: BTreeSet::new(),
                submitted: BTreeSet::new(),
                deferred: Vec::new(),
                completed: Vec::new(),
                completions,
                io_quiesce_attempted: false,
                io_quiesce_clean: true,
                lifecycle_transport_clean: true,
                lifecycle_expected: false,
            })),
            lifecycle,
            runtime_shutdown: Arc::new(|_| false),
            runtime_join: Arc::new(|session_id| RuntimeThreadJoinOutcome::Clean { session_id }),
            forced_unavailable: Arc::new(Mutex::new(None)),
            _ui_thread: PhantomData,
        };
        assert!(matches!(
            controller.set_master_muted(true),
            Err(ApplicationAudioControlError::CoordinatorSaturated)
        ));
        drop(receiver);
        assert!(matches!(
            controller.set_master_muted(true),
            Err(ApplicationAudioControlError::CoordinatorStopped)
        ));
        assert!(matches!(
            map_gain_error(MixerControlError::Saturated, None),
            ApplicationAudioControlError::MixerQueueSaturated
        ));
        assert!(matches!(
            map_gain_error(MixerControlError::OwnerStopped, None),
            ApplicationAudioControlError::MixerWorkerStopped
        ));
    }

    #[test]
    fn terminal_output_failure_receiver_is_owned_and_taken_exactly_once() {
        let (mut application, probe) = test_application(Arc::new(Mutex::new(Vec::new())));
        let mut failures = application
            .take_output_failure_events()
            .expect("a physical application owns one terminal-failure receiver");
        assert!(application.take_output_failure_events().is_none());

        assert!(probe.fail_output());
        assert_eq!(
            block_on(failures.next()),
            Some(MixerOutputFailure::BackendFailure)
        );
        assert_eq!(block_on(failures.next()), None);

        let report = application.shutdown();
        assert!(report.is_clean());
        assert!(matches!(
            report.output,
            ApplicationAudioOutputShutdown::Physical(MixerShutdown {
                clean: true,
                failure: Some(MixerOutputFailure::BackendFailure),
            })
        ));
    }

    #[test]
    fn output_death_is_retained_by_master_and_session_controls_then_joined() {
        let (application, probe) = test_application(Arc::new(Mutex::new(Vec::new())));
        let controller = application.controller();
        let pending = controller.begin_session(SessionId::from(78)).unwrap();
        let (_, package_binding) = pending
            .scope()
            .extension_options_for_sandbox_root("output", "failure")
            .unwrap();
        let mut package_binding = package_binding.unwrap();
        package_binding.commit().unwrap();
        let session = pending.commit();
        let package = session.package("output", "failure").unwrap();
        assert_eq!(controller.set_master_linear(0.5).unwrap().linear(), 0.5);
        assert_eq!(session.set_linear(0.5).unwrap().linear(), 0.5);
        assert_eq!(package.set_linear(0.5).unwrap().linear(), 0.5);
        assert!(probe.fail_output());
        assert!(matches!(
            controller.set_master_muted(true),
            Err(ApplicationAudioControlError::OutputFailed(
                MixerOutputFailure::BackendFailure
            ))
        ));
        assert!(matches!(
            session.set_muted(true),
            Err(ApplicationAudioControlError::OutputFailed(
                MixerOutputFailure::BackendFailure
            ))
        ));
        assert!(matches!(
            package.set_muted(true),
            Err(ApplicationAudioControlError::OutputFailed(
                MixerOutputFailure::BackendFailure
            ))
        ));
        drop(package_binding);

        let report = application.shutdown();
        assert!(report.coordinator_joined);
        assert!(report.lifecycle_worker_joined);
        assert_eq!(report.sessions.len(), 1);
        assert!(report.sessions[0].is_clean());
        assert!(matches!(
            report.output,
            ApplicationAudioOutputShutdown::Physical(MixerShutdown {
                clean: true,
                failure: Some(MixerOutputFailure::BackendFailure),
            })
        ));
        assert!(
            report.is_clean(),
            "operational output death stays visible in the report without invalidating proven cleanup"
        );
        assert!(matches!(
            controller.set_master_linear(1.0),
            Err(ApplicationAudioControlError::CoordinatorStopped)
        ));
    }

    #[test]
    fn output_death_between_begin_and_initial_policy_keeps_registration_publishable() {
        let (application, probe) = test_application(Arc::new(Mutex::new(Vec::new())));
        let controller = application.controller();
        let pending = controller.begin_session(SessionId::from(79)).unwrap();
        assert!(probe.fail_output());
        assert!(matches!(
            pending.stage_policy(0.5, true, std::iter::empty()),
            Err(ApplicationAudioControlError::OutputFailed(
                MixerOutputFailure::BackendFailure
            ))
        ));
        pending.commit();

        let report = application.shutdown();
        assert_eq!(report.sessions.len(), 1);
        assert!(report.sessions[0].is_clean(), "{report}");
        assert!(matches!(
            report.output,
            ApplicationAudioOutputShutdown::Physical(MixerShutdown {
                clean: true,
                failure: Some(MixerOutputFailure::BackendFailure),
            })
        ));
    }

    #[test]
    fn confirmed_post_death_opens_bounded_mixer_free_sessions_and_reuses_capacity() {
        let (application, probe) = test_application(Arc::new(Mutex::new(Vec::new())));
        let controller = application.controller();
        assert!(probe.fail_output());
        assert!(matches!(
            controller.set_master_muted(true),
            Err(ApplicationAudioControlError::OutputFailed(
                MixerOutputFailure::BackendFailure
            ))
        ));

        for raw in 200..457 {
            let pending = controller
                .begin_session(SessionId::from(raw))
                .expect("confirmed output death falls back to mixer-free registration");
            assert!(matches!(
                pending.stage_policy(0.5, false, std::iter::empty()),
                Err(ApplicationAudioControlError::NotApplicable(_))
            ));
            pending.commit();
        }
        assert_eq!(
            controller.close_session(SessionId::from(200)),
            SessionAudioCloseDisposition::Requested
        );
        let replacement = controller
            .begin_session(SessionId::from(999))
            .expect("mixer-free metadata remains uncapped after exact close");
        assert!(matches!(
            replacement.stage_policy(1.0, false, std::iter::empty()),
            Err(ApplicationAudioControlError::NotApplicable(_))
        ));
        replacement.commit();

        let report = application.shutdown();
        assert_eq!(report.sessions.len(), 258);
        assert!(report.sessions.iter().all(|result| result.is_clean()));
    }

    #[test]
    fn unavailable_seal_rejects_open_and_uncertain_startup_never_reports_clean() {
        let mut sealed = test_unavailable_application(clean_unavailable_cause());
        let controller = sealed.controller();
        sealed.seal_application_for_test();
        assert!(matches!(
            controller.begin_session(SessionId::from(73)),
            Err(ApplicationAudioOpenError::Registration(
                SessionAudioRegistrationError::ApplicationSealed
            ))
        ));
        let sealed_report = sealed.shutdown();
        assert!(sealed_report.sessions.is_empty());
        assert!(sealed_report.is_clean());

        let uncertain =
            SystemMixerUnavailable::from(MixerStartError::<SystemOutputError>::OwnerStopped);
        let uncertain_report = test_unavailable_application(uncertain).shutdown();
        assert!(matches!(
            uncertain_report.output,
            ApplicationAudioOutputShutdown::Unavailable(ref retained)
                if matches!(retained.error(), MixerStartError::OwnerStopped)
                    && !retained.cleanup_proven()
        ));
        assert!(
            !uncertain_report.is_clean(),
            "lost physical startup proof cannot become clean shutdown"
        );
    }

    #[test]
    fn unavailable_registration_admits_257_sessions_and_reuses_exact_close_identity() {
        let application = test_unavailable_application(clean_unavailable_cause());
        let controller = application.controller();
        for id in 100..357 {
            controller
                .begin_session(SessionId::from(id))
                .expect("mixer-free metadata is proportional to live terminal sessions")
                .commit();
        }
        assert_eq!(
            controller.close_session(SessionId::from(100)),
            SessionAudioCloseDisposition::Requested
        );

        let replacement = controller
            .begin_session(SessionId::from(999))
            .expect("exact close permits a new mixer-free generation");
        replacement.commit();

        let report = application.shutdown();
        assert_eq!(report.sessions.len(), 258);
        assert!(report.sessions.iter().all(|result| result.is_clean()));
        assert!(report.is_clean());
    }

    #[test]
    fn exact_clean_join_is_authoritative_when_shutdown_sender_is_already_absent() {
        let session_id = SessionId::from(6);
        let result = |shutdown, runtime, retirement| SessionAudioCloseResult {
            session_id,
            shutdown,
            runtime,
            publication_failure: None,
            retirement,
        };
        let physical = SessionAudioRetirementResult::Physical;

        assert!(
            result(
                RuntimeShutdownRequest::AlreadyClosed,
                RuntimeThreadJoinOutcome::Clean { session_id },
                physical(Ok(()))
            )
            .is_clean(),
            "an exact clean join proves an already-stopped runtime"
        );
        assert!(
            result(
                RuntimeShutdownRequest::Requested,
                RuntimeThreadJoinOutcome::Clean { session_id },
                physical(Ok(()))
            )
            .is_clean()
        );
        assert!(
            !result(
                RuntimeShutdownRequest::Requested,
                RuntimeThreadJoinOutcome::Clean {
                    session_id: SessionId::from(7),
                },
                physical(Ok(())),
            )
            .is_clean(),
            "a clean join for another id is not this session's proof"
        );
        for runtime in [
            RuntimeThreadJoinOutcome::NotTrackedOrAlreadyJoined { session_id },
            RuntimeThreadJoinOutcome::Panicked { session_id },
            RuntimeThreadJoinOutcome::SpawnFailed { session_id },
        ] {
            assert!(
                !result(RuntimeShutdownRequest::Requested, runtime, physical(Ok(()))).is_clean(),
                "a shutdown send cannot promote {runtime:?} into proof"
            );
        }
        assert!(
            !result(
                RuntimeShutdownRequest::Requested,
                RuntimeThreadJoinOutcome::Clean { session_id },
                physical(Err(MixerSessionRetirementError::CleanupFailed)),
            )
            .is_clean(),
            "runtime proof cannot hide failed audio retirement"
        );
    }

    #[test]
    fn coordinator_reports_already_stopped_runtime_clean_from_exact_join() {
        let (service, probe) =
            start_test_mixer(PHYSICAL_SAMPLE_RATE, TestDriverConfig::default()).unwrap();
        let application = ApplicationAudio::with_service_and_runtime(
            service,
            production_limits(),
            Arc::new(|_| false),
            Arc::new(|session_id| RuntimeThreadJoinOutcome::Clean { session_id }),
            Arc::new(Vec::new),
            Arc::new(|| {}),
        )
        .unwrap_or_else(|(error, _)| panic!("test lifecycle workers start: {error}"));
        let controller = application.controller();
        controller
            .begin_session(SessionId::from(7))
            .unwrap()
            .commit();
        assert_eq!(
            controller.close_session(SessionId::from(7)),
            SessionAudioCloseDisposition::Requested
        );

        let report = shutdown_while_rendering(application, probe);
        assert_eq!(report.sessions.len(), 1);
        assert_eq!(
            report.sessions[0].shutdown,
            RuntimeShutdownRequest::AlreadyClosed
        );
        assert!(report.sessions[0].is_clean());
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn prejoined_publication_failure_retires_without_double_join_or_unowned_handle() {
        let joins = Arc::new(AtomicUsize::new(0));
        let join_calls = Arc::clone(&joins);
        let (application, probe) = test_application_with_join(
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(move |session_id| {
                join_calls.fetch_add(1, Ordering::AcqRel);
                RuntimeThreadJoinOutcome::Clean { session_id }
            }),
        );
        let session_id = SessionId::from(8);
        let pending = application.controller().begin_session(session_id).unwrap();
        assert_eq!(
            pending.abort_prejoined_parts(
                session_id,
                RuntimeThreadPublicationFailure::PublicationUnwound,
                RuntimeThreadJoinOutcome::Clean { session_id },
            ),
            SessionAudioCloseDisposition::Requested
        );

        let report = shutdown_while_rendering(application, probe);
        assert_eq!(
            joins.load(Ordering::Acquire),
            0,
            "exact join is not repeated"
        );
        assert!(report.unowned_runtime_joins.is_empty());
        assert_eq!(report.sessions.len(), 1);
        let result = report.sessions[0];
        assert_eq!(
            result.publication_failure,
            Some(RuntimeThreadPublicationFailure::PublicationUnwound)
        );
        assert_eq!(
            result.runtime,
            RuntimeThreadJoinOutcome::Clean { session_id }
        );
        assert!(result.retirement.is_clean_for(session_id));
        assert!(
            !result.is_clean(),
            "cleanup proof cannot erase startup failure"
        );
        assert!(!report.is_clean());
    }

    #[test]
    fn mismatched_prejoined_proof_falls_back_to_ordinary_exact_join() {
        let joins = Arc::new(AtomicUsize::new(0));
        let join_calls = Arc::clone(&joins);
        let (application, probe) = test_application_with_join(
            Arc::new(Mutex::new(Vec::new())),
            Arc::new(move |session_id| {
                join_calls.fetch_add(1, Ordering::AcqRel);
                RuntimeThreadJoinOutcome::Clean { session_id }
            }),
        );
        let session_id = SessionId::from(9);
        let pending = application.controller().begin_session(session_id).unwrap();
        assert_eq!(
            pending.abort_prejoined_parts(
                session_id,
                RuntimeThreadPublicationFailure::PublicationUnwound,
                RuntimeThreadJoinOutcome::Clean {
                    session_id: SessionId::from(10),
                },
            ),
            SessionAudioCloseDisposition::InvalidRuntimeProof
        );

        let report = shutdown_while_rendering(application, probe);
        assert_eq!(
            joins.load(Ordering::Acquire),
            1,
            "rejected proof leaves the rollback guard armed for exact join"
        );
        assert!(report.unowned_runtime_joins.is_empty());
        assert_eq!(report.sessions.len(), 1);
        let result = report.sessions[0];
        assert_eq!(result.shutdown, RuntimeShutdownRequest::Requested);
        assert_eq!(result.publication_failure, None);
        assert_eq!(
            result.runtime,
            RuntimeThreadJoinOutcome::Clean { session_id }
        );
        assert!(result.is_clean());
        assert!(report.is_clean(), "{report}");
    }

    fn shutdown_while_rendering(
        application: ApplicationAudio<MixerService>,
        probe: TestDriverProbe,
    ) -> ApplicationAudioShutdownReport {
        let stop = Arc::new(AtomicBool::new(false));
        let render_stop = Arc::clone(&stop);
        let renderer = thread::spawn(move || {
            while !render_stop.load(Ordering::Acquire) {
                let mut output = [0.0; 256];
                let _ = probe.render(&mut output, 2);
                thread::yield_now();
            }
        });
        let report = application.shutdown();
        stop.store(true, Ordering::Release);
        renderer.join().unwrap();
        report
    }

    #[test]
    fn fixed_rate_duplicate_close_and_join_barrier_are_exact() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (application, probe) = test_application(Arc::clone(&events));
        assert_eq!(
            application.service.as_ref().unwrap().format().sample_rate(),
            48_000
        );
        let controller = application.controller();
        controller
            .begin_session(SessionId::from(7))
            .unwrap()
            .commit();
        assert_eq!(
            controller.close_session(SessionId::from(7)),
            SessionAudioCloseDisposition::Requested
        );
        assert_eq!(
            controller.close_session(SessionId::from(7)),
            SessionAudioCloseDisposition::AlreadyClosing
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while events.lock().unwrap().len() < 2 {
            assert!(Instant::now() < deadline, "runtime join did not begin");
            thread::yield_now();
        }
        assert_eq!(&*events.lock().unwrap(), &["shutdown:7", "join:7"]);
        let report = shutdown_while_rendering(application, probe);
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn close_admission_never_waits_for_a_busy_coordinator() {
        let (service, probe) =
            start_test_mixer(PHYSICAL_SAMPLE_RATE, TestDriverConfig::default()).unwrap();
        let (entered, entered_receiver) = mpsc::sync_channel(1);
        let release = Arc::new(std::sync::Barrier::new(2));
        let shutdown_release = Arc::clone(&release);
        let first = AtomicBool::new(true);
        let application = ApplicationAudio::with_service_and_runtime(
            service,
            production_limits(),
            Arc::new(move |session_id| {
                if session_id == SessionId::from(70) && first.swap(false, Ordering::AcqRel) {
                    let _ = entered.send(());
                    shutdown_release.wait();
                }
                true
            }),
            Arc::new(|session_id| RuntimeThreadJoinOutcome::Clean { session_id }),
            Arc::new(Vec::new),
            Arc::new(|| {}),
        )
        .unwrap_or_else(|(error, _)| panic!("test lifecycle workers start: {error}"));
        let controller = application.controller();
        for id in [70, 71] {
            controller
                .begin_session(SessionId::from(id))
                .unwrap()
                .commit();
        }

        assert_eq!(
            controller.close_session(SessionId::from(70)),
            SessionAudioCloseDisposition::Requested
        );
        entered_receiver
            .recv_timeout(Duration::from_secs(3))
            .unwrap();
        let started = Instant::now();
        assert_eq!(
            controller.close_session(SessionId::from(71)),
            SessionAudioCloseDisposition::Requested
        );
        assert!(
            started.elapsed() < Duration::from_millis(100),
            "iced close waited for coordinator-owned session gates"
        );

        release.wait();
        let report = shutdown_while_rendering(application, probe);
        assert_eq!(report.sessions.len(), 2);
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn quit_signals_every_session_then_quiesces_io_before_join() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (application, probe) = test_application(Arc::clone(&events));
        let controller = application.controller();
        controller
            .begin_session(SessionId::from(10))
            .unwrap()
            .commit();
        controller
            .begin_session(SessionId::from(11))
            .unwrap()
            .commit();
        let report = shutdown_while_rendering(application, probe);
        let events = events.lock().unwrap();
        let io = events.iter().position(|event| event == "io").unwrap();
        let joins = events
            .iter()
            .enumerate()
            .filter_map(|(index, event)| event.starts_with("join:").then_some(index))
            .collect::<Vec<_>>();
        assert!(
            events[..io]
                .iter()
                .filter(|event| event.starts_with("shutdown:"))
                .count()
                == 2
        );
        assert!(joins.into_iter().all(|join| join > io));
        assert_eq!(report.sessions.len(), 2);
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn application_seal_rejects_new_registration_without_publication() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (mut application, probe) = test_application(events);
        let controller = application.controller();
        application.owner.as_mut().unwrap().seal();
        let rollback_probe = probe.clone();
        let stop = Arc::new(AtomicBool::new(false));
        let render_stop = Arc::clone(&stop);
        let renderer = thread::spawn(move || {
            while !render_stop.load(Ordering::Acquire) {
                let mut output = [0.0; 256];
                let _ = rollback_probe.render(&mut output, 2);
                thread::yield_now();
            }
        });
        let result = controller.begin_session(SessionId::from(20));
        stop.store(true, Ordering::Release);
        renderer.join().unwrap();
        assert!(matches!(
            result,
            Err(ApplicationAudioOpenError::Registration(
                SessionAudioRegistrationError::ApplicationSealed
            ))
        ));
        let report = shutdown_while_rendering(application, probe);
        assert!(
            report.sessions.is_empty(),
            "failed open was never published"
        );
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn mass_close_stays_responsive_while_first_exact_join_is_blocked() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let (entered_tx, entered_rx) = mpsc::sync_channel(1);
        let release = Arc::new(std::sync::Barrier::new(2));
        let join_release = Arc::clone(&release);
        let join_events = Arc::clone(&events);
        let (application, probe) = test_application_with_join(
            Arc::clone(&events),
            Arc::new(move |id| {
                join_events.lock().unwrap().push(format!("join:{id}"));
                if id == SessionId::from(1) {
                    let _ = entered_tx.send(());
                    join_release.wait();
                }
                RuntimeThreadJoinOutcome::Clean { session_id: id }
            }),
        );
        let controller = application.controller();
        for id in 1..=32 {
            controller
                .begin_session(SessionId::from(id))
                .unwrap()
                .commit();
        }
        assert!(matches!(
            controller.begin_session(SessionId::from(1)),
            Err(ApplicationAudioOpenError::DuplicateSession)
        ));
        let silent = controller
            .begin_session(SessionId::from(33))
            .expect("the 33rd terminal receives session-local emulated output");
        assert!(matches!(
            silent.unavailable_cause(),
            Some(SessionAudioUnavailable::PhysicalCapacity)
        ));
        silent.commit();
        assert_eq!(
            controller.close_session(SessionId::from(1)),
            SessionAudioCloseDisposition::Requested
        );
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let started = Instant::now();
        for id in 2..=33 {
            assert_eq!(
                controller.close_session(SessionId::from(id)),
                SessionAudioCloseDisposition::Requested
            );
        }
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "iced close commands blocked behind the first runtime join"
        );
        release.wait();
        let report = shutdown_while_rendering(application, probe);
        assert_eq!(report.sessions.len(), 33);
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn physical_capacity_is_session_local_and_a_later_session_uses_a_released_slot() {
        let (application, probe) = test_application(Arc::new(Mutex::new(Vec::new())));
        let renderer = TestAudioRenderer::start(probe);
        let controller = application.controller();
        for id in 1..=32 {
            let pending = controller.begin_session(SessionId::from(id)).unwrap();
            assert!(pending.unavailable_cause().is_none());
            pending.commit();
        }
        let silent = controller.begin_session(SessionId::from(33)).unwrap();
        assert!(matches!(
            silent.unavailable_cause(),
            Some(SessionAudioUnavailable::PhysicalCapacity)
        ));
        let silent = silent.commit();
        assert!(matches!(
            application.availability(),
            ApplicationAudioAvailability::Physical
        ));

        assert_eq!(
            controller.close_session(SessionId::from(1)),
            SessionAudioCloseDisposition::Requested
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        while lock_state(&application.state)
            .closing
            .contains(&SessionId::from(1))
        {
            assert!(Instant::now() < deadline, "physical slot was not retired");
            let _ = controller.set_master_muted(false);
            thread::yield_now();
        }
        let later = controller.begin_session(SessionId::from(34)).unwrap();
        assert!(
            later.unavailable_cause().is_none(),
            "a later terminal receives physical admission after exact retirement"
        );
        later.commit();
        assert!(matches!(
            silent.set_muted(true),
            Err(ApplicationAudioControlError::NotApplicable(
                SessionAudioUnavailable::PhysicalCapacity
            ))
        ));

        let report = application.shutdown();
        drop(renderer);
        assert_eq!(report.sessions.len(), 34);
        assert!(report.sessions.iter().all(|result| result.is_clean()));
        assert!(report.is_clean(), "{report}");
    }

    #[test]
    fn quiesce_panic_still_joins_retires_and_shuts_output_for_iced_ok_or_error() {
        for iced_failed in [false, true] {
            let events = Arc::new(Mutex::new(Vec::new()));
            let join_events = Arc::clone(&events);
            let io_events = Arc::clone(&events);
            let (application, probe) = test_application_with_hooks(
                Arc::clone(&events),
                Arc::new(move |id| {
                    join_events.lock().unwrap().push(format!("join:{id}"));
                    RuntimeThreadJoinOutcome::Clean { session_id: id }
                }),
                Arc::new(move || {
                    io_events.lock().unwrap().push("io:panic".to_string());
                    panic!("injected I/O quiesce panic");
                }),
            );
            let controller = application.controller();
            for id in [50, 51] {
                controller
                    .begin_session(SessionId::from(id))
                    .unwrap()
                    .commit();
            }

            let report = shutdown_while_rendering(application, probe);
            assert!(report.io_quiesce_attempted);
            assert!(!report.io_quiesce_clean);
            assert_eq!(report.sessions.len(), 2);
            assert!(
                report
                    .sessions
                    .iter()
                    .all(|result| result.retirement.is_clean_for(result.session_id))
            );
            assert!(report.output.is_clean(), "physical output still shut down");
            assert!(!report.is_clean(), "the quiesce panic remains visible");
            let events = events.lock().unwrap();
            let io = events.iter().position(|event| event == "io:panic").unwrap();
            assert!(
                events[..io]
                    .iter()
                    .filter(|event| event.starts_with("shutdown:"))
                    .count()
                    == 2
            );
            assert!(
                events[io + 1..]
                    .iter()
                    .all(|event| event.starts_with("join:"))
            );
            drop(events);

            let combined = if iced_failed {
                crate::finish_run_with_audio::<anyhow::Error>(
                    Err(anyhow::anyhow!("injected iced failure")),
                    report,
                )
            } else {
                crate::finish_run_with_audio::<anyhow::Error>(Ok(()), report)
            };
            let combined = combined.expect_err("the quiesce panic is never reported as clean");
            let message = combined.to_string();
            assert!(message.contains("I/O quiesce clean=false"));
            assert_eq!(message.contains("injected iced failure"), iced_failed);
        }
    }

    #[test]
    fn uncommitted_open_guard_retires_after_spawn_failure_or_panic() {
        for outcome in [
            RuntimeThreadJoinOutcome::SpawnFailed {
                session_id: SessionId::from(40),
            },
            RuntimeThreadJoinOutcome::Panicked {
                session_id: SessionId::from(40),
            },
        ] {
            let events = Arc::new(Mutex::new(Vec::new()));
            let join_events = Arc::clone(&events);
            let (application, probe) = test_application_with_join(
                Arc::clone(&events),
                Arc::new(move |id| {
                    join_events.lock().unwrap().push(format!("join:{id}"));
                    outcome
                }),
            );
            let pending = application
                .controller()
                .begin_session(SessionId::from(40))
                .unwrap();
            if matches!(outcome, RuntimeThreadJoinOutcome::SpawnFailed { .. }) {
                assert_eq!(
                    pending.abort(),
                    SessionAudioCloseDisposition::Requested,
                    "ordinary spawn failure takes the explicit rollback path"
                );
            } else {
                let unwind = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    let _rollback_guard = pending;
                    panic!("injected construction panic");
                }));
                assert!(unwind.is_err(), "the rollback guard survived the unwind");
            }
            let report = shutdown_while_rendering(application, probe);
            assert_eq!(report.sessions.len(), 1);
            assert_eq!(report.sessions[0].runtime, outcome);
            assert!(
                report.sessions[0]
                    .retirement
                    .is_clean_for(report.sessions[0].session_id)
            );
            assert!(!report.is_clean(), "spawn failure/panic must stay visible");
            let events = events.lock().unwrap();
            let shutdown = events
                .iter()
                .position(|event| event == "shutdown:40")
                .unwrap();
            let join = events.iter().position(|event| event == "join:40").unwrap();
            assert!(
                shutdown < join,
                "runtime shutdown must precede its exact join"
            );
            assert_eq!(
                events
                    .iter()
                    .filter(|event| *event == "shutdown:40")
                    .count(),
                1
            );
            // Rollback starts retirement before application shutdown, so its
            // exact join can race with the global I/O-quiesce boundary.
            assert_eq!(events.iter().filter(|event| *event == "io").count(), 1);
            assert_eq!(events.iter().filter(|event| *event == "join:40").count(), 1);
        }
    }
}
