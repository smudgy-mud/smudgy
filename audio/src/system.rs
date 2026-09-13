//! Optional direct system-output adapter.
//!
//! CPAL and the generic driver seam stay private. The public wrapper exposes
//! only Smudgy-owned formats, handles, and stable error classifications.

use std::{
    cell::UnsafeCell,
    fmt,
    mem::ManuallyDrop,
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use cpal::{
    BufferSize, ErrorKind as CpalErrorKind, SampleFormat, StreamConfig, SupportedBufferSize,
    traits::{DeviceTrait, HostTrait, StreamTrait},
};
use rtrb::{Consumer, PushError, RingBuffer};

use super::{
    AudioSessionId, CallbackStallPolicy, DriverFailureSignal, DriverMaintenance,
    JoinedOutputDriver, JoinedRenderer, MAX_PHYSICAL_CALLBACK_FRAMES, MixerControlError,
    MixerFormat, MixerOutputFailure, MixerService, MixerSessionOwner, MixerSessionRegistrar,
    MixerShutdown, MixerStartError, PhysicalOutputFormat, PhysicalSampleFormat,
};

const PHYSICAL_CHANNELS: usize = 2;
const PHYSICAL_SCRATCH_SAMPLES: usize = MAX_PHYSICAL_CALLBACK_FRAMES * PHYSICAL_CHANNELS;
const CALLBACK_RETIREMENT_POLL: Duration = Duration::from_millis(1);
const RECOVERY_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const RECOVERY_MAX_BACKOFF: Duration = Duration::from_secs(5);
const RECOVERY_MAX_NULL_ADVANCE: Duration = Duration::from_millis(100);
const CALLBACK_STALL_TIMEOUT: Duration = Duration::from_secs(2);
const OWNER_RESUME_GAP: Duration = Duration::from_millis(500);
/// Output-demand silence threshold: one 16-bit LSB. Nothing below it reaches a
/// listener on any PCM endpoint, so it cannot justify holding the device open.
const AUDIBLE_SAMPLE_THRESHOLD: f32 = 1.0 / 32_768.0;
/// Continuous silence before the demand-driven policy releases the stream.
const OUTPUT_IDLE_DETACH_AFTER: Duration = Duration::from_secs(10);
const RUNTIME_ERROR_QUEUE_CAPACITY: usize = 32;
const ERROR_ADMISSION_PHASE_MASK: usize = 0b11;
const ERROR_ADMISSION_COUNT_ONE: usize = 0b100;
static SYSTEM_OUTPUT_LEASED: AtomicBool = AtomicBool::new(false);

fn recovery_attempt_is_warning(attempt: u32) -> bool {
    attempt.is_power_of_two()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
enum EndpointPhase {
    Active = 0,
    Provisional = 1,
    Failed = 2,
}

impl EndpointPhase {
    fn from_admission(admission: usize) -> Option<Self> {
        match admission & ERROR_ADMISSION_PHASE_MASK {
            value if value == Self::Active as usize => Some(Self::Active),
            value if value == Self::Provisional as usize => Some(Self::Provisional),
            value if value == Self::Failed as usize => Some(Self::Failed),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GenerationActivation {
    Activated,
    Deferred,
    RuntimeError,
    Invalid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
enum DriverRuntimeEvent {
    DeviceBusy,
    DeviceChanged,
    DeviceNotAvailable,
    HostUnavailable,
    InvalidInput,
    PermissionDenied,
    RealtimeDenied,
    ResourceExhausted,
    StreamInvalidated,
    UnsupportedConfig,
    UnsupportedOperation,
    Xrun,
    BackendError,
    Other,
    CallbackStalled,
}

impl DriverRuntimeEvent {
    const ALL: [Self; 15] = [
        Self::DeviceBusy,
        Self::DeviceChanged,
        Self::DeviceNotAvailable,
        Self::HostUnavailable,
        Self::InvalidInput,
        Self::PermissionDenied,
        Self::RealtimeDenied,
        Self::ResourceExhausted,
        Self::StreamInvalidated,
        Self::UnsupportedConfig,
        Self::UnsupportedOperation,
        Self::Xrun,
        Self::BackendError,
        Self::Other,
        Self::CallbackStalled,
    ];

    const fn bit(self) -> u64 {
        1 << self as u8
    }

    const fn is_advisory(self) -> bool {
        matches!(
            self,
            Self::DeviceChanged | Self::RealtimeDenied | Self::Xrun
        )
    }

    const fn is_recoverable(self) -> bool {
        matches!(
            self,
            Self::DeviceBusy
                | Self::DeviceNotAvailable
                | Self::HostUnavailable
                | Self::ResourceExhausted
                | Self::StreamInvalidated
                | Self::BackendError
                | Self::CallbackStalled
        )
    }
}

#[cfg(test)]
struct TestProofHook {
    before_unwrap_entered: std::sync::mpsc::SyncSender<()>,
    before_unwrap_release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    unwrap_attempted: std::sync::mpsc::SyncSender<bool>,
    unwrap_notified: AtomicBool,
    late_upgrade_entered: std::sync::mpsc::SyncSender<()>,
    late_upgrade_release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
}

#[cfg(test)]
impl TestProofHook {
    fn wait(gate: &(std::sync::Mutex<bool>, std::sync::Condvar)) {
        let (lock, ready) = gate;
        let mut released = lock.lock().unwrap();
        while !*released {
            released = ready.wait(released).unwrap();
        }
    }

    fn before_unwrap(&self) {
        let _ = self.before_unwrap_entered.send(());
        Self::wait(&self.before_unwrap_release);
    }

    fn after_unwrap(&self, proven: bool) {
        if self
            .unwrap_notified
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
        {
            let _ = self.unwrap_attempted.send(proven);
        }
    }

    fn after_late_upgrade(&self) {
        let _ = self.late_upgrade_entered.send(());
        Self::wait(&self.late_upgrade_release);
    }
}

#[cfg(test)]
struct TestAdmissionHook {
    entered: std::sync::mpsc::SyncSender<()>,
    release: Arc<(std::sync::Mutex<bool>, std::sync::Condvar)>,
    armed: AtomicBool,
}

#[cfg(test)]
impl TestAdmissionHook {
    fn after_first_checks(&self) {
        if self.armed.swap(false, Ordering::SeqCst) {
            let _ = self.entered.send(());
            TestProofHook::wait(&self.release);
        }
    }
}

/// Stable stage at which direct system-output setup failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemOutputOperation {
    /// Acquire the process lease or create the native host.
    Setup,
    /// Find the default output device.
    Enumerate,
    /// Select an exact supported device configuration.
    Plan,
    /// Build the physical stream.
    Build,
    /// Start the physical stream.
    Play,
    /// Retire a native object or stream.
    Drop,
}

/// Stable, CPAL-independent category for a system-output failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemOutputErrorKind {
    /// No usable default output device or host is currently available.
    DeviceUnavailable,
    /// The process output is already leased or the default device is busy.
    OutputInUse,
    /// The default device has no exact supported mixer format.
    UnsupportedFormat,
    /// The platform output backend failed.
    BackendFailure,
    /// The backend violated the bounded callback or ownership protocol.
    Protocol,
}

/// Stable CPAL-free system-output error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SystemOutputError {
    kind: SystemOutputErrorKind,
    operation: SystemOutputOperation,
    detail: String,
}

impl SystemOutputError {
    fn new(
        kind: SystemOutputErrorKind,
        operation: SystemOutputOperation,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            operation,
            detail: detail.into(),
        }
    }

    /// Stable failure category.
    #[must_use]
    pub const fn kind(&self) -> SystemOutputErrorKind {
        self.kind
    }

    /// Stable operation that failed.
    #[must_use]
    pub const fn operation(&self) -> SystemOutputOperation {
        self.operation
    }

    /// Human-readable platform detail with no stable parsing contract.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

impl fmt::Display for SystemOutputError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "system output {:?} failed ({:?}): {}",
            self.operation, self.kind, self.detail
        )
    }
}

impl std::error::Error for SystemOutputError {}

/// Failure returned while starting [`SystemMixerService`].
pub type SystemMixerStartError = MixerStartError<SystemOutputError>;

/// Exact, cloneable cause for a physical mixer that was unavailable during
/// application startup.
///
/// This value is diagnostic only. In particular,
/// [`MixerStartError::CleanupUncertain`] preserves the fact that the failed
/// physical startup did not prove cleanup; it must never be interpreted as a
/// successful physical shutdown or retried in-process.
#[derive(Debug, Clone)]
pub struct SystemMixerUnavailable(Arc<SystemMixerStartError>);

impl SystemMixerUnavailable {
    /// Returns the exact failed startup result, including platform source and
    /// any cleanup-uncertain primary cause.
    #[must_use]
    pub fn error(&self) -> &SystemMixerStartError {
        &self.0
    }

    /// Whether the failed startup reported proven physical cleanup.
    #[must_use]
    pub fn cleanup_proven(&self) -> bool {
        !matches!(
            &*self.0,
            MixerStartError::CleanupUncertain(_) | MixerStartError::OwnerStopped
        )
    }
}

impl fmt::Display for SystemMixerUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

impl std::error::Error for SystemMixerUnavailable {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error())
    }
}

impl From<SystemMixerStartError> for SystemMixerUnavailable {
    fn from(error: SystemMixerStartError) -> Self {
        Self(Arc::new(error))
    }
}

/// One non-generic process system-output service.
///
/// The value is thread-affine because the contained mixer service is
/// thread-affine. Only the default output device is considered.
/// [`shutdown`](Self::shutdown) is the proof-bearing joined path. Ordinary
/// `Drop` requests autonomous cleanup but cannot report or synchronously prove
/// its outcome; an uncertain cleanup deliberately retains the process lease.
pub struct SystemMixerService(MixerService);

impl fmt::Debug for SystemMixerService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SystemMixerService")
            .field("format", &self.format())
            .finish_non_exhaustive()
    }
}

impl SystemMixerService {
    /// Start the sole process mixer on the default physical output device.
    ///
    /// Startup verifies the device and its format but does not open a native
    /// stream. The stream is built on the first audible frame and released
    /// again after continuous silence, so an idle client holds no audio
    /// endpoint and asserts no power request.
    ///
    /// # Errors
    ///
    /// Returns a stable mixer or system-output startup failure. An uncertain
    /// prior retirement deliberately retains the process lease. An active
    /// service, or an ordinarily dropped service still retiring on its owner,
    /// returns [`SystemOutputErrorKind::OutputInUse`].
    pub fn start(sample_rate: u32) -> Result<Self, SystemMixerStartError> {
        let lease =
            OutputLease::acquire(&SYSTEM_OUTPUT_LEASED).map_err(MixerStartError::Backend)?;
        MixerService::start_with_driver::<CpalOutputDriver<SystemHostFactory>>(
            SystemDriverSettings {
                factory: SystemHostFactory,
                lease,
                sample_rate,
                idle: OutputIdlePolicy::DetachWhenIdle {
                    after: OUTPUT_IDLE_DETACH_AFTER,
                },
                #[cfg(test)]
                proof_hook: None,
            },
            sample_rate,
        )
        .map(Self)
    }

    /// Verified logical mixer format.
    #[must_use]
    pub fn format(&self) -> MixerFormat {
        self.0.format()
    }

    /// Takes the unique stream of terminal process-output failures.
    ///
    /// Recoverable endpoint interruptions remain internal to the driver.
    pub fn take_output_failure_events(&mut self) -> Option<crate::MixerOutputFailureReceiver> {
        self.0.take_output_failure_events()
    }

    /// Returns a weak, cloneable session-registration authority.
    ///
    /// The registrar never owns this thread-affine service or its unique
    /// physical-output join authority.
    #[must_use]
    pub fn session_registrar(&self) -> MixerSessionRegistrar {
        self.0.session_registrar()
    }

    /// Returns a weak, cloneable authority over the application main track.
    ///
    /// The authority never retains this thread-affine service or its unique
    /// physical-output join capability.
    #[must_use]
    pub fn master_gain_authority(&self) -> crate::MixerMasterGainAuthority {
        self.0.master_gain_authority()
    }

    /// Add one fully preinstalled session subtree.
    ///
    /// # Errors
    ///
    /// Returns a bounded control or topology error.
    pub fn add_session(&self, id: AudioSessionId) -> Result<MixerSessionOwner, MixerControlError> {
        self.0.add_session(id)
    }

    /// Seal production and join the system-output owner.
    #[must_use]
    pub fn shutdown(self) -> MixerShutdown {
        self.0.shutdown()
    }
}

struct OutputLease {
    flag: &'static AtomicBool,
}

impl OutputLease {
    fn acquire(flag: &'static AtomicBool) -> Result<Self, SystemOutputError> {
        flag.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                SystemOutputError::new(
                    SystemOutputErrorKind::OutputInUse,
                    SystemOutputOperation::Setup,
                    "the process system output is already leased",
                )
            })?;
        Ok(Self { flag })
    }
}

impl Drop for OutputLease {
    fn drop(&mut self) {
        self.flag.store(false, Ordering::Release);
    }
}

#[derive(Debug, Clone)]
struct HostFailure {
    kind: SystemOutputErrorKind,
    detail: String,
}

impl HostFailure {
    fn new(kind: SystemOutputErrorKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: detail.into(),
        }
    }
}

trait HostFactory: Clone + Send + 'static {
    type Host: OutputHost;

    fn create(self) -> Result<Self::Host, HostFailure>;
}

trait OutputHost {
    type Device: OutputDevice;

    fn default_output_device(&self) -> Result<Option<Self::Device>, HostFailure>;
}

trait OutputDevice: 'static {
    type Stream: OutputStream;

    fn supported_output_configs(&self) -> Result<Vec<OutputConfigRange>, HostFailure>;
    fn build_output_stream(
        &self,
        config: OutputStreamConfig,
        data: OutputDataCallback,
        error: OutputErrorCallback,
    ) -> Result<Self::Stream, HostFailure>;
}

trait OutputStream: 'static {
    fn play(&self) -> Result<(), HostFailure>;
}

type OutputDataCallback = Box<dyn for<'a> FnMut(Option<OutputBuffer<'a>>) + Send + 'static>;
type OutputErrorCallback = Box<dyn FnMut(cpal::Error) + Send + 'static>;

struct RuntimeErrorQueue {
    consumer: Consumer<cpal::Error>,
    overflow: Arc<AtomicU64>,
}

fn runtime_error_channel(
    state: &Arc<CallbackState>,
    generation: u64,
) -> (OutputErrorCallback, RuntimeErrorQueue) {
    let (mut producer, consumer) = RingBuffer::new(RUNTIME_ERROR_QUEUE_CAPACITY);
    let overflow = Arc::new(AtomicU64::new(0));
    let callback_overflow = Arc::clone(&overflow);
    let state = Arc::downgrade(state);
    let callback = Box::new(move |error: cpal::Error| {
        let Some(state) = state.upgrade() else {
            std::mem::forget(error);
            return;
        };
        let event = runtime_event_from_cpal(error.kind());
        let Some(_active) = state.enter_error_callback(generation, event) else {
            std::mem::forget(error);
            return;
        };
        let accepted = state.report_runtime_event(event);
        if !accepted {
            // No live owner can safely destroy this possibly allocated
            // platform detail. Retain a notification from a stale/revoked
            // callback rather than enqueueing it after consumer retirement.
            std::mem::forget(error);
            return;
        }
        if let Err(PushError::Full(error)) = producer.push(error) {
            let previous = callback_overflow.fetch_add(1, Ordering::Relaxed);
            if previous == 0 {
                let _ = state.report_runtime_event(DriverRuntimeEvent::BackendError);
            }
            // Destroying an owned backend string can allocate or take a
            // platform lock. Queue saturation is exceptional, so explicitly
            // retain only the overflowed value rather than doing that work on
            // the native callback thread.
            std::mem::forget(error);
        }
    });
    (callback, RuntimeErrorQueue { consumer, overflow })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriverSampleFormat {
    F32,
    I16,
    U16,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputBufferRange {
    Range { min: usize, max: usize },
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputConfigRange {
    channels: usize,
    min_sample_rate: u32,
    max_sample_rate: u32,
    sample_format: DriverSampleFormat,
    buffer_size: OutputBufferRange,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputBufferRequest {
    Default,
    Fixed(usize),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputStreamConfig {
    channels: usize,
    sample_rate: u32,
    sample_format: DriverSampleFormat,
    buffer_size: OutputBufferRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OutputPlan {
    stream: OutputStreamConfig,
    physical: PhysicalOutputFormat,
}

fn plan_output(
    ranges: impl IntoIterator<Item = OutputConfigRange>,
    sample_rate: u32,
) -> Result<OutputPlan, HostFailure> {
    let mut selected: Option<(u8, OutputPlan)> = None;
    for range in ranges {
        if range.channels != PHYSICAL_CHANNELS
            || sample_rate < range.min_sample_rate
            || sample_rate > range.max_sample_rate
        {
            continue;
        }
        let (rank, physical_sample_format) = match range.sample_format {
            DriverSampleFormat::F32 => (0, PhysicalSampleFormat::F32),
            DriverSampleFormat::I16 => (1, PhysicalSampleFormat::I16),
            DriverSampleFormat::U16 => (2, PhysicalSampleFormat::U16),
            DriverSampleFormat::Other => continue,
        };
        let (buffer_size, buffer_frames_hint) = match range.buffer_size {
            OutputBufferRange::Unknown => (OutputBufferRequest::Default, None),
            OutputBufferRange::Range { min, max }
                if min > 0 && min <= max && min <= MAX_PHYSICAL_CALLBACK_FRAMES =>
            {
                let frames = super::INTERNAL_BUFFER_FRAMES.clamp(min, max);
                (OutputBufferRequest::Fixed(frames), Some(frames))
            }
            OutputBufferRange::Range { .. } => continue,
        };
        let plan = OutputPlan {
            stream: OutputStreamConfig {
                channels: PHYSICAL_CHANNELS,
                sample_rate,
                sample_format: range.sample_format,
                buffer_size,
            },
            physical: PhysicalOutputFormat {
                sample_rate,
                channels: PHYSICAL_CHANNELS,
                sample_format: physical_sample_format,
                buffer_frames_hint,
            },
        };
        if selected.as_ref().is_none_or(|(best, _)| rank < *best) {
            selected = Some((rank, plan));
        }
    }
    selected.map(|(_, plan)| plan).ok_or_else(|| {
        HostFailure::new(
            SystemOutputErrorKind::UnsupportedFormat,
            "the default device has no exact stereo PCM configuration at the requested rate",
        )
    })
}

enum OutputBuffer<'a> {
    F32(&'a mut [f32]),
    I16(&'a mut [i16]),
    U16(&'a mut [u16]),
}

impl OutputBuffer<'_> {
    fn silence(&mut self) {
        match self {
            Self::F32(output) => output.fill(0.0),
            Self::I16(output) => output.fill(0),
            Self::U16(output) => output.fill(1 << 15),
        }
    }

    fn len(&self) -> usize {
        match self {
            Self::F32(output) => output.len(),
            Self::I16(output) => output.len(),
            Self::U16(output) => output.len(),
        }
    }

    fn sample_format(&self) -> DriverSampleFormat {
        match self {
            Self::F32(_) => DriverSampleFormat::F32,
            Self::I16(_) => DriverSampleFormat::I16,
            Self::U16(_) => DriverSampleFormat::U16,
        }
    }

    fn copy_from_f32(&mut self, source: &[f32]) {
        match self {
            Self::F32(output) => output.copy_from_slice(source),
            Self::I16(output) => {
                for (output, sample) in output.iter_mut().zip(source.iter().copied()) {
                    *output = cpal::Sample::from_sample(sample);
                }
            }
            Self::U16(output) => {
                for (output, sample) in output.iter_mut().zip(source.iter().copied()) {
                    *output = cpal::Sample::from_sample(sample);
                }
            }
        }
    }
}

/// Owner-rendered audio captured while a demanded stream was being attached.
///
/// The first native callbacks of the new stream drain it ahead of live
/// rendering, so waking from idle loses no onset. Access follows the callback
/// scratch rules: sole entry through the `active` claim.
struct Primer {
    samples: Box<[f32]>,
    len: usize,
    pos: usize,
}

impl Primer {
    fn new() -> Self {
        Self {
            samples: vec![0.0; PHYSICAL_SCRATCH_SAMPLES].into_boxed_slice(),
            len: 0,
            pos: 0,
        }
    }

    fn stash(&mut self, rendered: &[f32]) {
        let len = rendered.len().min(self.samples.len());
        self.samples[..len].copy_from_slice(&rendered[..len]);
        self.len = len;
        self.pos = 0;
    }

    fn clear(&mut self) {
        self.len = 0;
        self.pos = 0;
    }

    /// Copies whole pending frames into the front of `output`; returns the
    /// sample count written.
    fn drain_into(&mut self, output: &mut [f32]) -> usize {
        let mut count = (self.len - self.pos).min(output.len());
        count -= count % PHYSICAL_CHANNELS;
        output[..count].copy_from_slice(&self.samples[self.pos..self.pos + count]);
        self.pos += count;
        count
    }
}

fn is_audible(samples: &[f32]) -> bool {
    samples
        .iter()
        .any(|sample| sample.abs() > AUDIBLE_SAMPLE_THRESHOLD)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OwnerRender {
    Silent,
    Audible,
}

struct CallbackState {
    // Admission proof: every gate check/claim/release and every owner
    // pause/revoke/generation/idle transition is SeqCst. `error_admission`
    // packs endpoint phase and the in-flight error-callback count into one
    // atomic word. Recovery activation can therefore change a clean,
    // callback-free provisional generation to Active in the same total order
    // in which an error callback either claims that generation (and marks it
    // Failed) or observes it as already Active. Data entry requires Active, so
    // no provisional callback can render across that handoff.
    revoked: AtomicBool,
    paused: AtomicBool,
    error_callbacks_paused: AtomicBool,
    active: AtomicUsize,
    error_admission: AtomicUsize,
    generation: AtomicU64,
    pending_recovery: AtomicU64,
    /// Set by any render that produced a sample above the audible threshold;
    /// consumed by the owner's idle accounting.
    audible: AtomicBool,
    renderer: UnsafeCell<Option<JoinedRenderer>>,
    scratch: UnsafeCell<Box<[f32]>>,
    primer: UnsafeCell<Primer>,
    failures: DriverFailureSignal,
    #[cfg(test)]
    proof_hook: Option<Arc<TestProofHook>>,
    #[cfg(test)]
    admission_hook: Option<Arc<TestAdmissionHook>>,
}

// SAFETY: callback entry is exclusive via `active`. The owner accesses the
// cells only before publishing a stream or after revocation, stream destruction,
// zero active callbacks, and unique strong ownership.
unsafe impl Sync for CallbackState {}

impl CallbackState {
    fn new(
        failures: DriverFailureSignal,
        #[cfg(test)] proof_hook: Option<Arc<TestProofHook>>,
        #[cfg(test)] admission_hook: Option<Arc<TestAdmissionHook>>,
    ) -> Self {
        Self {
            revoked: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            error_callbacks_paused: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            error_admission: AtomicUsize::new(EndpointPhase::Active as usize),
            generation: AtomicU64::new(1),
            pending_recovery: AtomicU64::new(0),
            audible: AtomicBool::new(false),
            renderer: UnsafeCell::new(None),
            scratch: UnsafeCell::new(vec![0.0; PHYSICAL_SCRATCH_SAMPLES].into_boxed_slice()),
            primer: UnsafeCell::new(Primer::new()),
            failures,
            #[cfg(test)]
            proof_hook,
            #[cfg(test)]
            admission_hook,
        }
    }

    fn install_renderer(&self, renderer: JoinedRenderer) -> bool {
        if self.revoked.load(Ordering::SeqCst) || self.active.load(Ordering::SeqCst) != 0 {
            return false;
        }
        // SAFETY: no stream exists yet, so the owner has exclusive access.
        let slot = unsafe { &mut *self.renderer.get() };
        if slot.is_some() {
            return false;
        }
        *slot = Some(renderer);
        true
    }

    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn enter(&self, generation: u64) -> Option<ActiveCallback<'_>> {
        if self.revoked.load(Ordering::SeqCst)
            || self.paused.load(Ordering::SeqCst)
            || self.generation.load(Ordering::SeqCst) != generation
            || EndpointPhase::from_admission(self.error_admission.load(Ordering::SeqCst))
                != Some(EndpointPhase::Active)
        {
            return None;
        }
        #[cfg(test)]
        if let Some(hook) = self.admission_hook.as_ref() {
            hook.after_first_checks();
        }
        if self
            .active
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            self.failures.report(MixerOutputFailure::BackendFailure);
            return None;
        }
        if self.revoked.load(Ordering::SeqCst)
            || self.paused.load(Ordering::SeqCst)
            || self.generation.load(Ordering::SeqCst) != generation
            || EndpointPhase::from_admission(self.error_admission.load(Ordering::SeqCst))
                != Some(EndpointPhase::Active)
        {
            self.active.store(0, Ordering::SeqCst);
            return None;
        }
        Some(ActiveCallback {
            active: &self.active,
        })
    }

    fn enter_error_callback(
        &self,
        generation: u64,
        event: DriverRuntimeEvent,
    ) -> Option<ActiveErrorCallback<'_>> {
        if self.revoked.load(Ordering::SeqCst)
            || self.error_callbacks_paused.load(Ordering::SeqCst)
            || self.generation.load(Ordering::SeqCst) != generation
        {
            return None;
        }
        #[cfg(test)]
        if let Some(hook) = self.admission_hook.as_ref() {
            hook.after_first_checks();
        }
        let mut admission = self.error_admission.load(Ordering::SeqCst);
        loop {
            let count = admission & !ERROR_ADMISSION_PHASE_MASK;
            let Some(count) = count.checked_add(ERROR_ADMISSION_COUNT_ONE) else {
                self.failures.report(MixerOutputFailure::BackendFailure);
                return None;
            };
            let phase = EndpointPhase::from_admission(admission)?;
            let next = count | phase as usize;
            match self.error_admission.compare_exchange(
                admission,
                next,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => break,
                Err(observed) => admission = observed,
            }
        }
        if self.revoked.load(Ordering::SeqCst)
            || self.error_callbacks_paused.load(Ordering::SeqCst)
            || self.generation.load(Ordering::SeqCst) != generation
        {
            self.error_admission
                .fetch_sub(ERROR_ADMISSION_COUNT_ONE, Ordering::SeqCst);
            return None;
        }
        if !event.is_advisory() {
            let mut admission = self.error_admission.load(Ordering::SeqCst);
            while EndpointPhase::from_admission(admission) == Some(EndpointPhase::Provisional) {
                let failed =
                    (admission & !ERROR_ADMISSION_PHASE_MASK) | EndpointPhase::Failed as usize;
                match self.error_admission.compare_exchange(
                    admission,
                    failed,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                ) {
                    Ok(_) => break,
                    Err(observed) => admission = observed,
                }
            }
        }
        Some(ActiveErrorCallback {
            admission: &self.error_admission,
        })
    }

    fn wait_for_error_callbacks(&self) {
        while self.error_admission.load(Ordering::SeqCst) & !ERROR_ADMISSION_PHASE_MASK != 0 {
            thread::sleep(CALLBACK_RETIREMENT_POLL);
        }
    }

    fn render_silenced(
        &self,
        output: &mut OutputBuffer<'_>,
        expected_sample_format: DriverSampleFormat,
    ) {
        let samples = output.len();
        if output.sample_format() != expected_sample_format
            || samples == 0
            || !samples.is_multiple_of(PHYSICAL_CHANNELS)
            || samples > PHYSICAL_SCRATCH_SAMPLES
        {
            self.failures
                .report(MixerOutputFailure::InvalidCallbackGeometry);
            return;
        }
        // SAFETY: the active-entry CAS provides sole callback access.
        let renderer = unsafe { &mut *self.renderer.get() };
        let Some(renderer) = renderer.as_mut() else {
            self.failures.report(MixerOutputFailure::BackendFailure);
            return;
        };
        // SAFETY: the active-entry CAS provides sole callback access.
        let scratch = unsafe { &mut *self.scratch.get() };
        let scratch = &mut scratch[..samples];
        // SAFETY: the active-entry CAS provides sole callback access.
        let primer = unsafe { &mut *self.primer.get() };
        let primed_samples = primer.drain_into(scratch);
        let live = &mut scratch[primed_samples..];
        if live.is_empty() || renderer.render(live, PHYSICAL_CHANNELS).is_ok() {
            if is_audible(scratch) {
                self.audible.store(true, Ordering::SeqCst);
            }
            output.copy_from_f32(scratch);
        }
    }

    fn take_audible(&self) -> bool {
        self.audible.swap(false, Ordering::SeqCst)
    }

    fn revoke(&self) {
        self.paused.store(true, Ordering::SeqCst);
        self.error_callbacks_paused.store(true, Ordering::SeqCst);
        self.revoked.store(true, Ordering::SeqCst);
    }

    fn pause(&self) {
        self.paused.store(true, Ordering::SeqCst);
        self.error_callbacks_paused.store(true, Ordering::SeqCst);
    }

    fn begin_stream_generation(&self) -> Option<u64> {
        self.pause();
        if self.error_admission.load(Ordering::SeqCst) & !ERROR_ADMISSION_PHASE_MASK != 0 {
            return None;
        }
        let generation = self.generation().checked_add(1)?;
        self.error_admission
            .store(EndpointPhase::Provisional as usize, Ordering::SeqCst);
        self.generation.store(generation, Ordering::SeqCst);
        Some(generation)
    }

    fn activate_generation(&self, generation: u64) -> GenerationActivation {
        if self.revoked.load(Ordering::SeqCst)
            || self.active.load(Ordering::SeqCst) != 0
            || self.generation.load(Ordering::SeqCst) != generation
        {
            return GenerationActivation::Invalid;
        }
        self.paused.store(false, Ordering::SeqCst);
        #[cfg(test)]
        if let Some(hook) = self.admission_hook.as_ref() {
            hook.after_first_checks();
        }
        match self.error_admission.compare_exchange(
            EndpointPhase::Provisional as usize,
            EndpointPhase::Active as usize,
            Ordering::SeqCst,
            Ordering::SeqCst,
        ) {
            Ok(_) => GenerationActivation::Activated,
            Err(admission) => {
                self.paused.store(true, Ordering::SeqCst);
                match EndpointPhase::from_admission(admission) {
                    Some(EndpointPhase::Provisional) => GenerationActivation::Deferred,
                    Some(EndpointPhase::Failed) => GenerationActivation::RuntimeError,
                    Some(EndpointPhase::Active) | None => GenerationActivation::Invalid,
                }
            }
        }
    }

    fn activate_error_callbacks(&self, generation: u64) -> bool {
        if self.revoked.load(Ordering::SeqCst)
            || self.error_admission.load(Ordering::SeqCst) & !ERROR_ADMISSION_PHASE_MASK != 0
            || self.generation.load(Ordering::SeqCst) != generation
        {
            return false;
        }
        self.error_callbacks_paused.store(false, Ordering::SeqCst);
        true
    }

    fn pause_error_callbacks(&self) {
        self.error_callbacks_paused.store(true, Ordering::SeqCst);
    }

    fn report_runtime_event(&self, event: DriverRuntimeEvent) -> bool {
        if self.failures.callback_epoch().is_none() {
            return false;
        }
        if event.is_recoverable() {
            self.pending_recovery
                .fetch_or(event.bit(), Ordering::Release);
        } else if !event.is_advisory() {
            self.failures.report(MixerOutputFailure::BackendFailure);
        }
        true
    }

    fn take_recovery_event(&self) -> Option<DriverRuntimeEvent> {
        let pending = self.pending_recovery.swap(0, Ordering::AcqRel);
        DriverRuntimeEvent::ALL
            .into_iter()
            .find(|event| !event.is_advisory() && pending & event.bit() != 0)
    }

    fn callback_epoch(&self) -> u64 {
        self.failures.callback_epoch().unwrap_or(0)
    }

    fn render_recovery_silence(&self, frames: usize) -> bool {
        self.render_owner_side(frames, false).is_some()
    }

    /// Renders `frames` on the owner while no native callback may enter.
    ///
    /// With `keep_audible`, an audible result is stashed as the primer for the
    /// stream that this demand is about to attach. Returns `None` when the
    /// render could not run or failed.
    fn render_owner_side(&self, frames: usize, keep_audible: bool) -> Option<OwnerRender> {
        if frames == 0
            || frames > MAX_PHYSICAL_CALLBACK_FRAMES
            || self.revoked.load(Ordering::SeqCst)
            || !self.paused.load(Ordering::SeqCst)
            || self
                .active
                .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            return None;
        }
        let _active = ActiveCallback {
            active: &self.active,
        };
        // SAFETY: owner-side rendering starts only after any old stream has
        // joined, remains paused to native callbacks, and owns `active`.
        let renderer = unsafe { &mut *self.renderer.get() };
        let Some(renderer) = renderer.as_mut() else {
            self.failures.report(MixerOutputFailure::BackendFailure);
            return None;
        };
        // SAFETY: the owner has sole callback access.
        let scratch = unsafe { &mut *self.scratch.get() };
        let scratch = &mut scratch[..frames * PHYSICAL_CHANNELS];
        if renderer.render(scratch, PHYSICAL_CHANNELS).is_err() {
            return None;
        }
        if !is_audible(scratch) {
            return Some(OwnerRender::Silent);
        }
        if keep_audible {
            // SAFETY: the owner has sole callback access.
            let primer = unsafe { &mut *self.primer.get() };
            primer.stash(scratch);
        }
        Some(OwnerRender::Audible)
    }

    /// Discards any stashed primer. Returns `false` if a callback held entry.
    fn clear_primer(&self) -> bool {
        if self
            .active
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return false;
        }
        let _active = ActiveCallback {
            active: &self.active,
        };
        // SAFETY: the active claim provides sole callback access.
        let primer = unsafe { &mut *self.primer.get() };
        primer.clear();
        true
    }

    #[cfg(test)]
    fn pause_after_late_upgrade(&self) {
        if let Some(hook) = self.proof_hook.as_ref() {
            hook.after_late_upgrade();
        }
    }
}

struct ActiveCallback<'a> {
    active: &'a AtomicUsize,
}

struct ActiveErrorCallback<'a> {
    admission: &'a AtomicUsize,
}

impl Drop for ActiveErrorCallback<'_> {
    fn drop(&mut self) {
        self.admission
            .fetch_sub(ERROR_ADMISSION_COUNT_ONE, Ordering::SeqCst);
    }
}

impl Drop for ActiveCallback<'_> {
    fn drop(&mut self) {
        self.active.store(0, Ordering::SeqCst);
    }
}

fn callback_for(
    state: &Arc<CallbackState>,
    generation: u64,
    sample_format: DriverSampleFormat,
) -> OutputDataCallback {
    let state = Arc::downgrade(state);
    Box::new(move |output| {
        // Establish the correct raw-format equilibrium before any fallible
        // operation or state upgrade. Opaque formats have already been
        // prefilled by CPAL and have no typed buffer to change.
        let mut output = output;
        if let Some(output) = output.as_mut() {
            output.silence();
        }
        let Some(state) = state.upgrade() else {
            return;
        };
        #[cfg(test)]
        state.pause_after_late_upgrade();
        let Some(_active) = state.enter(generation) else {
            return;
        };
        let Some(mut output) = output else {
            state
                .failures
                .report(MixerOutputFailure::InvalidCallbackGeometry);
            return;
        };
        let rendered = catch_unwind(AssertUnwindSafe(|| {
            state.render_silenced(&mut output, sample_format);
        }));
        if let Err(payload) = rendered {
            output.silence();
            state.failures.report(MixerOutputFailure::RendererPanicked);
            std::mem::forget(payload);
        }
    })
}

fn runtime_event_from_cpal(kind: CpalErrorKind) -> DriverRuntimeEvent {
    match kind {
        CpalErrorKind::DeviceBusy => DriverRuntimeEvent::DeviceBusy,
        CpalErrorKind::DeviceChanged => DriverRuntimeEvent::DeviceChanged,
        CpalErrorKind::DeviceNotAvailable => DriverRuntimeEvent::DeviceNotAvailable,
        CpalErrorKind::HostUnavailable => DriverRuntimeEvent::HostUnavailable,
        CpalErrorKind::InvalidInput => DriverRuntimeEvent::InvalidInput,
        CpalErrorKind::PermissionDenied => DriverRuntimeEvent::PermissionDenied,
        CpalErrorKind::RealtimeDenied => DriverRuntimeEvent::RealtimeDenied,
        CpalErrorKind::ResourceExhausted => DriverRuntimeEvent::ResourceExhausted,
        CpalErrorKind::StreamInvalidated => DriverRuntimeEvent::StreamInvalidated,
        CpalErrorKind::UnsupportedConfig => DriverRuntimeEvent::UnsupportedConfig,
        CpalErrorKind::UnsupportedOperation => DriverRuntimeEvent::UnsupportedOperation,
        CpalErrorKind::Xrun => DriverRuntimeEvent::Xrun,
        CpalErrorKind::BackendError => DriverRuntimeEvent::BackendError,
        _ => DriverRuntimeEvent::Other,
    }
}

/// How the driver holds the native output stream while the mixer is silent.
///
/// An open stream keeps the endpoint's power request asserted even when every
/// frame it receives is zero. On Windows that blocks automatic sleep for as
/// long as the process lives, so production output is demand-driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutputIdlePolicy {
    /// Build and play the stream at start and hold it until shutdown.
    AlwaysOpen,
    /// Build the stream on the first audible frame and release it after
    /// `after` of continuous silence. Logical time keeps advancing through
    /// owner-side null rendering while no stream exists.
    DetachWhenIdle { after: Duration },
}

struct SystemDriverSettings<F> {
    factory: F,
    lease: OutputLease,
    sample_rate: u32,
    idle: OutputIdlePolicy,
    #[cfg(test)]
    proof_hook: Option<Arc<TestProofHook>>,
}

type HostDevice<F> = <<F as HostFactory>::Host as OutputHost>::Device;

struct CpalOutputDriver<F: HostFactory> {
    factory: F,
    sample_rate: u32,
    device: Option<ManuallyDrop<<F::Host as OutputHost>::Device>>,
    stream: Option<ManuallyDrop<<<F::Host as OutputHost>::Device as OutputDevice>::Stream>>,
    runtime_errors: Option<RuntimeErrorQueue>,
    plan: OutputPlan,
    callback: ManuallyDrop<Option<Arc<CallbackState>>>,
    retired_callback: ManuallyDrop<Option<CallbackState>>,
    lease: ManuallyDrop<OutputLease>,
    retired: bool,
    cleanup_uncertain: bool,
    recovery: RecoveryState,
    idle: OutputIdlePolicy,
    /// Last instant a render produced an audible sample while a stream was
    /// active; the demand-driven policy releases the stream once `now`
    /// exceeds this by its idle threshold.
    last_audible_at: Instant,
    output_generation: u64,
    logged_runtime_events: u64,
    last_callback_epoch: u64,
    last_callback_at: Instant,
    last_maintenance_at: Instant,
}

/// Endpoint lifecycle beyond a live stream. `Waiting` and `CatchingUp` are
/// failure recovery; `Idle` and `Waking` are demand-driven output.
enum RecoveryState {
    Active,
    /// No native stream exists by choice. The owner null-renders wall-clock
    /// time until a render turns audible, then attaches a stream.
    Idle {
        last_null_tick: Instant,
    },
    /// A demanded stream is built and playing but its data callback is not
    /// yet admitted. Logical time stays frozen until activation.
    Waking {
        generation: u64,
    },
    Waiting {
        reason: DriverRuntimeEvent,
        attempt: u32,
        next_attempt: Instant,
        backoff: Duration,
        started_at: Instant,
        last_null_tick: Instant,
    },
    CatchingUp {
        reason: DriverRuntimeEvent,
        attempt: u32,
        backoff: Duration,
        started_at: Instant,
        last_null_tick: Instant,
        generation: u64,
        format: PhysicalOutputFormat,
    },
}

fn system_error(operation: SystemOutputOperation, failure: HostFailure) -> SystemOutputError {
    SystemOutputError::new(failure.kind, operation, failure.detail)
}

/// Classifies an owner-side attach failure as the runtime event recovery
/// reports and retries under.
fn runtime_event_for(error: &SystemOutputError) -> DriverRuntimeEvent {
    match error.kind() {
        SystemOutputErrorKind::DeviceUnavailable => DriverRuntimeEvent::DeviceNotAvailable,
        SystemOutputErrorKind::OutputInUse => DriverRuntimeEvent::DeviceBusy,
        SystemOutputErrorKind::UnsupportedFormat => DriverRuntimeEvent::UnsupportedConfig,
        SystemOutputErrorKind::BackendFailure | SystemOutputErrorKind::Protocol => {
            DriverRuntimeEvent::BackendError
        }
    }
}

fn contain_host_call<T>(
    operation: SystemOutputOperation,
    call: impl FnOnce() -> Result<T, HostFailure>,
) -> Result<T, SystemOutputError> {
    match catch_unwind(AssertUnwindSafe(call)) {
        Ok(result) => result.map_err(|error| system_error(operation, error)),
        Err(payload) => {
            std::mem::forget(payload);
            Err(SystemOutputError::new(
                SystemOutputErrorKind::BackendFailure,
                operation,
                "the native audio backend unwound",
            ))
        }
    }
}

fn drop_owned<T>(
    owned: &mut Option<ManuallyDrop<T>>,
    detail: &'static str,
) -> Result<(), SystemOutputError> {
    let Some(mut value) = owned.take() else {
        return Ok(());
    };
    match catch_unwind(AssertUnwindSafe(|| unsafe {
        ManuallyDrop::drop(&mut value);
    })) {
        Ok(()) => Ok(()),
        Err(payload) => {
            std::mem::forget(payload);
            Err(SystemOutputError::new(
                SystemOutputErrorKind::BackendFailure,
                SystemOutputOperation::Drop,
                detail,
            ))
        }
    }
}

fn mark_cleanup_uncertain(failures: &DriverFailureSignal) {
    if let Some(status) = failures.0.upgrade() {
        status.finish_retirement(false);
    }
}

fn cleanup_failed_setup<H, D>(
    host: &mut Option<ManuallyDrop<H>>,
    device: &mut Option<ManuallyDrop<D>>,
    lease: &mut Option<ManuallyDrop<OutputLease>>,
    failures: &DriverFailureSignal,
    cause: SystemOutputError,
    mut uncertain: bool,
) -> SystemOutputError {
    for result in [
        drop_owned(device, "the native output device destructor unwound"),
        drop_owned(host, "the native audio host destructor unwound"),
    ] {
        if result.is_err() {
            uncertain = true;
        }
    }
    if uncertain {
        mark_cleanup_uncertain(failures);
    } else if let Err(error) = drop_owned(lease, "the process output lease destructor unwound") {
        mark_cleanup_uncertain(failures);
        return error;
    }
    cause
}

fn cleanup_failed_recovery<H, D>(
    host: &mut Option<ManuallyDrop<H>>,
    device: &mut Option<ManuallyDrop<D>>,
    cause: SystemOutputError,
) -> (SystemOutputError, bool) {
    let uncertain = [
        drop_owned(device, "the native output device destructor unwound"),
        drop_owned(host, "the native audio host destructor unwound"),
    ]
    .into_iter()
    .any(|result| result.is_err());
    (cause, uncertain)
}

fn stage_default_endpoint<F: HostFactory>(
    factory: F,
    sample_rate: u32,
    host: &mut Option<ManuallyDrop<F::Host>>,
    device: &mut Option<ManuallyDrop<HostDevice<F>>>,
) -> Result<OutputPlan, SystemOutputError> {
    *host = Some(ManuallyDrop::new(contain_host_call(
        SystemOutputOperation::Setup,
        || factory.create(),
    )?));
    let selected = contain_host_call(SystemOutputOperation::Enumerate, || {
        host.as_ref()
            .expect("host is staged during enumeration")
            .default_output_device()
    })?;
    *device = Some(ManuallyDrop::new(selected.ok_or_else(|| {
        SystemOutputError::new(
            SystemOutputErrorKind::DeviceUnavailable,
            SystemOutputOperation::Enumerate,
            "the native audio host has no default output device",
        )
    })?));
    let ranges = contain_host_call(SystemOutputOperation::Plan, || {
        device
            .as_ref()
            .expect("device is staged during planning")
            .supported_output_configs()
    })?;
    plan_output(ranges, sample_rate)
        .map_err(|error| system_error(SystemOutputOperation::Plan, error))
}

impl<F: HostFactory> JoinedOutputDriver for CpalOutputDriver<F> {
    type Settings = SystemDriverSettings<F>;
    type Error = SystemOutputError;

    const CALLBACK_STALL_POLICY: CallbackStallPolicy = CallbackStallPolicy::DriverManaged;

    fn setup(
        settings: Self::Settings,
        _internal_buffer_size: usize,
        failures: DriverFailureSignal,
    ) -> Result<(Self, PhysicalOutputFormat), Self::Error> {
        let SystemDriverSettings {
            factory,
            lease,
            sample_rate,
            idle,
            #[cfg(test)]
            proof_hook,
        } = settings;
        let retained_factory = factory.clone();
        let mut lease = Some(ManuallyDrop::new(lease));
        let mut host = None;
        let mut device = None;
        let plan = match stage_default_endpoint(factory, sample_rate, &mut host, &mut device) {
            Ok(plan) => plan,
            Err(cause) => {
                return Err(cleanup_failed_setup(
                    &mut host,
                    &mut device,
                    &mut lease,
                    &failures,
                    cause,
                    false,
                ));
            }
        };
        if let Err(cause) = drop_owned(&mut host, "the native audio host destructor unwound") {
            return Err(cleanup_failed_setup(
                &mut host,
                &mut device,
                &mut lease,
                &failures,
                cause,
                true,
            ));
        }
        let physical = plan.physical;
        let now = Instant::now();
        Ok((
            Self {
                factory: retained_factory,
                sample_rate,
                device,
                stream: None,
                runtime_errors: None,
                plan,
                callback: ManuallyDrop::new(Some(Arc::new(CallbackState::new(
                    failures,
                    #[cfg(test)]
                    proof_hook,
                    #[cfg(test)]
                    None,
                )))),
                retired_callback: ManuallyDrop::new(None),
                lease: lease
                    .take()
                    .expect("lease transfers into the published driver"),
                retired: false,
                cleanup_uncertain: false,
                recovery: RecoveryState::Active,
                idle,
                last_audible_at: now,
                output_generation: 1,
                logged_runtime_events: 0,
                last_callback_epoch: 0,
                last_callback_at: now,
                last_maintenance_at: now,
            },
            physical,
        ))
    }

    fn start(&mut self, renderer: JoinedRenderer) -> Result<(), Self::Error> {
        let callback = self.callback.as_ref().ok_or_else(|| {
            SystemOutputError::new(
                SystemOutputErrorKind::Protocol,
                SystemOutputOperation::Build,
                "the physical callback state was already retired",
            )
        })?;
        if !callback.install_renderer(renderer) {
            return Err(SystemOutputError::new(
                SystemOutputErrorKind::Protocol,
                SystemOutputOperation::Build,
                "the physical callback renderer was already installed or revoked",
            ));
        }
        if matches!(self.idle, OutputIdlePolicy::DetachWhenIdle { .. }) {
            // Demand-driven output builds no stream until the first audible
            // frame. Pausing the callback state admits owner-side null
            // rendering, which is what advances logical time until then.
            callback.pause();
            self.drop_device()?;
            self.recovery = RecoveryState::Idle {
                last_null_tick: Instant::now(),
            };
            return Ok(());
        }
        let device = self.device.as_ref().ok_or_else(|| {
            SystemOutputError::new(
                SystemOutputErrorKind::Protocol,
                SystemOutputOperation::Build,
                "the output device was already retired",
            )
        })?;
        let (error_callback, runtime_errors) =
            runtime_error_channel(callback, self.output_generation);
        let stream = contain_host_call(SystemOutputOperation::Build, || {
            device.build_output_stream(
                self.plan.stream,
                callback_for(
                    callback,
                    self.output_generation,
                    self.plan.stream.sample_format,
                ),
                error_callback,
            )
        })?;
        self.stream = Some(ManuallyDrop::new(stream));
        self.runtime_errors = Some(runtime_errors);
        self.drop_device()?;
        Ok(())
    }

    fn play(&mut self) -> Result<(), Self::Error> {
        if matches!(self.recovery, RecoveryState::Idle { .. }) {
            // Demand-driven output plays once the first audible frame
            // attaches a stream. Maintenance replaces `recovery` with
            // `Active` before it reopens, so a wake's own play is not
            // short-circuited here.
            return Ok(());
        }
        let stream = self.stream.as_ref().ok_or_else(|| {
            SystemOutputError::new(
                SystemOutputErrorKind::Protocol,
                SystemOutputOperation::Play,
                "the output stream was not built",
            )
        })?;
        contain_host_call(SystemOutputOperation::Play, || stream.play())
    }

    fn maintain(&mut self, now: Instant) -> DriverMaintenance {
        self.maintain_recovery(now)
    }

    fn close_and_join(&mut self) -> bool {
        let Some(callback) = self.callback.as_ref() else {
            return false;
        };
        callback.revoke();
        if self.stream.is_some() != self.runtime_errors.is_some() {
            self.cleanup_uncertain = true;
            return false;
        }
        if let Some(mut stream) = self.stream.take() {
            let dropped = catch_unwind(AssertUnwindSafe(|| unsafe {
                ManuallyDrop::drop(&mut stream);
            }));
            if let Err(payload) = dropped {
                std::mem::forget(payload);
                self.callback
                    .as_ref()
                    .expect("callback remains owned after stream drop panic")
                    .failures
                    .report(MixerOutputFailure::BackendFailure);
                return false;
            }
        }
        self.callback
            .as_ref()
            .expect("callback remains owned until retirement proof")
            .wait_for_error_callbacks();
        self.finish_runtime_errors();
        if self.drop_device().is_err() || self.cleanup_uncertain {
            self.callback
                .as_ref()
                .expect("callback remains owned after device drop panic")
                .failures
                .report(MixerOutputFailure::BackendFailure);
            return false;
        }
        #[cfg(test)]
        if let Some(hook) = self
            .callback
            .as_ref()
            .expect("callback remains owned until atomic unwrap")
            .proof_hook
            .as_ref()
        {
            hook.before_unwrap();
        }
        let callback = self
            .callback
            .take()
            .expect("callback state is present until retirement proof");
        let callback = {
            let mut callback = callback;
            loop {
                match Arc::try_unwrap(callback) {
                    Ok(callback) => break callback,
                    Err(returned) => {
                        #[cfg(test)]
                        if let Some(hook) = returned.proof_hook.as_ref() {
                            hook.after_unwrap(false);
                        }
                        callback = returned;
                        // Successful stream destruction prevents new native
                        // callback entry. A preexisting strong callback owner
                        // is proof-bearing work, not a wall-clock failure, so
                        // shutdown waits without spinning or inventing a
                        // timeout-based quarantine.
                        thread::sleep(CALLBACK_RETIREMENT_POLL);
                    }
                }
            }
        };
        #[cfg(test)]
        if let Some(hook) = callback.proof_hook.as_ref() {
            hook.after_unwrap(true);
        }
        *self.retired_callback = Some(callback);
        self.retired = true;
        true
    }
}

impl<F: HostFactory> CpalOutputDriver<F> {
    fn log_runtime_event(&mut self, event: DriverRuntimeEvent, detail: Option<&cpal::Error>) {
        if self.logged_runtime_events & event.bit() != 0 {
            return;
        }
        self.logged_runtime_events |= event.bit();
        let detail = detail.map_or_else(
            || format!("{event:?}"),
            |error| format!("{event:?}: {error}"),
        );
        match event {
            DriverRuntimeEvent::DeviceChanged => {
                log::info!(
                    "physical audio route changed ({detail}); the active stream remains usable"
                );
            }
            DriverRuntimeEvent::RealtimeDenied => {
                log::warn!(
                    "physical audio was denied real-time scheduling ({detail}); playback remains active"
                );
            }
            DriverRuntimeEvent::Xrun => {
                log::warn!(
                    "physical audio reported an underrun or overrun ({detail}); playback continues"
                );
            }
            DriverRuntimeEvent::CallbackStalled => {
                log::warn!("physical audio callbacks stalled; rebuilding the endpoint");
            }
            event if event.is_recoverable() => {
                log::warn!("physical audio driver requested endpoint recovery: {detail}");
            }
            _ => log::error!("physical audio driver reported a terminal event: {detail}"),
        }
    }

    fn drain_error_queue(&mut self, queue: &mut RuntimeErrorQueue) {
        while let Ok(error) = queue.consumer.pop() {
            let event = runtime_event_from_cpal(error.kind());
            self.log_runtime_event(event, Some(&error));
            // The owned platform detail is destroyed here on the mixer
            // owner, never in CPAL's native error callback.
            drop(error);
        }
        let overflow = queue.overflow.swap(0, Ordering::AcqRel);
        if overflow != 0 {
            log::error!(
                "physical audio runtime-error queue overflowed; retained {overflow} owned errors to preserve callback safety"
            );
        }
    }

    fn drain_runtime_errors(&mut self) {
        if let Some(mut queue) = self.runtime_errors.take() {
            self.drain_error_queue(&mut queue);
            self.runtime_errors = Some(queue);
        }
    }

    fn finish_runtime_errors(&mut self) {
        self.drain_runtime_errors();
        self.runtime_errors = None;
    }

    fn reset_runtime_event_log(&mut self) {
        self.logged_runtime_events = 0;
    }

    fn recovery_failure(
        &mut self,
        host: &mut Option<ManuallyDrop<F::Host>>,
        device: &mut Option<ManuallyDrop<<F::Host as OutputHost>::Device>>,
        cause: SystemOutputError,
    ) -> SystemOutputError {
        let (cause, uncertain) = cleanup_failed_recovery(host, device, cause);
        self.cleanup_uncertain |= uncertain;
        cause
    }

    fn stage_recovery_device(
        &mut self,
    ) -> Result<(ManuallyDrop<HostDevice<F>>, OutputPlan), SystemOutputError> {
        let mut host = None;
        let mut device = None;
        let plan = match stage_default_endpoint(
            self.factory.clone(),
            self.sample_rate,
            &mut host,
            &mut device,
        ) {
            Ok(plan) => plan,
            Err(cause) => return Err(self.recovery_failure(&mut host, &mut device, cause)),
        };
        if let Err(cause) = drop_owned(&mut host, "the native audio host destructor unwound") {
            self.cleanup_uncertain = true;
            return Err(self.recovery_failure(&mut host, &mut device, cause));
        }
        Ok((
            device
                .take()
                .expect("recovery device remains staged after planning"),
            plan,
        ))
    }

    fn retire_stream_for_recovery(&mut self) -> bool {
        let Some(callback) = self.callback.as_ref() else {
            self.cleanup_uncertain = true;
            return false;
        };
        if self.stream.is_none() || self.stream.is_some() != self.runtime_errors.is_some() {
            self.cleanup_uncertain = true;
            return false;
        }
        callback.pause();
        if let Some(mut stream) = self.stream.take() {
            let dropped = catch_unwind(AssertUnwindSafe(|| unsafe {
                ManuallyDrop::drop(&mut stream);
            }));
            if let Err(payload) = dropped {
                std::mem::forget(payload);
                self.cleanup_uncertain = true;
                return false;
            }
        }
        self.callback
            .as_ref()
            .expect("callback remains owned during recovery")
            .wait_for_error_callbacks();
        let generation_advanced = self
            .callback
            .as_ref()
            .expect("callback remains owned during recovery")
            .begin_stream_generation()
            .is_some();
        self.finish_runtime_errors();
        if !generation_advanced {
            self.callback
                .as_ref()
                .expect("callback remains owned during recovery")
                .failures
                .report(MixerOutputFailure::BackendFailure);
            return false;
        }
        if self
            .callback
            .as_ref()
            .is_none_or(|callback| callback.active.load(Ordering::SeqCst) != 0)
        {
            self.cleanup_uncertain = true;
            return false;
        }
        true
    }

    fn attempt_reopen(
        &mut self,
        last_null_tick: &mut Instant,
        null_frames_remaining: &mut usize,
    ) -> Result<(PhysicalOutputFormat, u64), SystemOutputError> {
        let callback = Arc::clone(self.callback.as_ref().ok_or_else(|| {
            SystemOutputError::new(
                SystemOutputErrorKind::Protocol,
                SystemOutputOperation::Build,
                "the recovery callback state was already retired",
            )
        })?);
        let generation = callback.begin_stream_generation().ok_or_else(|| {
            SystemOutputError::new(
                SystemOutputErrorKind::Protocol,
                SystemOutputOperation::Build,
                "the physical stream generation was exhausted",
            )
        })?;
        let staged = self.stage_recovery_device();
        *last_null_tick =
            self.render_recovery_time(*last_null_tick, Instant::now(), null_frames_remaining);
        let (device, plan) = staged?;
        let mut device = Some(device);
        if !callback.activate_error_callbacks(generation) {
            let cause = SystemOutputError::new(
                SystemOutputErrorKind::Protocol,
                SystemOutputOperation::Build,
                "the recovered physical error callback could not be activated",
            );
            let mut host = None;
            let cause = self.recovery_failure(&mut host, &mut device, cause);
            *last_null_tick =
                self.render_recovery_time(*last_null_tick, Instant::now(), null_frames_remaining);
            return Err(cause);
        }
        let (error_callback, mut runtime_errors) = runtime_error_channel(&callback, generation);
        let stream = contain_host_call(SystemOutputOperation::Build, || {
            device
                .as_ref()
                .expect("recovery device is locally staged during build")
                .build_output_stream(
                    plan.stream,
                    callback_for(&callback, generation, plan.stream.sample_format),
                    error_callback,
                )
        });
        *last_null_tick =
            self.render_recovery_time(*last_null_tick, Instant::now(), null_frames_remaining);
        let stream = match stream {
            Ok(stream) => stream,
            Err(cause) => {
                callback.pause_error_callbacks();
                callback.wait_for_error_callbacks();
                self.drain_error_queue(&mut runtime_errors);
                drop(runtime_errors);
                let mut host = None;
                let cause = self.recovery_failure(&mut host, &mut device, cause);
                *last_null_tick = self.render_recovery_time(
                    *last_null_tick,
                    Instant::now(),
                    null_frames_remaining,
                );
                return Err(cause);
            }
        };
        self.stream = Some(ManuallyDrop::new(stream));
        self.runtime_errors = Some(runtime_errors);
        let device_dropped = drop_owned(
            &mut device,
            "the recovered native output device destructor unwound",
        );
        if device_dropped.is_err() {
            self.cleanup_uncertain = true;
        }
        *last_null_tick =
            self.render_recovery_time(*last_null_tick, Instant::now(), null_frames_remaining);
        device_dropped?;
        let played = self.play();
        *last_null_tick =
            self.render_recovery_time(*last_null_tick, Instant::now(), null_frames_remaining);
        if let Err(cause) = played {
            let _ = self.retire_stream_for_recovery();
            *last_null_tick =
                self.render_recovery_time(*last_null_tick, Instant::now(), null_frames_remaining);
            return Err(cause);
        }
        self.plan = plan;
        self.output_generation = generation;
        // The native stream is playing, but its data callback remains paused
        // until owner-side null rendering catches logical time up to the
        // physical endpoint. Error callbacks stay live while that happens.
        Ok((plan.physical, generation))
    }

    fn recovery_null_frame_budget(&self) -> usize {
        usize::try_from(
            ((RECOVERY_MAX_NULL_ADVANCE.as_nanos() * u128::from(self.sample_rate)) / 1_000_000_000)
                .min(MAX_PHYSICAL_CALLBACK_FRAMES as u128),
        )
        .expect("recovery frame budget is bounded by the maximum callback size")
        .max(1)
    }

    fn recovery_frames_between(&self, from: Instant, now: Instant) -> usize {
        ((now.saturating_duration_since(from).as_nanos() * u128::from(self.sample_rate))
            / 1_000_000_000)
            .min(usize::MAX as u128) as usize
    }

    fn render_recovery_time(
        &self,
        from: Instant,
        now: Instant,
        frames_remaining: &mut usize,
    ) -> Instant {
        let Some(callback) = self.callback.as_ref() else {
            return from;
        };
        let frames = self
            .recovery_frames_between(from, now)
            .min(*frames_remaining)
            .min(MAX_PHYSICAL_CALLBACK_FRAMES);
        if frames == 0 || !callback.render_recovery_silence(frames) {
            return from;
        }
        *frames_remaining -= frames;
        let rendered_nanos =
            u64::try_from((frames as u128 * 1_000_000_000) / u128::from(self.sample_rate))
                .expect("bounded callback frames fit a u64 nanosecond duration");
        from + Duration::from_nanos(rendered_nanos)
    }

    fn schedule_recovery_wait(
        &mut self,
        reason: DriverRuntimeEvent,
        attempt: u32,
        backoff: Duration,
        started_at: Instant,
        last_null_tick: Instant,
        detail: impl fmt::Display,
    ) {
        let now = Instant::now();
        let elapsed = now.saturating_duration_since(started_at);
        if recovery_attempt_is_warning(attempt) {
            log::warn!(
                "physical audio recovery attempt {attempt} failed after {elapsed:?}: {detail}; retrying in {backoff:?}"
            );
        } else {
            log::debug!(
                "physical audio recovery attempt {attempt} failed after {elapsed:?}: {detail}; retrying in {backoff:?}"
            );
        }
        self.recovery = RecoveryState::Waiting {
            reason,
            attempt,
            next_attempt: now + backoff,
            backoff: backoff.saturating_mul(2).min(RECOVERY_MAX_BACKOFF),
            started_at,
            last_null_tick,
        };
    }

    fn detect_callback_stall(&mut self, now: Instant) {
        let Some(callback) = self.callback.as_ref() else {
            return;
        };
        let epoch = callback.callback_epoch();
        let owner_resumed =
            now.saturating_duration_since(self.last_maintenance_at) >= OWNER_RESUME_GAP;
        self.last_maintenance_at = now;
        if owner_resumed || epoch != self.last_callback_epoch {
            self.last_callback_epoch = epoch;
            self.last_callback_at = now;
            return;
        }
        if now.saturating_duration_since(self.last_callback_at) >= CALLBACK_STALL_TIMEOUT {
            self.log_runtime_event(DriverRuntimeEvent::CallbackStalled, None);
            if let Some(callback) = self.callback.as_ref() {
                let _ = callback.report_runtime_event(DriverRuntimeEvent::CallbackStalled);
            }
            // The queued event is consumed below. Move the observation point
            // as well so an unexpectedly delayed maintenance pass cannot
            // enqueue an immediate duplicate.
            self.last_callback_at = now;
        }
    }

    fn note_recovered(&mut self, now: Instant) {
        self.last_maintenance_at = now;
        self.last_callback_at = now;
        self.last_callback_epoch = self
            .callback
            .as_ref()
            .map_or(0, |callback| callback.callback_epoch());
    }

    fn rebase_recovery_cursor_after_owner_resume(&mut self, now: Instant) {
        let last_null_tick = match &mut self.recovery {
            RecoveryState::Waiting { last_null_tick, .. }
            | RecoveryState::CatchingUp { last_null_tick, .. }
            | RecoveryState::Idle { last_null_tick } => last_null_tick,
            RecoveryState::Active | RecoveryState::Waking { .. } => return,
        };
        let skipped = now.saturating_duration_since(*last_null_tick);
        *last_null_tick = now;
        log::info!(
            "physical audio owner resumed after a suspended interval; discarded {skipped:?} of wall-clock backlog"
        );
    }

    #[allow(clippy::too_many_lines)]
    fn maintain_recovery(&mut self, now: Instant) -> DriverMaintenance {
        // This budget is shared by every elapsed-time checkpoint in one
        // maintenance pass. A long sleep therefore cannot turn one owner
        // iteration into an unbounded logical catch-up loop.
        let mut null_frames_remaining = self.recovery_null_frame_budget();
        self.drain_runtime_errors();
        if self.cleanup_uncertain {
            return DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure);
        }
        let owner_resumed =
            now.saturating_duration_since(self.last_maintenance_at) >= OWNER_RESUME_GAP;
        if matches!(&self.recovery, RecoveryState::Active) {
            self.detect_callback_stall(now);
        } else {
            self.last_maintenance_at = now;
            if owner_resumed {
                // A long owner gap is process suspension, not runnable audio
                // time. Rebasing prevents hours of suspend from becoming a
                // multi-minute 10x catch-up while ordinary recovery ticks
                // continue to advance the mixer silently.
                self.rebase_recovery_cursor_after_owner_resume(now);
            }
        }
        let pending = self
            .callback
            .as_ref()
            .and_then(|callback| callback.take_recovery_event());
        let state = std::mem::replace(&mut self.recovery, RecoveryState::Active);
        match state {
            RecoveryState::Active => {
                let Some(reason) = pending else {
                    return self.maintain_idle_policy(now, &mut null_frames_remaining);
                };
                log::warn!(
                    "physical audio endpoint generation {} requires recovery: {reason:?}",
                    self.output_generation
                );
                let started_at = Instant::now();
                // Publish the recovery state before any endpoint call. The
                // first reopen is intentionally deferred to the next owner
                // maintenance iteration.
                self.recovery = RecoveryState::Waiting {
                    reason,
                    attempt: 0,
                    next_attempt: started_at,
                    backoff: RECOVERY_INITIAL_BACKOFF,
                    started_at,
                    last_null_tick: started_at,
                };
                if !self.retire_stream_for_recovery() {
                    return DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure);
                }
                let after_drop = Instant::now();
                let last_null_tick =
                    self.render_recovery_time(started_at, after_drop, &mut null_frames_remaining);
                if let RecoveryState::Waiting {
                    last_null_tick: cursor,
                    ..
                } = &mut self.recovery
                {
                    *cursor = last_null_tick;
                }
                DriverMaintenance::Continue
            }
            RecoveryState::Waiting {
                mut reason,
                attempt,
                next_attempt,
                backoff,
                started_at,
                last_null_tick,
            } => {
                if let Some(pending) = pending {
                    reason = pending;
                }
                let mut last_null_tick =
                    self.render_recovery_time(last_null_tick, now, &mut null_frames_remaining);
                if now < next_attempt {
                    self.recovery = RecoveryState::Waiting {
                        reason,
                        attempt,
                        next_attempt,
                        backoff,
                        started_at,
                        last_null_tick,
                    };
                    return DriverMaintenance::Continue;
                }
                let next_attempt_number = attempt.saturating_add(1);
                match self.attempt_reopen(&mut last_null_tick, &mut null_frames_remaining) {
                    Ok((format, generation)) => {
                        self.recovery = RecoveryState::CatchingUp {
                            reason,
                            attempt: next_attempt_number,
                            backoff,
                            started_at,
                            last_null_tick,
                            generation,
                            format,
                        };
                        DriverMaintenance::Continue
                    }
                    Err(error) if self.cleanup_uncertain => {
                        log::error!("physical audio recovery lost teardown proof: {error}");
                        DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure)
                    }
                    Err(error) if error.kind() == SystemOutputErrorKind::Protocol => {
                        log::error!(
                            "physical audio recovery encountered a terminal protocol failure: {error}"
                        );
                        DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure)
                    }
                    Err(error) => {
                        self.schedule_recovery_wait(
                            reason,
                            next_attempt_number,
                            backoff,
                            started_at,
                            last_null_tick,
                            &error,
                        );
                        DriverMaintenance::Continue
                    }
                }
            }
            RecoveryState::CatchingUp {
                mut reason,
                attempt,
                backoff,
                started_at,
                mut last_null_tick,
                generation,
                format,
            } => {
                let mut pending = pending;
                let catch_up_to = Instant::now();
                if pending.is_none() {
                    last_null_tick = self.render_recovery_time(
                        last_null_tick,
                        catch_up_to,
                        &mut null_frames_remaining,
                    );
                    // Error callbacks are live while data callbacks are
                    // paused. Drain once more before activation so a failure
                    // observed during catch-up retires this provisional
                    // endpoint without letting it render physical data.
                    self.drain_runtime_errors();
                    pending = self
                        .callback
                        .as_ref()
                        .and_then(|callback| callback.take_recovery_event());
                }
                if let Some(pending) = pending {
                    reason = pending;
                    let observed_at = Instant::now();
                    // Publish the waiting state before destroying a native
                    // stream. The retry deadline is refreshed after teardown
                    // so slow destruction cannot consume the backoff.
                    self.recovery = RecoveryState::Waiting {
                        reason,
                        attempt,
                        next_attempt: observed_at + backoff,
                        backoff: backoff.saturating_mul(2).min(RECOVERY_MAX_BACKOFF),
                        started_at,
                        last_null_tick,
                    };
                    if !self.retire_stream_for_recovery() {
                        return DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure);
                    }
                    let last_null_tick = self.render_recovery_time(
                        last_null_tick,
                        Instant::now(),
                        &mut null_frames_remaining,
                    );
                    let detail =
                        format!("{reason:?} while the replacement endpoint was still provisional");
                    self.schedule_recovery_wait(
                        reason,
                        attempt,
                        backoff,
                        started_at,
                        last_null_tick,
                        detail,
                    );
                    return DriverMaintenance::Continue;
                }

                // Compare with the same instant used for rendering. Looking
                // at the clock again here would manufacture a fresh partial
                // backlog from owner-side bookkeeping and could prevent a
                // 48 kHz stream from ever crossing the one-frame threshold.
                if self.recovery_frames_between(last_null_tick, catch_up_to) != 0 {
                    self.recovery = RecoveryState::CatchingUp {
                        reason,
                        attempt,
                        backoff,
                        started_at,
                        last_null_tick,
                        generation,
                        format,
                    };
                    return DriverMaintenance::Continue;
                }
                let activation = self
                    .callback
                    .as_ref()
                    .map_or(GenerationActivation::Invalid, |callback| {
                        callback.activate_generation(generation)
                    });
                match activation {
                    GenerationActivation::Activated => {}
                    GenerationActivation::Deferred => {
                        // An advisory error callback was already admitted at
                        // the handoff point. It does not invalidate the new
                        // endpoint, but activation waits for the callback to
                        // leave so the atomic phase transition remains the
                        // sole linearization point.
                        self.recovery = RecoveryState::CatchingUp {
                            reason,
                            attempt,
                            backoff,
                            started_at,
                            last_null_tick,
                            generation,
                            format,
                        };
                        return DriverMaintenance::Continue;
                    }
                    GenerationActivation::RuntimeError => {
                        return self.retire_after_activation_lost(
                            attempt,
                            backoff,
                            started_at,
                            last_null_tick,
                            &mut null_frames_remaining,
                        );
                    }
                    GenerationActivation::Invalid => {
                        let error = SystemOutputError::new(
                            SystemOutputErrorKind::Protocol,
                            SystemOutputOperation::Play,
                            "the recovered physical stream could not be activated",
                        );
                        log::error!(
                            "physical audio recovery encountered a terminal protocol failure: {error}"
                        );
                        return DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure);
                    }
                }
                let recovered_at = Instant::now();
                log::info!(
                    "physical audio recovered on generation {} after {} attempts and {:?}: {format:?}",
                    self.output_generation,
                    attempt,
                    recovered_at.saturating_duration_since(started_at)
                );
                self.reset_runtime_event_log();
                self.note_recovered(recovered_at);
                self.note_attached(recovered_at);
                self.recovery = RecoveryState::Active;
                DriverMaintenance::Continue
            }
            RecoveryState::Idle { last_null_tick } => {
                // No native stream exists, so `pending` can hold nothing
                // actionable: error callbacks were joined at release.
                let (last_null_tick, rendered) =
                    self.render_idle_time(last_null_tick, now, &mut null_frames_remaining);
                if rendered != OwnerRender::Audible {
                    self.recovery = RecoveryState::Idle { last_null_tick };
                    return DriverMaintenance::Continue;
                }
                log::debug!("physical audio output demanded; attaching the native stream");
                self.wake_from_idle()
            }
            RecoveryState::Waking { generation } => self.maintain_waking(generation),
        }
    }

    /// Active-stream idle accounting for the demand-driven policy.
    fn maintain_idle_policy(
        &mut self,
        now: Instant,
        null_frames_remaining: &mut usize,
    ) -> DriverMaintenance {
        let OutputIdlePolicy::DetachWhenIdle { after } = self.idle else {
            return DriverMaintenance::Continue;
        };
        let Some(callback) = self.callback.as_ref() else {
            return DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure);
        };
        if callback.take_audible() {
            self.last_audible_at = now;
            return DriverMaintenance::Continue;
        }
        if now.saturating_duration_since(self.last_audible_at) < after {
            return DriverMaintenance::Continue;
        }
        log::debug!("physical audio output silent for {after:?}; releasing the native stream");
        // Publish the idle state before destroying the stream, as recovery
        // does, so a teardown unwind is observed against the intended state.
        self.recovery = RecoveryState::Idle {
            last_null_tick: now,
        };
        if !self.retire_stream_for_recovery() {
            return DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure);
        }
        if let Some(callback) = self.callback.as_ref() {
            // Callbacks admitted before the pause may have flagged audio that
            // the stream already played; it is not new demand.
            callback.take_audible();
        }
        let last_null_tick = self.render_recovery_time(now, Instant::now(), null_frames_remaining);
        self.recovery = RecoveryState::Idle { last_null_tick };
        DriverMaintenance::Continue
    }

    /// Null-renders idle wall-clock time, keeping an audible result as the
    /// primer. Mirrors [`Self::render_recovery_time`] with demand detection.
    fn render_idle_time(
        &self,
        from: Instant,
        now: Instant,
        frames_remaining: &mut usize,
    ) -> (Instant, OwnerRender) {
        let Some(callback) = self.callback.as_ref() else {
            return (from, OwnerRender::Silent);
        };
        let frames = self
            .recovery_frames_between(from, now)
            .min(*frames_remaining)
            .min(MAX_PHYSICAL_CALLBACK_FRAMES);
        if frames == 0 {
            return (from, OwnerRender::Silent);
        }
        let Some(rendered) = callback.render_owner_side(frames, true) else {
            return (from, OwnerRender::Silent);
        };
        *frames_remaining -= frames;
        let rendered_nanos =
            u64::try_from((frames as u128 * 1_000_000_000) / u128::from(self.sample_rate))
                .expect("bounded callback frames fit a u64 nanosecond duration");
        (from + Duration::from_nanos(rendered_nanos), rendered)
    }

    /// Attaches a stream for demanded output. Logical time stays frozen: the
    /// primer already holds the onset, so no null budget is spent.
    fn wake_from_idle(&mut self) -> DriverMaintenance {
        let mut frozen_tick = Instant::now();
        let mut frozen_budget = 0;
        match self.attempt_reopen(&mut frozen_tick, &mut frozen_budget) {
            Ok((_format, generation)) => self.maintain_waking(generation),
            Err(error) if self.cleanup_uncertain => {
                log::error!("physical audio output attach lost teardown proof: {error}");
                DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure)
            }
            Err(error) if error.kind() == SystemOutputErrorKind::Protocol => {
                log::error!(
                    "physical audio output attach encountered a terminal protocol failure: {error}"
                );
                DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure)
            }
            Err(error) => {
                // The endpoint changed or vanished while idle. Ordinary
                // recovery takes over with backoff; by the time it succeeds
                // the primer is stale.
                if let Some(callback) = self.callback.as_ref() {
                    callback.clear_primer();
                }
                let started_at = Instant::now();
                self.schedule_recovery_wait(
                    runtime_event_for(&error),
                    1,
                    RECOVERY_INITIAL_BACKOFF,
                    started_at,
                    started_at,
                    &error,
                );
                DriverMaintenance::Continue
            }
        }
    }

    /// Admits the data callback of a demanded stream once no error callback
    /// contends for the handoff.
    fn maintain_waking(&mut self, generation: u64) -> DriverMaintenance {
        self.drain_runtime_errors();
        let pending = self
            .callback
            .as_ref()
            .and_then(|callback| callback.take_recovery_event());
        if let Some(reason) = pending {
            let observed_at = Instant::now();
            self.recovery = RecoveryState::Waiting {
                reason,
                attempt: 1,
                next_attempt: observed_at + RECOVERY_INITIAL_BACKOFF,
                backoff: RECOVERY_INITIAL_BACKOFF
                    .saturating_mul(2)
                    .min(RECOVERY_MAX_BACKOFF),
                started_at: observed_at,
                last_null_tick: observed_at,
            };
            if !self.retire_stream_for_recovery() {
                return DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure);
            }
            if let Some(callback) = self.callback.as_ref() {
                callback.clear_primer();
            }
            let detail = format!("{reason:?} while the demanded endpoint was still provisional");
            self.schedule_recovery_wait(
                reason,
                1,
                RECOVERY_INITIAL_BACKOFF,
                observed_at,
                Instant::now(),
                detail,
            );
            return DriverMaintenance::Continue;
        }
        let activation = self
            .callback
            .as_ref()
            .map_or(GenerationActivation::Invalid, |callback| {
                callback.activate_generation(generation)
            });
        match activation {
            GenerationActivation::Activated => {
                let attached_at = Instant::now();
                self.reset_runtime_event_log();
                self.note_recovered(attached_at);
                self.note_attached(attached_at);
                self.recovery = RecoveryState::Active;
                log::debug!(
                    "physical audio output attached on generation {}",
                    self.output_generation
                );
                DriverMaintenance::Continue
            }
            GenerationActivation::Deferred => {
                self.recovery = RecoveryState::Waking { generation };
                DriverMaintenance::Continue
            }
            GenerationActivation::RuntimeError => {
                let observed_at = Instant::now();
                let mut frozen_budget = 0;
                self.retire_after_activation_lost(
                    1,
                    RECOVERY_INITIAL_BACKOFF,
                    observed_at,
                    observed_at,
                    &mut frozen_budget,
                )
            }
            GenerationActivation::Invalid => {
                let error = SystemOutputError::new(
                    SystemOutputErrorKind::Protocol,
                    SystemOutputOperation::Play,
                    "the demanded physical stream could not be activated",
                );
                log::error!(
                    "physical audio output attach encountered a terminal protocol failure: {error}"
                );
                DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure)
            }
        }
    }

    /// Restarts idle accounting for a stream that just became active.
    fn note_attached(&mut self, at: Instant) {
        self.last_audible_at = at;
        if let Some(callback) = self.callback.as_ref() {
            callback.take_audible();
        }
    }

    /// The error callback won the atomic handoff that would have admitted
    /// data callbacks. Publishes Waiting first, then retires the still-silent
    /// endpoint and uses the callback's exact classified event when present.
    fn retire_after_activation_lost(
        &mut self,
        attempt: u32,
        backoff: Duration,
        started_at: Instant,
        last_null_tick: Instant,
        null_frames_remaining: &mut usize,
    ) -> DriverMaintenance {
        let observed_at = Instant::now();
        self.recovery = RecoveryState::Waiting {
            reason: DriverRuntimeEvent::BackendError,
            attempt,
            next_attempt: observed_at + backoff,
            backoff: backoff.saturating_mul(2).min(RECOVERY_MAX_BACKOFF),
            started_at,
            last_null_tick,
        };
        if !self.retire_stream_for_recovery() {
            return DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure);
        }
        if let Some(callback) = self.callback.as_ref() {
            callback.clear_primer();
        }
        let terminal_failure = self
            .callback
            .as_ref()
            .and_then(|callback| callback.failures.0.upgrade())
            .and_then(|status| status.failure());
        if let Some(failure) = terminal_failure {
            return DriverMaintenance::Terminal(failure);
        }
        let reason = self
            .callback
            .as_ref()
            .and_then(|callback| callback.take_recovery_event())
            .unwrap_or(DriverRuntimeEvent::BackendError);
        let last_null_tick =
            self.render_recovery_time(last_null_tick, Instant::now(), null_frames_remaining);
        let detail = format!("{reason:?} won replacement-endpoint activation");
        self.schedule_recovery_wait(reason, attempt, backoff, started_at, last_null_tick, detail);
        DriverMaintenance::Continue
    }

    fn drop_device(&mut self) -> Result<(), SystemOutputError> {
        let result = drop_owned(
            &mut self.device,
            "the native output device destructor unwound",
        );
        if result.is_err() {
            self.cleanup_uncertain = true;
        }
        result
    }
}

impl<F: HostFactory> Drop for CpalOutputDriver<F> {
    fn drop(&mut self) {
        if !self.retired {
            // `MonitoredBackend` retains this entire driver on an unproven
            // close, so this path is only reachable after proof.
            return;
        }
        let callback = self.retired_callback.take();
        let callback_dropped = catch_unwind(AssertUnwindSafe(|| drop(callback)));
        if let Err(payload) = callback_dropped {
            std::mem::forget(payload);
            panic!("system callback state destructor unwound after retirement proof");
        }
        // SAFETY: the stream is destroyed and callback authority is gone.
        unsafe { ManuallyDrop::drop(&mut self.lease) };
    }
}

#[derive(Clone, Copy)]
struct SystemHostFactory;

struct SystemHost(cpal::Host);
struct SystemDevice(cpal::Device);
struct SystemStream(cpal::Stream);

impl HostFactory for SystemHostFactory {
    type Host = SystemHost;

    fn create(self) -> Result<Self::Host, HostFailure> {
        Ok(SystemHost(cpal::default_host()))
    }
}

impl OutputHost for SystemHost {
    type Device = SystemDevice;

    fn default_output_device(&self) -> Result<Option<Self::Device>, HostFailure> {
        Ok(self.0.default_output_device().map(SystemDevice))
    }
}

impl OutputDevice for SystemDevice {
    type Stream = SystemStream;

    fn supported_output_configs(&self) -> Result<Vec<OutputConfigRange>, HostFailure> {
        let configs = self
            .0
            .supported_output_configs()
            .map_err(|error| host_failure_from_cpal(&error))?;
        Ok(configs
            .map(|config| OutputConfigRange {
                channels: usize::from(config.channels()),
                min_sample_rate: config.min_sample_rate(),
                max_sample_rate: config.max_sample_rate(),
                sample_format: match config.sample_format() {
                    SampleFormat::F32 => DriverSampleFormat::F32,
                    SampleFormat::I16 => DriverSampleFormat::I16,
                    SampleFormat::U16 => DriverSampleFormat::U16,
                    _ => DriverSampleFormat::Other,
                },
                buffer_size: match *config.buffer_size() {
                    SupportedBufferSize::Range { min, max } => OutputBufferRange::Range {
                        min: min as usize,
                        max: max as usize,
                    },
                    SupportedBufferSize::Unknown => OutputBufferRange::Unknown,
                },
            })
            .collect())
    }

    fn build_output_stream(
        &self,
        config: OutputStreamConfig,
        mut data: OutputDataCallback,
        mut error: OutputErrorCallback,
    ) -> Result<Self::Stream, HostFailure> {
        let channels = u16::try_from(config.channels).map_err(|_| {
            HostFailure::new(
                SystemOutputErrorKind::Protocol,
                "physical channel count does not fit CPAL",
            )
        })?;
        let buffer_size = match config.buffer_size {
            OutputBufferRequest::Default => BufferSize::Default,
            OutputBufferRequest::Fixed(frames) => {
                BufferSize::Fixed(u32::try_from(frames).map_err(|_| {
                    HostFailure::new(
                        SystemOutputErrorKind::Protocol,
                        "physical buffer hint does not fit CPAL",
                    )
                })?)
            }
        };
        let stream_config = StreamConfig {
            channels,
            sample_rate: config.sample_rate,
            buffer_size,
        };
        let sample_format = match config.sample_format {
            DriverSampleFormat::F32 => SampleFormat::F32,
            DriverSampleFormat::I16 => SampleFormat::I16,
            DriverSampleFormat::U16 => SampleFormat::U16,
            DriverSampleFormat::Other => {
                return Err(HostFailure::new(
                    SystemOutputErrorKind::UnsupportedFormat,
                    "the planned sample format is not supported",
                ));
            }
        };
        let stream = self
            .0
            .build_output_stream_raw(
                stream_config,
                sample_format,
                move |raw, _| dispatch_raw_output(raw, &mut data),
                move |platform_error| {
                    error(platform_error);
                },
                None,
            )
            .map_err(|error| host_failure_from_cpal(&error))?;
        Ok(SystemStream(stream))
    }
}

fn dispatch_raw_output(raw: &mut cpal::Data, data: &mut OutputDataCallback) {
    match raw.sample_format() {
        SampleFormat::F32 => data(raw.as_slice_mut::<f32>().map(OutputBuffer::F32)),
        SampleFormat::I16 => data(raw.as_slice_mut::<i16>().map(OutputBuffer::I16)),
        SampleFormat::U16 => data(raw.as_slice_mut::<u16>().map(OutputBuffer::U16)),
        _ => {
            // CPAL's raw output contract pre-fills the entire buffer with the
            // format's equilibrium. Preserve that opaque equilibrium and let
            // the callback state latch the protocol mismatch without casting.
            data(None);
        }
    }
}

impl OutputStream for SystemStream {
    fn play(&self) -> Result<(), HostFailure> {
        self.0
            .play()
            .map_err(|error| host_failure_from_cpal(&error))
    }
}

fn host_failure_from_cpal(error: &cpal::Error) -> HostFailure {
    let kind = match error.kind() {
        cpal::ErrorKind::DeviceBusy => SystemOutputErrorKind::OutputInUse,
        cpal::ErrorKind::DeviceNotAvailable
        | cpal::ErrorKind::HostUnavailable
        | cpal::ErrorKind::PermissionDenied => SystemOutputErrorKind::DeviceUnavailable,
        cpal::ErrorKind::UnsupportedConfig | cpal::ErrorKind::UnsupportedOperation => {
            SystemOutputErrorKind::UnsupportedFormat
        }
        cpal::ErrorKind::InvalidInput => SystemOutputErrorKind::Protocol,
        _ => SystemOutputErrorKind::BackendFailure,
    };
    HostFailure::new(kind, error.to_string())
}

#[cfg(test)]
mod tests;
