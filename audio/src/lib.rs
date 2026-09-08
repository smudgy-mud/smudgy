#![cfg_attr(
    not(any(test, feature = "test-support")),
    allow(
        dead_code,
        reason = "S2a lands the private driver seam before S2b adds its production constructor"
    )
)]

//! Process-level audio mixing primitives for Smudgy.
//!
//! This crate deliberately has no Deno, V8, UI, package, or session-runtime
//! dependency. It owns the bounded physical-mixer topology that those layers
//! use through explicit adapters.

#[cfg(feature = "physical-output")]
mod system;

#[cfg(feature = "physical-output")]
pub use system::{
    SystemMixerService, SystemMixerStartError, SystemMixerUnavailable, SystemOutputError,
    SystemOutputErrorKind, SystemOutputOperation,
};

use std::{
    cell::UnsafeCell,
    collections::HashMap,
    convert::Infallible,
    fmt,
    future::Future,
    marker::PhantomData,
    mem::ManuallyDrop,
    panic::{AssertUnwindSafe, catch_unwind},
    pin::Pin,
    rc::Rc,
    sync::{
        Arc, Condvar, Mutex, Weak,
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError},
    },
    task::{Context, Poll, Waker},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use futures::channel::{mpsc as futures_mpsc, oneshot};
use kira::{
    AudioManager, AudioManagerSettings, Capacities, Decibels, Frame as KiraFrame, Tween,
    backend::{Backend, Renderer},
    sound::{Sound, SoundData},
    track::{MainTrackBuilder, TrackBuilder, TrackHandle},
};

/// Smallest accepted application or session linear gain.
pub const MIN_CONTROL_GAIN: f32 = 0.0;
/// Largest accepted application or session linear gain.
pub const MAX_CONTROL_GAIN: f32 = 1.0;
/// Initial remembered application and session linear gain.
pub const DEFAULT_CONTROL_GAIN: f32 = 1.0;

/// The fixed number of frames in one internal mixer quantum.
pub const INTERNAL_BUFFER_FRAMES: usize = 128;
/// Hard maximum accepted from one physical output callback.
pub const MAX_PHYSICAL_CALLBACK_FRAMES: usize = 8192;
/// Maximum number of simultaneous sessions in the first mixer profile.
pub const MAX_SESSIONS: usize = 32;
/// Maximum number of simultaneous inputs on each session bus.
pub const INPUTS_PER_BUS: usize = 32;
/// Bounded number of pending ordinary control operations.
pub const CONTROL_QUEUE_CAPACITY: usize = 128;

const SESSION_BUS_COUNT: usize = 3;
const RETIREMENT_QUEUE_CAPACITY: usize = MAX_SESSIONS * SESSION_BUS_COUNT * INPUTS_PER_BUS;
const SESSION_RETIREMENT_QUEUE_CAPACITY: usize = MAX_SESSIONS;
const RETIREMENT_SCAN_INTERVAL: Duration = Duration::from_millis(10);
const SESSION_TRACK_RETIREMENT_TIMEOUT: Duration = Duration::from_secs(2);

/// A stable session identity within one process mixer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioSessionId(pub u64);

/// A source category within a session's mixer subtree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionBus {
    /// Web Audio and other script-created playback.
    Script,
    /// Native protocol and client-interface sounds.
    Native,
    /// Speech and accessibility playback.
    Speech,
}

impl SessionBus {
    const fn ordinal(self) -> usize {
        match self {
            Self::Script => 0,
            Self::Native => 1,
            Self::Speech => 2,
        }
    }
}

/// Applied state of one application or session gain authority.
///
/// Muting is independent from the remembered linear gain. A muted authority
/// therefore reports an effective gain of zero without discarding the value
/// that will be restored when it is unmuted.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MixerGainState {
    linear: f32,
    muted: bool,
}

impl MixerGainState {
    /// Remembered finite linear gain in [`MIN_CONTROL_GAIN`] through
    /// [`MAX_CONTROL_GAIN`], inclusive.
    #[must_use]
    pub const fn linear(self) -> f32 {
        self.linear
    }

    /// Whether this authority is currently muted.
    #[must_use]
    pub const fn is_muted(self) -> bool {
        self.muted
    }

    /// Gain currently applied to the owned Kira track.
    #[must_use]
    pub const fn effective_linear(self) -> f32 {
        if self.muted { 0.0 } else { self.linear }
    }
}

impl Default for MixerGainState {
    fn default() -> Self {
        Self {
            linear: DEFAULT_CONTROL_GAIN,
            muted: false,
        }
    }
}

#[derive(Debug, Clone, Copy)]
enum MixerGainUpdate {
    Linear(f32),
    Muted(bool),
    State(MixerGainState),
}

impl MixerGainUpdate {
    fn apply(self, current: MixerGainState) -> MixerGainState {
        match self {
            Self::Linear(linear) => MixerGainState { linear, ..current },
            Self::Muted(muted) => MixerGainState { muted, ..current },
            Self::State(state) => state,
        }
    }
}

fn validate_control_gain(linear: f32) -> Result<f32, MixerControlError> {
    if !linear.is_finite() || !(MIN_CONTROL_GAIN..=MAX_CONTROL_GAIN).contains(&linear) {
        return Err(MixerControlError::InvalidGain);
    }
    Ok(if linear == 0.0 { 0.0 } else { linear })
}

fn linear_to_decibels(linear: f32) -> Decibels {
    if linear == 0.0 {
        Decibels::SILENCE
    } else {
        Decibels(20.0 * linear.log10())
    }
}

fn immediate_tween() -> Tween {
    Tween {
        duration: Duration::ZERO,
        ..Tween::default()
    }
}

/// Exact logical format mixed by one [`MixerService`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixerFormat {
    sample_rate: u32,
}

impl MixerFormat {
    /// Sample rate verified against the backend during service startup.
    #[must_use]
    pub const fn sample_rate(self) -> u32 {
        self.sample_rate
    }
    /// The mixer is permanently stereo.
    #[must_use]
    pub const fn number_of_channels(self) -> usize {
        2
    }
    /// Maximum frames in one bounded logical render chunk.
    ///
    /// One physical callback can contain multiple chunks, up to
    /// [`MAX_PHYSICAL_CALLBACK_FRAMES`] in total.
    #[must_use]
    pub const fn max_frames_per_callback(self) -> usize {
        INTERNAL_BUFFER_FRAMES
    }
}

/// Sample encoding selected for the physical output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhysicalSampleFormat {
    /// Native 32-bit floating-point samples.
    F32,
    /// Native signed 16-bit integer samples.
    I16,
    /// Native unsigned 16-bit integer samples.
    U16,
}

/// Exact format negotiated for one physical output stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhysicalOutputFormat {
    sample_rate: u32,
    channels: usize,
    sample_format: PhysicalSampleFormat,
    buffer_frames_hint: Option<usize>,
}

impl PhysicalOutputFormat {
    /// Negotiated physical sample rate.
    #[must_use]
    pub const fn sample_rate(self) -> u32 {
        self.sample_rate
    }
    /// Negotiated physical channel count.
    #[must_use]
    pub const fn number_of_channels(self) -> usize {
        self.channels
    }
    /// Negotiated physical sample encoding.
    #[must_use]
    pub const fn sample_format(self) -> PhysicalSampleFormat {
        self.sample_format
    }
    /// Driver-requested buffer-size hint, when the driver has one.
    #[must_use]
    pub const fn buffer_frames_hint(self) -> Option<usize> {
        self.buffer_frames_hint
    }
    /// Maximum callback size accepted by the bounded conversion/render path.
    #[must_use]
    pub const fn max_frames_per_callback(self) -> usize {
        MAX_PHYSICAL_CALLBACK_FRAMES
    }
}

/// One stereo sample exchanged with a [`MixerInput`].
///
/// This value belongs to `smudgy_audio`; mixer inputs do not depend on the
/// physical mixer's internal frame representation.
#[derive(Debug, Default, Clone, Copy, PartialEq)]
pub struct MixerFrame {
    left: f32,
    right: f32,
}

impl MixerFrame {
    /// A silent stereo frame.
    pub const ZERO: Self = Self::new(0.0, 0.0);

    /// Creates a frame from separate left and right samples.
    #[must_use]
    pub const fn new(left: f32, right: f32) -> Self {
        Self { left, right }
    }

    /// Creates a frame with the same sample in both channels.
    #[must_use]
    pub const fn from_mono(sample: f32) -> Self {
        Self::new(sample, sample)
    }

    /// Returns the left-channel sample.
    #[must_use]
    pub const fn left(self) -> f32 {
        self.left
    }

    /// Returns the right-channel sample.
    #[must_use]
    pub const fn right(self) -> f32 {
        self.right
    }
}

/// Whether an input has more audio to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixerInputStatus {
    /// Keep the input active for the next quantum.
    Active,
    /// Close this logical input after the current quantum.
    Finished,
}

/// A real-time audio producer installed into one reserved mixer slot.
///
/// Implementations must overwrite the complete output slice and must not
/// allocate, block, or unwind. The slot contains an unexpected unwind and
/// fails closed so it cannot cross the backend callback boundary.
pub trait MixerInput: Send + 'static {
    /// Render the next stereo quantum.
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus;

    /// Returns an off-render observer for process-output failure.
    ///
    /// The default is no observer. Implementations that return one must
    /// prebuild it before reserving mixer capacity. Smudgy invokes it at most
    /// once on the cleanup worker, never on the physical callback or command
    /// owner, and contains any unwind.
    fn output_failure_observer(&self) -> Option<Arc<dyn MixerFailureObserver>> {
        None
    }
}

/// Stable operational failure of the shared process output.
///
/// This notification is deliberately separate from proof-bearing logical
/// input retirement: an endpoint can learn why output died before its cleanup
/// future proves whether callback ownership was retired safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixerOutputFailure {
    /// The physical driver stopped or reported a device/backend error.
    BackendFailure,
    /// Render callbacks stopped advancing beyond the resume grace period.
    CallbackStalled,
    /// The physical callback supplied invalid channel or buffer geometry.
    InvalidCallbackGeometry,
    /// The Kira render boundary unwound and was silenced.
    RendererPanicked,
    /// The sole mixer command owner unwound.
    OwnerPanicked,
}

/// One bounded, change-driven stream of terminal process-output failures.
///
/// The sole mixer owner publishes at most one item. Recoverable endpoint
/// interruptions are deliberately not observable through this stream.
pub type MixerOutputFailureReceiver = futures_mpsc::Receiver<MixerOutputFailure>;

/// Receives one off-render process-output failure notification for an input.
pub trait MixerFailureObserver: Send + Sync + 'static {
    /// Publish the first operational failure to the hosted endpoint.
    ///
    /// Implementations must finish in bounded time and must not block. Calls
    /// share the sole cleanup worker with proof-bearing source destruction;
    /// Smudgy contains unwinds but cannot recover a worker blocked by user code.
    fn output_failed(&self, failure: MixerOutputFailure);
}

const PHASE_MASK: u64 = 0x0f;
const ACTIVE: u64 = 1 << 4;
const SUSPENDED: u64 = 1 << 5;
const FAILED: u64 = 1 << 6;
const HAS_PAYLOAD: u64 = 1 << 7;
const SLOT_CAUSE_SHIFT: u32 = 8;
const SLOT_CAUSE_MASK: u64 = 0x07 << SLOT_CAUSE_SHIFT;
const GENERATION_SHIFT: u32 = 11;
const MAX_GENERATION: u64 = u64::MAX >> GENERATION_SHIFT;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
enum SlotPhase {
    Free = 0,
    Reserved = 1,
    Installed = 2,
    Running = 3,
    Closing = 4,
    Retiring = 5,
    ForcedClean = 6,
    Quarantined = 7,
}

const fn slot_word(generation: u64, phase: SlotPhase, flags: u64) -> u64 {
    (generation << GENERATION_SHIFT) | phase as u64 | flags
}

const fn slot_generation(word: u64) -> u64 {
    word >> GENERATION_SHIFT
}

fn slot_phase(word: u64) -> SlotPhase {
    match word & PHASE_MASK {
        0 => SlotPhase::Free,
        1 => SlotPhase::Reserved,
        2 => SlotPhase::Installed,
        3 => SlotPhase::Running,
        4 => SlotPhase::Closing,
        5 => SlotPhase::Retiring,
        6 => SlotPhase::ForcedClean,
        _ => SlotPhase::Quarantined,
    }
}

const fn slot_failure_cause(failure: MixerOutputFailure) -> u64 {
    let cause = match failure {
        MixerOutputFailure::BackendFailure => 1,
        MixerOutputFailure::InvalidCallbackGeometry => 2,
        MixerOutputFailure::RendererPanicked => 3,
        MixerOutputFailure::OwnerPanicked => 4,
        MixerOutputFailure::CallbackStalled => 5,
    };
    cause << SLOT_CAUSE_SHIFT
}

fn decode_slot_failure(word: u64) -> Option<MixerOutputFailure> {
    match (word & SLOT_CAUSE_MASK) >> SLOT_CAUSE_SHIFT {
        1 => Some(MixerOutputFailure::BackendFailure),
        2 => Some(MixerOutputFailure::InvalidCallbackGeometry),
        3 => Some(MixerOutputFailure::RendererPanicked),
        4 => Some(MixerOutputFailure::OwnerPanicked),
        5 => Some(MixerOutputFailure::CallbackStalled),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SessionKey {
    id: AudioSessionId,
    generation: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct SlotAddress {
    session: SessionKey,
    bus: SessionBus,
    index: usize,
}

struct ForcedWaiter {
    generation: u64,
    result: Option<Result<MixerInputRetirement, MixerRetirementError>>,
    waker: Option<Waker>,
    retained: Option<ManuallyDrop<Arc<InputSlot>>>,
}

struct PreparedRetirement {
    source: Option<Box<dyn MixerInput>>,
    observer: ManuallyDrop<Option<Arc<dyn MixerFailureObserver>>>,
    pending_failure_notification: Option<MixerOutputFailure>,
    result: Result<MixerInputRetirement, MixerRetirementError>,
}

struct ObserverAuthority(ManuallyDrop<Option<Arc<dyn MixerFailureObserver>>>);

impl ObserverAuthority {
    const fn new() -> Self {
        Self(ManuallyDrop::new(None))
    }

    fn replace(&mut self, observer: Option<Arc<dyn MixerFailureObserver>>) {
        if let Some(existing) = self.0.take() {
            std::mem::forget(existing);
        }
        *self.0 = observer;
    }

    fn clone_observer(&self) -> Option<Arc<dyn MixerFailureObserver>> {
        self.0.as_ref().map(Arc::clone)
    }

    fn take(&mut self) -> Option<Arc<dyn MixerFailureObserver>> {
        self.0.take()
    }
}

struct InputSlot {
    address: SlotAddress,
    word: AtomicU64,
    payload: UnsafeCell<ManuallyDrop<Option<Box<dyn MixerInput>>>>,
    failure_observer: Mutex<ObserverAuthority>,
    failure_notified: AtomicBool,
    forced: Mutex<ForcedWaiter>,
}

// The packed ACTIVE bit grants the render callback exclusive source access.
// Every Kira-side reference is Weak. Off-RT payload mutation is allowed only in
// phases in which ACTIVE cannot be newly acquired.
unsafe impl Sync for InputSlot {}

impl InputSlot {
    fn new(address: SlotAddress) -> Self {
        Self {
            address,
            word: AtomicU64::new(slot_word(1, SlotPhase::Free, 0)),
            payload: UnsafeCell::new(ManuallyDrop::new(None)),
            failure_observer: Mutex::new(ObserverAuthority::new()),
            failure_notified: AtomicBool::new(false),
            forced: Mutex::new(ForcedWaiter {
                generation: 1,
                result: None,
                waker: None,
                retained: None,
            }),
        }
    }

    fn reserve(&self) -> Result<u64, MixerMutationError> {
        loop {
            let word = self.word.load(Ordering::Acquire);
            if slot_phase(word) != SlotPhase::Free {
                return Err(MixerMutationError::InternalInvariant);
            }
            let generation = slot_generation(word);
            if generation == 0 || generation > MAX_GENERATION {
                self.quarantine_word(word);
                return Err(MixerMutationError::GenerationExhausted);
            }
            if self
                .word
                .compare_exchange_weak(
                    word,
                    slot_word(generation, SlotPhase::Reserved, 0),
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                let mut forced = lock_recover(&self.forced);
                forced.generation = generation;
                forced.result = None;
                forced.waker = None;
                debug_assert!(forced.retained.is_none());
                self.failure_notified.store(false, Ordering::Release);
                return Ok(generation);
            }
        }
    }

    fn install(
        &self,
        generation: u64,
        source: Box<dyn MixerInput>,
    ) -> Result<(), Box<dyn MixerInput>> {
        let expected = slot_word(generation, SlotPhase::Reserved, 0);
        if self.word.load(Ordering::Acquire) != expected {
            return Err(source);
        }
        let observer = match catch_unwind(AssertUnwindSafe(|| source.output_failure_observer())) {
            Ok(observer) => observer,
            Err(payload) => {
                std::mem::forget(payload);
                return Err(source);
            }
        };
        // SAFETY: Reserved cannot enter rendering. Start admission excludes the
        // service seal/forced-cleanup path until Installed is opened or dropped.
        let payload = unsafe { &mut **self.payload.get() };
        if payload.is_some() {
            self.quarantine_word(expected);
            return Err(source);
        }
        *payload = Some(source);
        lock_recover(&self.failure_observer).replace(observer);
        if self
            .word
            .compare_exchange(
                expected,
                slot_word(generation, SlotPhase::Installed, HAS_PAYLOAD),
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_err()
        {
            let observer = lock_recover(&self.failure_observer).take();
            forget_observer_panic(observer);
            return Err(payload.take().expect("installed payload disappeared"));
        }
        Ok(())
    }

    fn open(&self, generation: u64) -> bool {
        self.word
            .compare_exchange(
                slot_word(generation, SlotPhase::Installed, HAS_PAYLOAD),
                slot_word(generation, SlotPhase::Running, HAS_PAYLOAD),
                Ordering::Release,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn set_suspended(&self, generation: u64, suspended: bool) -> bool {
        loop {
            let word = self.word.load(Ordering::Acquire);
            if slot_generation(word) != generation || slot_phase(word) != SlotPhase::Running {
                return false;
            }
            let next = if suspended {
                word | SUSPENDED
            } else {
                word & !SUSPENDED
            };
            if next == word {
                return true;
            }
            if self
                .word
                .compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn close(&self, generation: u64, failed: bool) -> bool {
        loop {
            let word = self.word.load(Ordering::Acquire);
            if slot_generation(word) != generation {
                return false;
            }
            match slot_phase(word) {
                SlotPhase::Reserved | SlotPhase::Installed | SlotPhase::Running => {}
                SlotPhase::Closing
                | SlotPhase::Retiring
                | SlotPhase::ForcedClean
                | SlotPhase::Quarantined => return true,
                SlotPhase::Free => return false,
            }
            let flags = (word & (ACTIVE | HAS_PAYLOAD | SLOT_CAUSE_MASK))
                | if failed || word & FAILED != 0 {
                    FAILED
                } else {
                    0
                };
            let next = slot_word(generation, SlotPhase::Closing, flags);
            if self
                .word
                .compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn close_after_output_failure(&self, generation: u64, failure: MixerOutputFailure) -> bool {
        loop {
            let word = self.word.load(Ordering::Acquire);
            if slot_generation(word) != generation {
                return false;
            }
            let phase = slot_phase(word);
            match phase {
                SlotPhase::Reserved => return self.close(generation, false),
                SlotPhase::Installed | SlotPhase::Running | SlotPhase::Closing => {}
                SlotPhase::Retiring | SlotPhase::ForcedClean | SlotPhase::Quarantined => {
                    return true;
                }
                SlotPhase::Free => return false,
            }
            if word & FAILED != 0 && decode_slot_failure(word) == Some(failure) {
                return true;
            }
            let next = slot_word(
                generation,
                SlotPhase::Closing,
                (word & (ACTIVE | HAS_PAYLOAD)) | FAILED | slot_failure_cause(failure),
            );
            if self
                .word
                .compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn try_enter(&self) -> SlotEntry {
        loop {
            let word = self.word.load(Ordering::Acquire);
            let generation = slot_generation(word);
            if slot_phase(word) != SlotPhase::Running || word & SUSPENDED != 0 {
                return SlotEntry::Silent;
            }
            if word & ACTIVE != 0 {
                let _ = self.close(generation, true);
                return SlotEntry::Silent;
            }
            if self
                .word
                .compare_exchange_weak(word, word | ACTIVE, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return SlotEntry::Entered(generation);
            }
        }
    }

    fn is_retire_ready(&self, generation: u64) -> bool {
        let word = self.word.load(Ordering::Acquire);
        slot_generation(word) == generation
            && slot_phase(word) == SlotPhase::Closing
            && word & ACTIVE == 0
    }

    fn prepare_retirement(
        &self,
        generation: u64,
    ) -> Result<PreparedRetirement, MixerRetirementError> {
        let word = self.word.load(Ordering::Acquire);
        if slot_generation(word) != generation
            || slot_phase(word) != SlotPhase::Closing
            || word & ACTIVE != 0
        {
            return Err(MixerRetirementError::Structural);
        }
        if self
            .word
            .compare_exchange(
                word,
                slot_word(
                    generation,
                    SlotPhase::Retiring,
                    word & (FAILED | HAS_PAYLOAD | SLOT_CAUSE_MASK),
                ),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return Err(MixerRetirementError::Structural);
        }
        // SAFETY: Closing prevents future entry and ACTIVE==0 proves no current
        // renderer owns the payload.
        let source = unsafe { (**self.payload.get()).take() };
        let expected_payload = word & HAS_PAYLOAD != 0;
        let result = if expected_payload == source.is_some() {
            Ok(MixerInputRetirement {
                failed_before_retirement: word & FAILED != 0,
                source_destructor_panicked: false,
                output_failure: decode_slot_failure(word),
            })
        } else {
            Err(MixerRetirementError::Structural)
        };
        let pending_failure_notification = decode_slot_failure(word).filter(|_| {
            self.failure_notified
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        });
        let observer = self.take_failure_observer();
        Ok(PreparedRetirement {
            source,
            observer: ManuallyDrop::new(observer),
            pending_failure_notification,
            result,
        })
    }

    fn finish_reusable(
        &self,
        generation: u64,
        result: Result<MixerInputRetirement, MixerRetirementError>,
    ) -> Result<(MixerInputRetirement, u64), MixerRetirementError> {
        if let Err(error) = result {
            self.word.store(
                slot_word(generation, SlotPhase::Quarantined, FAILED),
                Ordering::Release,
            );
            return Err(error);
        }
        let retirement = result.expect("checked successful retirement");
        let word = self.word.load(Ordering::Acquire);
        if slot_generation(word) != generation || slot_phase(word) != SlotPhase::Retiring {
            self.quarantine_word(word);
            return Err(MixerRetirementError::Structural);
        }
        let Some(next_generation) = generation
            .checked_add(1)
            .filter(|next| *next <= MAX_GENERATION)
        else {
            self.word.store(
                slot_word(generation, SlotPhase::Quarantined, FAILED),
                Ordering::Release,
            );
            return Err(MixerRetirementError::GenerationExhausted);
        };
        self.word.store(
            slot_word(next_generation, SlotPhase::Free, 0),
            Ordering::Release,
        );
        Ok((retirement, next_generation))
    }

    fn finish_terminal(
        &self,
        generation: u64,
        result: Result<MixerInputRetirement, MixerRetirementError>,
    ) -> Result<MixerInputRetirement, MixerRetirementError> {
        let word = self.word.load(Ordering::Acquire);
        let result =
            if slot_generation(word) == generation && slot_phase(word) == SlotPhase::Retiring {
                result
            } else {
                Err(MixerRetirementError::Structural)
            };
        let flags = result.as_ref().map_or(FAILED, |retirement| {
            let failed = if retirement.failed_before_retirement {
                FAILED
            } else {
                0
            };
            failed | retirement.output_failure.map_or(0, slot_failure_cause)
        });
        self.word.store(
            slot_word(
                generation,
                if result.is_ok() {
                    SlotPhase::ForcedClean
                } else {
                    SlotPhase::Quarantined
                },
                flags,
            ),
            Ordering::Release,
        );
        self.finish_forced(generation, result);
        result
    }

    fn prepare_forced_terminal(
        &self,
        quarantine_active: bool,
    ) -> Option<(u64, PreparedRetirement)> {
        let word = self.word.load(Ordering::Acquire);
        let generation = slot_generation(word);
        match slot_phase(word) {
            SlotPhase::Free | SlotPhase::ForcedClean | SlotPhase::Retiring => return None,
            SlotPhase::Reserved | SlotPhase::Installed | SlotPhase::Running => {
                self.force_close_current();
                return self.prepare_forced_terminal(quarantine_active);
            }
            SlotPhase::Closing | SlotPhase::Quarantined => {}
        }
        if word & ACTIVE != 0 {
            if quarantine_active {
                self.force_quarantine();
            }
            return None;
        }
        if self
            .word
            .compare_exchange(
                word,
                slot_word(
                    generation,
                    SlotPhase::Retiring,
                    word & (FAILED | HAS_PAYLOAD | SLOT_CAUSE_MASK),
                ),
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            return self.prepare_forced_terminal(quarantine_active);
        }
        // SAFETY: Closing prevents future entry, ACTIVE is clear, and Retiring
        // excludes all future render entry and payload mutation.
        let source = unsafe { (**self.payload.get()).take() };
        let expected_payload = word & HAS_PAYLOAD != 0;
        let result = if slot_phase(word) == SlotPhase::Quarantined {
            Err(MixerRetirementError::OwnerUncertain)
        } else if expected_payload == source.is_some() {
            Ok(MixerInputRetirement {
                failed_before_retirement: word & FAILED != 0,
                source_destructor_panicked: false,
                output_failure: decode_slot_failure(word),
            })
        } else {
            Err(MixerRetirementError::Structural)
        };
        let pending_failure_notification = decode_slot_failure(word).filter(|_| {
            self.failure_notified
                .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        });
        let observer = self.take_failure_observer();
        Some((
            generation,
            PreparedRetirement {
                source,
                observer: ManuallyDrop::new(observer),
                pending_failure_notification,
                result,
            },
        ))
    }

    fn is_terminal_for_session(&self) -> bool {
        matches!(
            slot_phase(self.word.load(Ordering::Acquire)),
            SlotPhase::Free | SlotPhase::ForcedClean | SlotPhase::Quarantined
        )
    }

    fn fail_live(&self, failure: MixerOutputFailure) -> Option<Arc<dyn MixerFailureObserver>> {
        loop {
            let word = self.word.load(Ordering::Acquire);
            if slot_phase(word) == SlotPhase::Closing {
                if word & FAILED == 0 || decode_slot_failure(word) != Some(failure) {
                    return None;
                }
                if self.failure_notified.swap(true, Ordering::AcqRel) {
                    return None;
                }
                return lock_recover(&self.failure_observer).clone_observer();
            }
            if !matches!(slot_phase(word), SlotPhase::Installed | SlotPhase::Running) {
                return None;
            }
            let next = slot_word(
                slot_generation(word),
                SlotPhase::Closing,
                (word & (ACTIVE | HAS_PAYLOAD)) | FAILED | slot_failure_cause(failure),
            );
            if self
                .word
                .compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                if self.failure_notified.swap(true, Ordering::AcqRel) {
                    return None;
                }
                return lock_recover(&self.failure_observer).clone_observer();
            }
        }
    }

    fn take_failure_observer(&self) -> Option<Arc<dyn MixerFailureObserver>> {
        lock_recover(&self.failure_observer).take()
    }

    fn force_close_current(&self) {
        loop {
            let word = self.word.load(Ordering::Acquire);
            match slot_phase(word) {
                SlotPhase::Free
                | SlotPhase::ForcedClean
                | SlotPhase::Quarantined
                | SlotPhase::Retiring
                | SlotPhase::Closing => return,
                SlotPhase::Reserved | SlotPhase::Installed | SlotPhase::Running => {}
            }
            let next = slot_word(
                slot_generation(word),
                SlotPhase::Closing,
                word & (ACTIVE | FAILED | HAS_PAYLOAD | SLOT_CAUSE_MASK),
            );
            if self
                .word
                .compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    fn force_quarantine(&self) {
        let word = self.word.load(Ordering::Acquire);
        let generation = slot_generation(word);
        self.quarantine_word(word);
        if let Some(observer) = self.take_failure_observer() {
            std::mem::forget(observer);
        }
        self.finish_forced(generation, Err(MixerRetirementError::OwnerUncertain));
    }

    fn finish_forced(
        &self,
        generation: u64,
        result: Result<MixerInputRetirement, MixerRetirementError>,
    ) {
        let (waker, retained) = {
            let mut forced = lock_recover(&self.forced);
            if forced.generation != generation {
                return;
            }
            forced.result = Some(result);
            (forced.waker.take(), forced.retained.take())
        };
        if let Some(waker) = waker {
            forget_panic(catch_unwind(AssertUnwindSafe(|| waker.wake())));
        }
        if let Some(mut retained) = retained {
            // SAFETY: terminal cleanup has resolved the slot, and the caller
            // still owns a temporary strong Arc while this method executes.
            unsafe { ManuallyDrop::drop(&mut retained) };
        }
    }

    fn poll_forced(
        &self,
        generation: u64,
        cx: &mut Context<'_>,
    ) -> Poll<Result<MixerInputRetirement, MixerRetirementError>> {
        let replacement = cx.waker().clone();
        let mut forced = lock_recover(&self.forced);
        if forced.generation != generation {
            return Poll::Ready(Err(MixerRetirementError::Structural));
        }
        if let Some(result) = forced.result {
            return Poll::Ready(result);
        }
        if forced
            .waker
            .as_ref()
            .is_none_or(|existing| !existing.will_wake(&replacement))
        {
            forced.waker = Some(replacement);
        }
        Poll::Pending
    }

    fn quarantine_word(&self, word: u64) {
        self.word.store(
            slot_word(
                slot_generation(word),
                SlotPhase::Quarantined,
                (word & (ACTIVE | HAS_PAYLOAD)) | FAILED,
            ),
            Ordering::Release,
        );
    }
}

enum SlotEntry {
    Silent,
    Entered(u64),
}

struct SlotRenderGuard<'a>(&'a InputSlot);

impl Drop for SlotRenderGuard<'_> {
    fn drop(&mut self) {
        self.0.word.fetch_and(!ACTIVE, Ordering::Release);
    }
}

struct SlotSoundData {
    slot: Weak<InputSlot>,
}

struct SlotSound {
    slot: Weak<InputSlot>,
}

impl SoundData for SlotSoundData {
    type Error = Infallible;
    type Handle = ();

    fn into_sound(self) -> Result<(Box<dyn Sound>, Self::Handle), Self::Error> {
        Ok((Box::new(SlotSound { slot: self.slot }), ()))
    }
}

impl Sound for SlotSound {
    fn process(&mut self, output: &mut [KiraFrame], _dt: f64, _info: &kira::info::Info) {
        output.fill(KiraFrame::ZERO);
        let Some(slot) = self.slot.upgrade() else {
            return;
        };
        let SlotEntry::Entered(generation) = slot.try_enter() else {
            return;
        };
        let _guard = SlotRenderGuard(&slot);
        // SAFETY: exact Running+ACTIVE grants exclusive payload access until
        // `_guard` clears ACTIVE with Release ordering.
        let source = unsafe { &mut **slot.payload.get() };
        let Some(source) = source.as_mut() else {
            let _ = slot.close(generation, true);
            return;
        };
        let mut rendered = [MixerFrame::ZERO; INTERNAL_BUFFER_FRAMES];
        let Some(rendered) = rendered.get_mut(..output.len()) else {
            let _ = slot.close(generation, true);
            return;
        };
        match catch_unwind(AssertUnwindSafe(|| source.render(rendered))) {
            Ok(MixerInputStatus::Active) => {
                copy_mixer_frames(output, rendered);
            }
            Ok(MixerInputStatus::Finished) => {
                copy_mixer_frames(output, rendered);
                let _ = slot.close(generation, false);
            }
            Err(payload) => {
                let _ = slot.close(generation, true);
                std::mem::forget(payload);
            }
        }
    }

    fn finished(&self) -> bool {
        false
    }
}

fn copy_mixer_frames(output: &mut [KiraFrame], rendered: &[MixerFrame]) {
    for (output, rendered) in output.iter_mut().zip(rendered) {
        *output = KiraFrame {
            left: rendered.left,
            right: rendered.right,
        };
    }
}

const DRIVER_PHASE_MASK: u64 = 0x0f;
const DRIVER_CAUSE_SHIFT: u32 = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
enum DriverPhase {
    Provisional = 0,
    Live = 1,
    Closing = 2,
    Failed = 3,
    Retired = 4,
    Uncertain = 5,
}

const fn driver_cause(failure: MixerOutputFailure) -> u64 {
    let cause = match failure {
        MixerOutputFailure::BackendFailure => 1,
        MixerOutputFailure::InvalidCallbackGeometry => 2,
        MixerOutputFailure::RendererPanicked => 3,
        MixerOutputFailure::OwnerPanicked => 4,
        MixerOutputFailure::CallbackStalled => 5,
    };
    cause << DRIVER_CAUSE_SHIFT
}

fn decode_driver_phase(word: u64) -> DriverPhase {
    match word & DRIVER_PHASE_MASK {
        0 => DriverPhase::Provisional,
        1 => DriverPhase::Live,
        2 => DriverPhase::Closing,
        3 => DriverPhase::Failed,
        4 => DriverPhase::Retired,
        _ => DriverPhase::Uncertain,
    }
}

fn decode_driver_cause(word: u64) -> Option<MixerOutputFailure> {
    match word >> DRIVER_CAUSE_SHIFT {
        1 => Some(MixerOutputFailure::BackendFailure),
        2 => Some(MixerOutputFailure::InvalidCallbackGeometry),
        3 => Some(MixerOutputFailure::RendererPanicked),
        4 => Some(MixerOutputFailure::OwnerPanicked),
        5 => Some(MixerOutputFailure::CallbackStalled),
        _ => None,
    }
}

struct DriverStatus {
    word: AtomicU64,
    callback_epoch: AtomicU64,
    #[cfg(any(test, feature = "test-support"))]
    panic_owner: AtomicBool,
    #[cfg(any(test, feature = "test-support"))]
    open_snapshot_hook: Mutex<Option<Arc<OpenSnapshotHook>>>,
    #[cfg(any(test, feature = "test-support"))]
    open_publish_hook: Mutex<Option<Arc<OpenSnapshotHook>>>,
    #[cfg(test)]
    retirement_scan_hook: Mutex<Option<Arc<RetirementScanHook>>>,
    #[cfg(test)]
    session_forced_hook: Mutex<Option<Arc<RetirementScanHook>>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DriverSnapshot {
    phase: DriverPhase,
    failure: Option<MixerOutputFailure>,
}

#[cfg(any(test, feature = "test-support"))]
struct OpenSnapshotHook {
    entered: SyncSender<()>,
    release: Arc<std::sync::Barrier>,
    armed: AtomicBool,
}

#[cfg(test)]
struct RetirementScanHook {
    entered: SyncSender<()>,
    release: Arc<std::sync::Barrier>,
    armed: AtomicBool,
}

impl DriverStatus {
    fn new() -> Self {
        Self {
            word: AtomicU64::new(DriverPhase::Provisional as u64),
            callback_epoch: AtomicU64::new(0),
            #[cfg(any(test, feature = "test-support"))]
            panic_owner: AtomicBool::new(false),
            #[cfg(any(test, feature = "test-support"))]
            open_snapshot_hook: Mutex::new(None),
            #[cfg(any(test, feature = "test-support"))]
            open_publish_hook: Mutex::new(None),
            #[cfg(test)]
            retirement_scan_hook: Mutex::new(None),
            #[cfg(test)]
            session_forced_hook: Mutex::new(None),
        }
    }

    fn snapshot(&self) -> DriverSnapshot {
        let word = self.word.load(Ordering::Acquire);
        DriverSnapshot {
            phase: decode_driver_phase(word),
            failure: decode_driver_cause(word),
        }
    }

    fn is_live(&self) -> bool {
        self.snapshot().phase == DriverPhase::Live
    }

    fn mark_live(&self) -> bool {
        self.word
            .compare_exchange(
                DriverPhase::Provisional as u64,
                DriverPhase::Live as u64,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_ok()
    }

    fn fail(&self, failure: MixerOutputFailure) -> bool {
        loop {
            let word = self.word.load(Ordering::Acquire);
            if !matches!(
                decode_driver_phase(word),
                DriverPhase::Provisional | DriverPhase::Live
            ) {
                return false;
            }
            let next = DriverPhase::Failed as u64 | driver_cause(failure);
            if self
                .word
                .compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return true;
            }
        }
    }

    fn record_render_callback(&self) {
        self.callback_epoch.fetch_add(1, Ordering::Release);
    }

    fn callback_epoch(&self) -> u64 {
        self.callback_epoch.load(Ordering::Acquire)
    }

    fn begin_close(&self) -> bool {
        loop {
            let word = self.word.load(Ordering::Acquire);
            if !matches!(
                decode_driver_phase(word),
                DriverPhase::Provisional | DriverPhase::Live
            ) {
                return false;
            }
            if self
                .word
                .compare_exchange_weak(
                    word,
                    DriverPhase::Closing as u64,
                    Ordering::AcqRel,
                    Ordering::Acquire,
                )
                .is_ok()
            {
                return true;
            }
        }
    }

    fn finish_retirement(&self, joined: bool) {
        loop {
            let word = self.word.load(Ordering::Acquire);
            let phase = decode_driver_phase(word);
            if matches!(phase, DriverPhase::Retired | DriverPhase::Uncertain) {
                return;
            }
            let next = if joined {
                DriverPhase::Retired as u64 | (word & !DRIVER_PHASE_MASK)
            } else {
                DriverPhase::Uncertain as u64 | (word & !DRIVER_PHASE_MASK)
            };
            if self
                .word
                .compare_exchange_weak(word, next, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
            {
                return;
            }
        }
    }

    fn failure(&self) -> Option<MixerOutputFailure> {
        let snapshot = self.snapshot();
        matches!(
            snapshot.phase,
            DriverPhase::Failed | DriverPhase::Retired | DriverPhase::Uncertain
        )
        .then_some(snapshot.failure)
        .flatten()
    }

    #[cfg(any(test, feature = "test-support"))]
    fn pause_after_open_snapshot(&self) {
        let hook = lock_recover(&self.open_snapshot_hook).clone();
        if let Some(hook) = hook
            && hook.armed.swap(false, Ordering::AcqRel)
        {
            let _ = hook.entered.send(());
            hook.release.wait();
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn pause_before_input_publish(&self) {
        let hook = lock_recover(&self.open_publish_hook).clone();
        if let Some(hook) = hook
            && hook.armed.swap(false, Ordering::AcqRel)
        {
            let _ = hook.entered.send(());
            hook.release.wait();
        }
    }

    #[cfg(test)]
    fn pause_after_input_retirement_scan(&self) {
        let hook = lock_recover(&self.retirement_scan_hook).clone();
        if let Some(hook) = hook
            && hook.armed.swap(false, Ordering::AcqRel)
        {
            let _ = hook.entered.send(());
            hook.release.wait();
        }
    }

    #[cfg(test)]
    fn pause_before_session_forced_cleanup(&self) {
        let hook = lock_recover(&self.session_forced_hook).clone();
        if let Some(hook) = hook
            && hook.armed.swap(false, Ordering::AcqRel)
        {
            let _ = hook.entered.send(());
            hook.release.wait();
        }
    }

    fn is_joined_retired(&self) -> bool {
        decode_driver_phase(self.word.load(Ordering::Acquire)) == DriverPhase::Retired
    }

    fn is_uncertain(&self) -> bool {
        decode_driver_phase(self.word.load(Ordering::Acquire)) == DriverPhase::Uncertain
    }
}

#[derive(Clone)]
struct DriverFailureSignal(Weak<DriverStatus>);

impl DriverFailureSignal {
    fn report(&self, failure: MixerOutputFailure) -> bool {
        self.0.upgrade().is_some_and(|status| status.fail(failure))
    }

    #[cfg(feature = "physical-output")]
    fn callback_epoch(&self) -> Option<u64> {
        self.0.upgrade().map(|status| status.callback_epoch())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DriverRenderError {
    InvalidGeometry,
    RendererPanicked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(feature = "physical-output"), allow(dead_code))]
enum DriverMaintenance {
    Continue,
    Terminal(MixerOutputFailure),
}

struct JoinedRenderer {
    renderer: Renderer,
    status: Arc<DriverStatus>,
}

impl JoinedRenderer {
    fn render(&mut self, output: &mut [f32], channels: usize) -> Result<(), DriverRenderError> {
        output.fill(0.0);
        self.status.record_render_callback();
        if !self.status.is_live() {
            return Ok(());
        }
        let frames = output.len().checked_div(channels).unwrap_or(0);
        if channels != 2
            || output.is_empty()
            || !output.len().is_multiple_of(channels)
            || frames > MAX_PHYSICAL_CALLBACK_FRAMES
        {
            self.status
                .fail(MixerOutputFailure::InvalidCallbackGeometry);
            return Err(DriverRenderError::InvalidGeometry);
        }
        let rendered = catch_unwind(AssertUnwindSafe(|| {
            self.renderer.on_start_processing();
            self.renderer.process(output, 2);
        }));
        match rendered {
            Ok(()) => Ok(()),
            Err(payload) => {
                output.fill(0.0);
                self.status.fail(MixerOutputFailure::RendererPanicked);
                std::mem::forget(payload);
                Err(DriverRenderError::RendererPanicked)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CallbackStallPolicy {
    /// The mixer terminalizes a stalled callback after its own grace period.
    MixerManaged,
    /// The driver detects callback stalls and recovers or terminalizes itself.
    DriverManaged,
    /// Callbacks advance only when explicitly driven, so elapsed wall time is
    /// not evidence that output stalled.
    Manual,
}

trait JoinedOutputDriver: Sized {
    type Settings;
    type Error;

    /// Selects exactly one owner for callback-stall detection. Future physical
    /// drivers default to the mixer's fail-closed retirement watchdog.
    const CALLBACK_STALL_POLICY: CallbackStallPolicy = CallbackStallPolicy::MixerManaged;

    fn setup(
        settings: Self::Settings,
        internal_buffer_size: usize,
        failures: DriverFailureSignal,
    ) -> Result<(Self, PhysicalOutputFormat), Self::Error>;
    fn start(&mut self, renderer: JoinedRenderer) -> Result<(), Self::Error>;
    fn play(&mut self) -> Result<(), Self::Error>;
    fn maintain(&mut self, _now: Instant) -> DriverMaintenance {
        DriverMaintenance::Continue
    }
    fn close_and_join(&mut self) -> bool;
}

struct MonitoredBackend<D: JoinedOutputDriver> {
    driver: ManuallyDrop<D>,
    status: Arc<DriverStatus>,
}

struct MonitoredBackendSettings<S> {
    inner: S,
    expected_sample_rate: u32,
    status: Arc<DriverStatus>,
    negotiated: Arc<Mutex<Option<PhysicalOutputFormat>>>,
}
enum MonitoredBackendError<E> {
    Backend(E),
    SampleRateMismatch { expected: u32, actual: u32 },
    UnsupportedFormat(PhysicalOutputFormat),
    FailedDuringStart(MixerOutputFailure),
}

impl<D: JoinedOutputDriver> Backend for MonitoredBackend<D> {
    type Settings = MonitoredBackendSettings<D::Settings>;
    type Error = MonitoredBackendError<D::Error>;

    fn setup(
        settings: Self::Settings,
        internal_buffer_size: usize,
    ) -> Result<(Self, u32), Self::Error> {
        let failures = DriverFailureSignal(Arc::downgrade(&settings.status));
        let (driver, format) = D::setup(settings.inner, internal_buffer_size, failures)
            .map_err(MonitoredBackendError::Backend)?;
        let backend = Self {
            driver: ManuallyDrop::new(driver),
            status: settings.status,
        };
        if format.sample_rate != settings.expected_sample_rate {
            return Err(MonitoredBackendError::SampleRateMismatch {
                expected: settings.expected_sample_rate,
                actual: format.sample_rate,
            });
        }
        if format.channels != 2 {
            return Err(MonitoredBackendError::UnsupportedFormat(format));
        }
        *lock_recover(&settings.negotiated) = Some(format);
        Ok((backend, format.sample_rate))
    }

    fn start(&mut self, renderer: Renderer) -> Result<(), Self::Error> {
        self.driver
            .start(JoinedRenderer {
                renderer,
                status: Arc::clone(&self.status),
            })
            .map_err(MonitoredBackendError::Backend)?;
        self.driver.play().map_err(|error| {
            self.status.fail(MixerOutputFailure::BackendFailure);
            MonitoredBackendError::Backend(error)
        })?;
        if self.status.mark_live() {
            Ok(())
        } else {
            Err(MonitoredBackendError::FailedDuringStart(
                self.status
                    .failure()
                    .unwrap_or(MixerOutputFailure::BackendFailure),
            ))
        }
    }
}

impl<D: JoinedOutputDriver> MonitoredBackend<D> {
    fn maintain(&mut self, now: Instant) -> DriverMaintenance {
        self.driver.maintain(now)
    }
}

impl<D: JoinedOutputDriver> Drop for MonitoredBackend<D> {
    fn drop(&mut self) {
        self.status.begin_close();
        let joined = forget_panic(catch_unwind(AssertUnwindSafe(|| {
            self.driver.close_and_join()
        }))) == Some(true);
        if !joined {
            // Lost callback-retirement proof: retaining the entire driver is
            // the only safe alternative to destroying its renderer/stream
            // authority while a physical callback may still exist.
            self.status.finish_retirement(false);
            return;
        }
        let dropped = forget_panic(catch_unwind(AssertUnwindSafe(|| unsafe {
            ManuallyDrop::drop(&mut self.driver);
        })))
        .is_some();
        self.status.finish_retirement(dropped);
    }
}

struct SlotPool {
    free: Vec<Arc<InputSlot>>,
    inventory: Vec<Weak<InputSlot>>,
}

impl SlotPool {
    fn reserve(&mut self) -> Result<ReservedRecord, MixerMutationError> {
        let slot = self.free.pop().ok_or(MixerMutationError::InputCapacity)?;
        let generation = slot.reserve()?;
        Ok(ReservedRecord { slot, generation })
    }

    fn restore(&mut self, record: ReservedRecord) -> Result<(), MixerMutationError> {
        let word = record.slot.word.load(Ordering::Acquire);
        if slot_phase(word) != SlotPhase::Reserved || slot_generation(word) != record.generation {
            record.slot.quarantine_word(word);
            return Err(MixerMutationError::InternalInvariant);
        }
        let next_generation = record
            .generation
            .checked_add(1)
            .filter(|next| *next <= MAX_GENERATION)
            .ok_or(MixerMutationError::GenerationExhausted)?;
        record.slot.word.store(
            slot_word(next_generation, SlotPhase::Free, 0),
            Ordering::Release,
        );
        self.free.push(record.slot);
        Ok(())
    }
}

struct SessionTrackHandles {
    root: TrackHandle,
    tracks: [TrackHandle; SESSION_BUS_COUNT],
}

struct SessionRetirementState {
    clean: bool,
    failure: Option<MixerSessionRetirementError>,
    inputs_closed: bool,
    tracks_dropped: bool,
    tracks_dropped_at: Option<Instant>,
    callback_stall: Option<CallbackStallObservation>,
}

struct CallbackStallObservation {
    epoch: u64,
    observed_at: Instant,
}

struct SessionTracks {
    handles: Option<SessionTrackHandles>,
    pools: [SlotPool; SESSION_BUS_COUNT],
    key: SessionKey,
    gain: MixerGainState,
    control: Arc<SessionControl>,
    // Minted with this exact generation so terminal owner cleanup can resolve
    // retirement even after it has stopped accepting retirement requests.
    retirement_completion: Option<oneshot::Sender<Result<(), MixerSessionRetirementError>>>,
    retirement: Option<SessionRetirementState>,
}

struct OpenedMixerSession {
    control: Arc<SessionControl>,
    retirement: oneshot::Receiver<Result<(), MixerSessionRetirementError>>,
}

impl SessionTracks {
    fn track_mut(&mut self, bus: SessionBus) -> &mut TrackHandle {
        &mut self
            .handles
            .as_mut()
            .expect("active session lost its track handles")
            .tracks[bus.ordinal()]
    }

    fn root_mut(&mut self) -> &mut TrackHandle {
        &mut self
            .handles
            .as_mut()
            .expect("active session lost its track handles")
            .root
    }

    fn pool_mut(&mut self, bus: SessionBus) -> &mut SlotPool {
        &mut self.pools[bus.ordinal()]
    }
}

struct MixerCore<D: JoinedOutputDriver> {
    manager: Option<AudioManager<MonitoredBackend<D>>>,
    sessions: HashMap<AudioSessionId, SessionTracks>,
    max_sessions: usize,
    inputs_per_bus: usize,
    next_session_generation: u64,
    master_gain: MixerGainState,
    cleanup_clean: bool,
    #[cfg(test)]
    reported_sub_track_count: Option<usize>,
    #[cfg(test)]
    session_track_retirement_timeout: Option<Duration>,
}

impl<D: JoinedOutputDriver> MixerCore<D> {
    fn maintain_output(&mut self, now: Instant) -> DriverMaintenance {
        self.manager.as_mut().map_or(
            DriverMaintenance::Terminal(MixerOutputFailure::BackendFailure),
            |manager| manager.backend_mut().maintain(now),
        )
    }

    fn with_limits(
        backend_settings: D::Settings,
        driver_status: Arc<DriverStatus>,
        format: MixerFormat,
        max_sessions: usize,
        inputs_per_bus: usize,
    ) -> Result<(Self, PhysicalOutputFormat), MixerStartError<D::Error>> {
        let retirement_status = Arc::clone(&driver_status);
        let negotiated = Arc::new(Mutex::new(None));
        let manager = AudioManager::new(AudioManagerSettings {
            capacities: Capacities {
                sub_track_capacity: max_sessions,
                send_track_capacity: 0,
                clock_capacity: 0,
                modulator_capacity: 0,
                listener_capacity: 0,
            },
            main_track_builder: MainTrackBuilder::new().sound_capacity(0),
            internal_buffer_size: INTERNAL_BUFFER_FRAMES,
            backend_settings: MonitoredBackendSettings {
                inner: backend_settings,
                expected_sample_rate: format.sample_rate,
                status: driver_status,
                negotiated: Arc::clone(&negotiated),
            },
        })
        .map_err(|error| {
            let cause = match error {
                MonitoredBackendError::Backend(error) => MixerStartupFailure::Backend(error),
                MonitoredBackendError::SampleRateMismatch { expected, actual } => {
                    MixerStartupFailure::SampleRateMismatch { expected, actual }
                }
                MonitoredBackendError::UnsupportedFormat(format) => {
                    MixerStartupFailure::UnsupportedPhysicalFormat(format)
                }
                MonitoredBackendError::FailedDuringStart(failure) => {
                    MixerStartupFailure::DriverFailed(failure)
                }
            };
            startup_error(cause, retirement_status.is_uncertain())
        })?;
        let negotiated = *lock_recover(&negotiated);
        let negotiated = negotiated.ok_or(MixerStartError::DriverFailed(
            MixerOutputFailure::BackendFailure,
        ))?;
        Ok((
            Self {
                manager: Some(manager),
                sessions: HashMap::with_capacity(max_sessions),
                max_sessions,
                inputs_per_bus,
                next_session_generation: 1,
                master_gain: MixerGainState::default(),
                cleanup_clean: true,
                #[cfg(test)]
                reported_sub_track_count: None,
                #[cfg(test)]
                session_track_retirement_timeout: None,
            },
            negotiated,
        ))
    }

    fn add_session(
        &mut self,
        id: AudioSessionId,
    ) -> Result<OpenedMixerSession, MixerMutationError> {
        if let Some(session) = self.sessions.get(&id) {
            return Err(
                if session.retirement.is_some() || session.control.is_closing() {
                    MixerMutationError::SessionRetirementPending
                } else {
                    MixerMutationError::DuplicateSession
                },
            );
        }
        if self.sessions.len() == self.max_sessions {
            return Err(
                if self
                    .sessions
                    .values()
                    .any(|session| session.retirement.is_some() || session.control.is_closing())
                {
                    MixerMutationError::SessionRetirementPending
                } else {
                    MixerMutationError::SessionCapacity
                },
            );
        }
        let generation = self.next_session_generation;
        self.next_session_generation = generation
            .checked_add(1)
            .ok_or(MixerMutationError::GenerationExhausted)?;
        let key = SessionKey { id, generation };
        let session_control = Arc::new(SessionControl::new(key));
        let manager = self
            .manager
            .as_mut()
            .ok_or(MixerMutationError::InternalInvariant)?;
        let mut root = manager
            .add_sub_track(
                TrackBuilder::new()
                    .sound_capacity(0)
                    .sub_track_capacity(SESSION_BUS_COUNT),
            )
            .map_err(|_| MixerMutationError::InternalInvariant)?;

        let mut build_bus =
            |bus: SessionBus| -> Result<(TrackHandle, SlotPool), MixerMutationError> {
                let mut track = root
                    .add_sub_track(
                        TrackBuilder::new()
                            .sound_capacity(self.inputs_per_bus)
                            .sub_track_capacity(0),
                    )
                    .map_err(|_| MixerMutationError::InternalInvariant)?;
                let mut free = Vec::with_capacity(self.inputs_per_bus);
                let mut inventory = Vec::with_capacity(self.inputs_per_bus);
                for index in 0..self.inputs_per_bus {
                    let slot = Arc::new(InputSlot::new(SlotAddress {
                        session: key,
                        bus,
                        index,
                    }));
                    track
                        .play(SlotSoundData {
                            slot: Arc::downgrade(&slot),
                        })
                        .map_err(|_| MixerMutationError::InternalInvariant)?;
                    inventory.push(Arc::downgrade(&slot));
                    free.push(slot);
                }
                Ok((track, SlotPool { free, inventory }))
            };

        let (script, script_pool) = build_bus(SessionBus::Script)?;
        let (native, native_pool) = build_bus(SessionBus::Native)?;
        let (speech, speech_pool) = build_bus(SessionBus::Speech)?;
        let (retirement_completion, retirement) = oneshot::channel();
        self.sessions.insert(
            id,
            SessionTracks {
                handles: Some(SessionTrackHandles {
                    root,
                    tracks: [script, native, speech],
                }),
                pools: [script_pool, native_pool, speech_pool],
                key,
                gain: MixerGainState::default(),
                control: Arc::clone(&session_control),
                retirement_completion: Some(retirement_completion),
                retirement: None,
            },
        );
        Ok(OpenedMixerSession {
            control: session_control,
            retirement,
        })
    }

    fn reserve(
        &mut self,
        key: SessionKey,
        bus: SessionBus,
    ) -> Result<ReservedRecord, MixerMutationError> {
        self.sessions
            .get_mut(&key.id)
            .filter(|session| session.key == key && session.retirement.is_none())
            .ok_or(MixerMutationError::UnknownSession)?
            .pool_mut(bus)
            .reserve()
    }

    fn restore_reserved(&mut self, record: ReservedRecord) -> Result<(), MixerMutationError> {
        let address = record.slot.address;
        self.sessions
            .get_mut(&address.session.id)
            .filter(|session| session.key == address.session)
            .ok_or(MixerMutationError::UnknownSession)?
            .pool_mut(address.bus)
            .restore(record)
    }

    fn recycle(
        &mut self,
        record: ReservedRecord,
        next_generation: u64,
    ) -> Result<(), MixerRetirementError> {
        let address = record.slot.address;
        let word = record.slot.word.load(Ordering::Acquire);
        if slot_generation(word) != next_generation || slot_phase(word) != SlotPhase::Free {
            record.slot.quarantine_word(word);
            return Err(MixerRetirementError::Structural);
        }
        let Some(session) = self
            .sessions
            .get_mut(&address.session.id)
            .filter(|session| session.key == address.session)
        else {
            record.slot.quarantine_word(word);
            return Err(MixerRetirementError::Structural);
        };
        session.pool_mut(address.bus).free.push(record.slot);
        Ok(())
    }

    fn set_gain(
        &mut self,
        key: SessionKey,
        bus: SessionBus,
        linear: f32,
    ) -> Result<(), MixerMutationError> {
        let session = self
            .sessions
            .get_mut(&key.id)
            .filter(|session| session.key == key && session.retirement.is_none())
            .ok_or(MixerMutationError::UnknownSession)?;
        session
            .track_mut(bus)
            .set_volume(linear_to_decibels(linear), immediate_tween());
        Ok(())
    }

    fn update_master_gain(
        &mut self,
        update: MixerGainUpdate,
    ) -> Result<MixerGainState, MixerMutationError> {
        let state = update.apply(self.master_gain);
        self.manager
            .as_mut()
            .ok_or(MixerMutationError::InternalInvariant)?
            .main_track()
            .set_volume(
                linear_to_decibels(state.effective_linear()),
                immediate_tween(),
            );
        self.master_gain = state;
        Ok(state)
    }

    fn update_session_gain(
        &mut self,
        key: SessionKey,
        update: MixerGainUpdate,
    ) -> Result<MixerGainState, MixerMutationError> {
        let session = self
            .sessions
            .get_mut(&key.id)
            .filter(|session| session.key == key && session.retirement.is_none())
            .ok_or(MixerMutationError::UnknownSession)?;
        let state = update.apply(session.gain);
        session.root_mut().set_volume(
            linear_to_decibels(state.effective_linear()),
            immediate_tween(),
        );
        session.gain = state;
        Ok(state)
    }

    fn close_all_slots(&self) {
        self.for_each_slot(|slot| slot.force_close_current());
    }

    fn begin_session_retirement(
        &mut self,
        request: SessionRetirementRequest,
    ) -> Result<(), SessionRetirementRequest> {
        let Some(session) = self
            .sessions
            .get_mut(&request.key.id)
            .filter(|session| session.key == request.key)
        else {
            return Err(request);
        };
        if session.retirement.is_some() {
            return Err(request);
        }
        session.retirement = Some(SessionRetirementState {
            clean: session.control.cleanup_clean.load(Ordering::Acquire),
            failure: None,
            inputs_closed: false,
            tracks_dropped: false,
            tracks_dropped_at: None,
            callback_stall: None,
        });
        Ok(())
    }

    fn session_control(&self, key: SessionKey) -> Option<Arc<SessionControl>> {
        self.sessions
            .get(&key.id)
            .filter(|session| session.key == key)
            .map(|session| Arc::clone(&session.control))
    }

    fn mark_session_cleanup_failed(&mut self, key: SessionKey) {
        let Some(session) = self
            .sessions
            .get_mut(&key.id)
            .filter(|session| session.key == key)
        else {
            return;
        };
        session
            .control
            .cleanup_clean
            .store(false, Ordering::Release);
        if let Some(retirement) = session.retirement.as_mut() {
            retirement.clean = false;
        }
    }

    fn close_drained_session_inputs(&mut self) {
        for session in self.sessions.values_mut() {
            let Some(retirement) = session.retirement.as_mut() else {
                continue;
            };
            if retirement.inputs_closed || !session.control.starts_drained() {
                continue;
            }
            for pool in &session.pools {
                for slot in &pool.inventory {
                    if let Some(slot) = slot.upgrade() {
                        slot.force_close_current();
                    }
                }
            }
            retirement.inputs_closed = true;
        }
    }

    fn prepare_session_forced_jobs(&self, pending: &[RetirementRequest]) -> Vec<CleanupJob> {
        let mut jobs = Vec::new();
        for session in self.sessions.values().filter(|session| {
            session
                .retirement
                .as_ref()
                .is_some_and(|retirement| retirement.inputs_closed)
        }) {
            for pool in &session.pools {
                for slot in &pool.inventory {
                    let Some(slot) = slot.upgrade() else {
                        continue;
                    };
                    if slot.is_terminal_for_session() {
                        continue;
                    }
                    if pending.iter().any(|request| {
                        request.record.generation
                            == slot_generation(slot.word.load(Ordering::Acquire))
                            && Arc::ptr_eq(&request.record.slot, &slot)
                    }) {
                        continue;
                    }
                    if let Some((generation, prepared)) = slot.prepare_forced_terminal(false) {
                        jobs.push(CleanupJob {
                            record: ReservedRecord { slot, generation },
                            prepared,
                            completion: None,
                            terminal: true,
                        });
                    }
                }
            }
        }
        jobs
    }

    fn drop_ready_session_tracks(&mut self) {
        for session in self
            .sessions
            .values_mut()
            .filter(|session| session.retirement.is_some())
        {
            if !session
                .retirement
                .as_ref()
                .is_some_and(|retirement| retirement.inputs_closed)
            {
                continue;
            }
            let inputs_terminal = session.pools.iter().all(|pool| {
                pool.inventory.iter().all(|slot| {
                    slot.upgrade()
                        .is_none_or(|slot| slot.is_terminal_for_session())
                })
            });
            if !inputs_terminal {
                continue;
            }
            let Some(handles) = session.handles.take() else {
                continue;
            };
            drop(handles);
            #[cfg(test)]
            session
                .control
                .tracks_dropped
                .store(true, Ordering::Release);
            session
                .retirement
                .as_mut()
                .expect("filtered retiring session")
                .tracks_dropped = true;
            session
                .retirement
                .as_mut()
                .expect("filtered retiring session")
                .tracks_dropped_at = Some(Instant::now());
            session
                .retirement
                .as_mut()
                .expect("filtered retiring session")
                .callback_stall = None;
        }
    }

    fn rendered_session_retirement_timeout(
        #[cfg(test)] configured_timeout: Option<Duration>,
    ) -> Option<Duration> {
        if D::CALLBACK_STALL_POLICY == CallbackStallPolicy::DriverManaged {
            return None;
        }
        #[cfg(test)]
        if let Some(timeout) = configured_timeout {
            return Some(timeout);
        }
        match D::CALLBACK_STALL_POLICY {
            CallbackStallPolicy::MixerManaged => Some(SESSION_TRACK_RETIREMENT_TIMEOUT),
            CallbackStallPolicy::DriverManaged | CallbackStallPolicy::Manual => None,
        }
    }

    fn complete_rendered_session_retirements(&mut self, driver_status: &DriverStatus) -> bool {
        let active_track_count = self
            .sessions
            .values()
            .filter(|session| session.handles.is_some())
            .count();
        let Some(manager) = self.manager.as_ref() else {
            return false;
        };
        #[cfg(test)]
        let reported_track_count = self
            .reported_sub_track_count
            .unwrap_or_else(|| manager.num_sub_tracks());
        #[cfg(not(test))]
        let reported_track_count = manager.num_sub_tracks();
        if reported_track_count > active_track_count {
            let now = Instant::now();
            let retirement_timeout = Self::rendered_session_retirement_timeout(
                #[cfg(test)]
                self.session_track_retirement_timeout,
            );
            let callback_epoch = driver_status.callback_epoch();
            let callback_stalled = self.sessions.values_mut().any(|session| {
                let Some(retirement) = session.retirement.as_mut() else {
                    return false;
                };
                let Some(dropped_at) = retirement.tracks_dropped_at else {
                    return false;
                };
                let Some(timeout) = retirement_timeout else {
                    return false;
                };
                if now.saturating_duration_since(dropped_at) < timeout {
                    return false;
                }
                let Some(observation) = retirement.callback_stall.as_mut() else {
                    // The owner and device callback can both be paused across
                    // sleep/resume. Begin a fresh grace period after the owner
                    // wakes instead of treating the old wall-clock gap as
                    // immediate proof that the stream died.
                    retirement.callback_stall = Some(CallbackStallObservation {
                        epoch: callback_epoch,
                        observed_at: now,
                    });
                    return false;
                };
                if observation.epoch != callback_epoch {
                    observation.epoch = callback_epoch;
                    observation.observed_at = now;
                    return false;
                }
                now.saturating_duration_since(observation.observed_at) >= timeout
            });
            if callback_stalled {
                // Kira removes dropped tracks only while its renderer advances.
                // A device that stops invoking callbacks must therefore
                // terminalize the shared output instead of leaving this
                // session's retirement future pending forever.
                self.cleanup_clean = false;
                driver_status.fail(MixerOutputFailure::CallbackStalled);
                return false;
            }
            return true;
        }
        if reported_track_count < active_track_count {
            self.cleanup_clean = false;
            for retirement in self
                .sessions
                .values_mut()
                .filter_map(|session| session.retirement.as_mut())
            {
                retirement.clean = false;
                retirement.failure = Some(MixerSessionRetirementError::Structural);
            }
            return false;
        }
        let retired = self
            .sessions
            .iter()
            .filter_map(|(id, session)| {
                session
                    .retirement
                    .as_ref()
                    .is_some_and(|retirement| retirement.tracks_dropped)
                    .then_some(*id)
            })
            .collect::<Vec<_>>();
        for id in retired {
            let Some(mut session) = self.sessions.remove(&id) else {
                continue;
            };
            let retirement = session
                .retirement
                .take()
                .expect("retired session lost completion authority");
            let result = if let Some(error) = retirement.failure {
                Err(error)
            } else if retirement.clean {
                Ok(())
            } else {
                Err(MixerSessionRetirementError::CleanupFailed)
            };
            let completion = session
                .retirement_completion
                .take()
                .expect("retired session lost completion authority");
            send_session_completion(completion, result);
        }
        true
    }

    fn finish_session_retirements_after_backend(&mut self, backend_clean: bool) {
        let sessions = std::mem::take(&mut self.sessions);
        for (_, mut session) in sessions {
            let retirement = session.retirement.take();
            let failure = retirement
                .as_ref()
                .and_then(|retirement| retirement.failure);
            let clean = retirement.as_ref().map_or(
                session.control.cleanup_clean.load(Ordering::Acquire),
                |retirement| retirement.clean,
            );
            let result = if let Some(error) = failure {
                Err(error)
            } else if !backend_clean {
                Err(MixerSessionRetirementError::OwnerUncertain)
            } else if clean {
                Ok(())
            } else {
                Err(MixerSessionRetirementError::CleanupFailed)
            };
            let completion = session
                .retirement_completion
                .take()
                .expect("terminal session lost completion authority");
            send_session_completion(completion, result);
        }
    }

    fn prepare_forced_jobs(&self) -> Vec<CleanupJob> {
        let mut jobs = Vec::new();
        self.for_each_slot(|slot| {
            if let Some((generation, prepared)) = slot.prepare_forced_terminal(true) {
                jobs.push(CleanupJob {
                    record: ReservedRecord {
                        slot: Arc::clone(slot),
                        generation,
                    },
                    prepared,
                    completion: None,
                    terminal: true,
                });
            }
        });
        jobs
    }

    fn force_quarantine_all(&self) {
        self.for_each_slot(|slot| slot.force_quarantine());
    }

    fn for_each_slot(&self, mut action: impl FnMut(&Arc<InputSlot>)) {
        for session in self.sessions.values() {
            for pool in &session.pools {
                for slot in &pool.inventory {
                    if let Some(slot) = slot.upgrade() {
                        action(&slot);
                    }
                }
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MixerMutationError {
    DriverStopped,
    DuplicateSession,
    SessionCapacity,
    SessionRetirementPending,
    UnknownSession,
    InputCapacity,
    GenerationExhausted,
    InternalInvariant,
}

struct ReservedRecord {
    slot: Arc<InputSlot>,
    generation: u64,
}

struct RetirementRequest {
    record: ReservedRecord,
    completion: oneshot::Sender<Result<MixerInputRetirement, MixerRetirementError>>,
}

struct SessionRetirementRequest {
    key: SessionKey,
}

enum SessionRetirementSubmission {
    Queued,
    // The exact generation's pre-minted completion will be resolved by
    // terminal cleanup or canceled if the owner exits without proof.
    AwaitCompletion,
}

struct CleanupJob {
    record: ReservedRecord,
    prepared: PreparedRetirement,
    completion: Option<oneshot::Sender<Result<MixerInputRetirement, MixerRetirementError>>>,
    terminal: bool,
}

struct CleanupResult {
    record: ReservedRecord,
    result: Result<MixerInputRetirement, MixerRetirementError>,
    completion: Option<oneshot::Sender<Result<MixerInputRetirement, MixerRetirementError>>>,
    terminal: bool,
}

struct FailureNotification {
    observer: Arc<dyn MixerFailureObserver>,
    failure: MixerOutputFailure,
}

enum CleanupTask {
    Retire(CleanupJob),
    Notify(FailureNotification),
}

enum OwnerCommand {
    AddSession(
        AudioSessionId,
        SyncSender<Result<OpenedMixerSession, MixerMutationError>>,
    ),
    Reserve(
        SessionKey,
        SessionBus,
        SyncSender<Result<ReservedRecord, MixerMutationError>>,
    ),
    SetGain(
        SessionKey,
        SessionBus,
        f32,
        SyncSender<Result<(), MixerMutationError>>,
    ),
    UpdateMasterGain(
        MixerGainUpdate,
        SyncSender<Result<MixerGainState, MixerMutationError>>,
    ),
    UpdateSessionGain(
        SessionKey,
        MixerGainUpdate,
        SyncSender<Result<MixerGainState, MixerMutationError>>,
    ),
    Shutdown,
}

struct GateState {
    production_sealed: bool,
    accepting_input_retirements: bool,
    accepting_session_retirements: bool,
    start_admissions: usize,
}

struct SessionGateState {
    start_admissions: usize,
}

struct SessionControl {
    key: SessionKey,
    gate: Mutex<SessionGateState>,
    closing: AtomicBool,
    drained: Condvar,
    cleanup_clean: AtomicBool,
    #[cfg(test)]
    tracks_dropped: AtomicBool,
    #[cfg(test)]
    cleanup_finish_hook: Mutex<Option<Arc<RetirementScanHook>>>,
    #[cfg(test)]
    request_enqueued_hook: Mutex<Option<Arc<RetirementScanHook>>>,
    #[cfg(test)]
    input_closed_hook: Mutex<Option<Arc<RetirementScanHook>>>,
}

impl SessionControl {
    fn new(key: SessionKey) -> Self {
        Self {
            key,
            gate: Mutex::new(SessionGateState {
                start_admissions: 0,
            }),
            closing: AtomicBool::new(false),
            drained: Condvar::new(),
            cleanup_clean: AtomicBool::new(true),
            #[cfg(test)]
            tracks_dropped: AtomicBool::new(false),
            #[cfg(test)]
            cleanup_finish_hook: Mutex::new(None),
            #[cfg(test)]
            request_enqueued_hook: Mutex::new(None),
            #[cfg(test)]
            input_closed_hook: Mutex::new(None),
        }
    }

    fn is_closing(&self) -> bool {
        self.closing.load(Ordering::Acquire)
    }

    fn begin_close(&self) -> Result<(), MixerSessionRetirementError> {
        let gate = self
            .gate
            .lock()
            .map_err(|_| MixerSessionRetirementError::OwnerUncertain)?;
        self.closing.store(true, Ordering::Release);
        drop(gate);
        Ok(())
    }

    fn starts_drained(&self) -> bool {
        lock_recover(&self.gate).start_admissions == 0
    }

    #[cfg(test)]
    fn pause_before_cleanup_finish(&self) {
        let hook = lock_recover(&self.cleanup_finish_hook).clone();
        if let Some(hook) = hook
            && hook.armed.swap(false, Ordering::AcqRel)
        {
            let _ = hook.entered.send(());
            hook.release.wait();
        }
    }

    #[cfg(test)]
    fn pause_after_request_enqueued(&self) {
        let hook = lock_recover(&self.request_enqueued_hook).clone();
        if let Some(hook) = hook
            && hook.armed.swap(false, Ordering::AcqRel)
        {
            let _ = hook.entered.send(());
            hook.release.wait();
        }
    }
}

struct ControlInner {
    gate: Mutex<GateState>,
    gate_drained: Condvar,
    commands: SyncSender<OwnerCommand>,
    retirements: SyncSender<RetirementRequest>,
    session_retirements: SyncSender<SessionRetirementRequest>,
    format: MixerFormat,
    driver_status: Arc<DriverStatus>,
}

impl ControlInner {
    fn admit_start(
        self: &Arc<Self>,
        session: &Arc<SessionControl>,
    ) -> Result<StartAdmission, MixerControlError> {
        let mut session_gate = session
            .gate
            .lock()
            .map_err(|_| MixerControlError::OwnerStopped)?;
        if session.is_closing() {
            return Err(MixerControlError::UnknownSession);
        }
        let mut gate = self
            .gate
            .lock()
            .map_err(|_| MixerControlError::OwnerStopped)?;
        if gate.production_sealed || !self.driver_status.is_live() {
            return Err(MixerControlError::OwnerStopped);
        }
        let next_global_admissions = gate
            .start_admissions
            .checked_add(1)
            .ok_or(MixerControlError::InternalInvariant)?;
        let next_session_admissions = session_gate
            .start_admissions
            .checked_add(1)
            .ok_or(MixerControlError::InternalInvariant)?;
        gate.start_admissions = next_global_admissions;
        session_gate.start_admissions = next_session_admissions;
        drop(gate);
        drop(session_gate);
        Ok(StartAdmission {
            control: Arc::clone(self),
            session: Arc::clone(session),
        })
    }

    fn submit_retirement(
        &self,
        request: RetirementRequest,
    ) -> Result<(), (MixerRetirementError, RetirementRequest)> {
        let Ok(gate) = self.gate.lock() else {
            return Err((MixerRetirementError::OwnerUncertain, request));
        };
        if !gate.accepting_input_retirements {
            return Err((MixerRetirementError::OwnerUncertain, request));
        }
        let result = match self.retirements.try_send(request) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(request)) => {
                Err((MixerRetirementError::QueueInvariant, request))
            }
            Err(TrySendError::Disconnected(request)) => {
                Err((MixerRetirementError::OwnerUncertain, request))
            }
        };
        drop(gate);
        result
    }

    fn submit_session_retirement(
        &self,
        request: SessionRetirementRequest,
    ) -> Result<SessionRetirementSubmission, MixerSessionRetirementError> {
        let Ok(gate) = self.gate.lock() else {
            return Err(MixerSessionRetirementError::OwnerUncertain);
        };
        if !gate.accepting_session_retirements {
            return Ok(SessionRetirementSubmission::AwaitCompletion);
        }
        let result = match self.session_retirements.try_send(request) {
            Ok(()) => Ok(SessionRetirementSubmission::Queued),
            Err(TrySendError::Full(_)) => Err(MixerSessionRetirementError::QueueInvariant),
            Err(TrySendError::Disconnected(_)) => Ok(SessionRetirementSubmission::AwaitCompletion),
        };
        drop(gate);
        result
    }

    fn seal_production(&self) -> Result<(), MixerControlError> {
        let mut gate = self
            .gate
            .lock()
            .map_err(|_| MixerControlError::OwnerStopped)?;
        gate.production_sealed = true;
        while gate.start_admissions != 0 {
            gate = self
                .gate_drained
                .wait(gate)
                .map_err(|_| MixerControlError::OwnerStopped)?;
        }
        Ok(())
    }

    fn stop_retirement_acceptance(&self) {
        let mut gate = lock_recover(&self.gate);
        gate.accepting_input_retirements = false;
        gate.accepting_session_retirements = false;
    }
}

struct StartAdmission {
    control: Arc<ControlInner>,
    session: Arc<SessionControl>,
}

impl Drop for StartAdmission {
    fn drop(&mut self) {
        let mut gate = lock_recover(&self.control.gate);
        debug_assert!(gate.start_admissions > 0);
        gate.start_admissions = gate.start_admissions.saturating_sub(1);
        if gate.start_admissions == 0 {
            self.control.gate_drained.notify_all();
        }
        drop(gate);
        let mut session_gate = lock_recover(&self.session.gate);
        debug_assert!(session_gate.start_admissions > 0);
        session_gate.start_admissions = session_gate.start_admissions.saturating_sub(1);
        if session_gate.start_admissions == 0 {
            self.session.drained.notify_all();
        }
    }
}

struct OwnerStatus {
    retired: AtomicBool,
    clean: AtomicBool,
}

impl OwnerStatus {
    fn new() -> Self {
        Self {
            retired: AtomicBool::new(false),
            clean: AtomicBool::new(false),
        }
    }
}

/// Errors reported by the bounded mixer control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixerControlError {
    /// The fixed command queue is currently full.
    Saturated,
    /// The mixer owner has already stopped or begun sealing.
    OwnerStopped,
    /// The session identity is already present.
    DuplicateSession,
    /// The fixed session capacity has been reached.
    SessionCapacity,
    /// A session is closing and Kira has not yet acknowledged track removal.
    ///
    /// Retry after awaiting the exact [`MixerSessionRetirement`] receipt.
    SessionRetirementPending,
    /// The requested session does not exist.
    UnknownSession,
    /// The selected bus has reached its fixed input capacity.
    InputCapacity,
    /// A finite nonnegative linear gain was not supplied.
    InvalidGain,
    /// A non-wrapping identity was exhausted.
    GenerationExhausted,
    /// A fixed-capacity invariant inside the prebuilt topology failed.
    InternalInvariant,
}

impl From<MixerMutationError> for MixerControlError {
    fn from(value: MixerMutationError) -> Self {
        match value {
            MixerMutationError::DuplicateSession => Self::DuplicateSession,
            MixerMutationError::DriverStopped => Self::OwnerStopped,
            MixerMutationError::SessionCapacity => Self::SessionCapacity,
            MixerMutationError::SessionRetirementPending => Self::SessionRetirementPending,
            MixerMutationError::UnknownSession => Self::UnknownSession,
            MixerMutationError::InputCapacity => Self::InputCapacity,
            MixerMutationError::GenerationExhausted => Self::GenerationExhausted,
            MixerMutationError::InternalInvariant => Self::InternalInvariant,
        }
    }
}

/// Failure to prove exact retirement of one mixer session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixerSessionRetirementError {
    /// The owner or retirement channel disappeared without a clean terminal proof.
    OwnerUncertain,
    /// A queue invariant failed; the exact session remains sealed fail-closed.
    QueueInvariant,
    /// Logical inputs were made permanently non-enterable, but cleanup was unclean.
    CleanupFailed,
    /// The exact session generation was no longer owned by the mixer.
    Structural,
}

/// Primary driver or format cause of a mixer startup failure.
#[derive(Debug)]
pub enum MixerStartupFailure<E> {
    /// The selected physical output driver could not be created or started.
    Backend(E),
    /// The physical output's actual sample rate did not match the fixed contract.
    SampleRateMismatch {
        /// Requested and published rate.
        expected: u32,
        /// Rate returned by physical-driver setup.
        actual: u32,
    },
    /// The negotiated physical format cannot carry the fixed stereo mixer.
    UnsupportedPhysicalFormat(PhysicalOutputFormat),
    /// The physical output failed during provisional startup.
    DriverFailed(MixerOutputFailure),
}

fn startup_error<E>(cause: MixerStartupFailure<E>, cleanup_uncertain: bool) -> MixerStartError<E> {
    if cleanup_uncertain {
        return MixerStartError::CleanupUncertain(cause);
    }
    match cause {
        MixerStartupFailure::Backend(error) => MixerStartError::Backend(error),
        MixerStartupFailure::SampleRateMismatch { expected, actual } => {
            MixerStartError::SampleRateMismatch { expected, actual }
        }
        MixerStartupFailure::UnsupportedPhysicalFormat(format) => {
            MixerStartError::UnsupportedPhysicalFormat(format)
        }
        MixerStartupFailure::DriverFailed(failure) => MixerStartError::DriverFailed(failure),
    }
}

/// Failure to construct the dedicated mixer owner.
#[derive(Debug)]
pub enum MixerStartError<E> {
    /// The requested fixed mixer sample rate was zero.
    InvalidSampleRate,
    /// The owner thread could not be created.
    Thread(std::io::Error),
    /// The selected physical output driver could not be created or started.
    Backend(E),
    /// The physical output's actual sample rate did not match the fixed contract.
    SampleRateMismatch {
        /// Requested and published rate.
        expected: u32,
        /// Rate returned by physical-driver setup.
        actual: u32,
    },
    /// The negotiated physical format cannot carry the fixed stereo mixer.
    UnsupportedPhysicalFormat(PhysicalOutputFormat),
    /// The physical output failed during provisional startup.
    DriverFailed(MixerOutputFailure),
    /// Startup failed and callback/driver retirement could not be proven.
    CleanupUncertain(MixerStartupFailure<E>),
    /// The owner terminated before reporting its startup result.
    OwnerStopped,
}

impl<E: fmt::Display + fmt::Debug> fmt::Display for MixerStartError<E> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidSampleRate => formatter.write_str("the mixer sample rate is invalid"),
            Self::Thread(error) => {
                write!(formatter, "the mixer owner thread could not start: {error}")
            }
            Self::Backend(error) => write!(formatter, "physical output could not start: {error}"),
            Self::SampleRateMismatch { expected, actual } => write!(
                formatter,
                "physical output selected {actual} Hz instead of the required {expected} Hz"
            ),
            Self::UnsupportedPhysicalFormat(format) => {
                write!(
                    formatter,
                    "physical output format is unsupported: {format:?}"
                )
            }
            Self::DriverFailed(failure) => {
                write!(
                    formatter,
                    "physical output failed during startup: {failure:?}"
                )
            }
            Self::CleanupUncertain(cause) => {
                write!(
                    formatter,
                    "physical output startup cleanup is uncertain: {cause:?}"
                )
            }
            Self::OwnerStopped => formatter.write_str("the mixer owner stopped during startup"),
        }
    }
}

impl<E> std::error::Error for MixerStartError<E> where E: std::error::Error + 'static {}

/// Result of explicitly joining the mixer owner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixerShutdown {
    /// Whether physical-driver retirement and forced input cleanup were proven.
    pub clean: bool,
    /// First operational output failure, independent of cleanup proof.
    pub failure: Option<MixerOutputFailure>,
}

/// Result of an off-render mixer-input retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MixerInputRetirement {
    /// Whether rendering or the source callback had failed closed.
    pub failed_before_retirement: bool,
    /// Whether the contained source's destructor panicked.
    pub source_destructor_panicked: bool,
    /// Global output cause published to this exact input, if any.
    pub output_failure: Option<MixerOutputFailure>,
}

impl MixerInputRetirement {
    /// Returns `true` when rendering and source destruction were both clean.
    #[must_use]
    pub const fn is_clean(self) -> bool {
        !self.failed_before_retirement && !self.source_destructor_panicked
    }
}

/// Failure to prove or complete one logical input retirement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MixerRetirementError {
    /// The fixed ownership queue invariant was violated.
    QueueInvariant,
    /// The mixer owner or backend retired without a clean proof.
    OwnerUncertain,
    /// A generation or phase invariant was violated.
    Structural,
    /// The non-wrapping slot generation was exhausted.
    GenerationExhausted,
    /// The source destructor panicked; the slot was quarantined.
    SourceDestructorPanicked,
}

enum ShutdownState {
    Accepted(oneshot::Receiver<Result<MixerInputRetirement, MixerRetirementError>>),
    Forced {
        slot: ManuallyDrop<Arc<InputSlot>>,
        generation: u64,
    },
    RetainedError {
        _slot: ManuallyDrop<Arc<InputSlot>>,
        error: MixerRetirementError,
    },
    Finished,
}

/// Single-owner future for an already-committed logical input retirement.
#[must_use = "mixer input retirement must be polled to observe its outcome"]
pub struct MixerInputShutdown {
    state: ShutdownState,
}

impl Drop for MixerInputShutdown {
    fn drop(&mut self) {
        let state = std::mem::replace(&mut self.state, ShutdownState::Finished);
        let ShutdownState::Forced {
            mut slot,
            generation,
        } = state
        else {
            return;
        };
        // SAFETY: replacing the state transfers this exact strong authority.
        let slot = unsafe { ManuallyDrop::take(&mut slot) };
        let mut forced = lock_recover(&slot.forced);
        let mut duplicate = false;
        if forced.generation == generation && forced.result.is_none() {
            if forced.retained.is_none() {
                forced.retained = Some(ManuallyDrop::new(Arc::clone(&slot)));
            } else {
                // An impossible duplicate forced owner is retained rather than
                // risking premature slot destruction.
                duplicate = true;
            }
        }
        drop(forced);
        if duplicate {
            std::mem::forget(slot);
            return;
        }
        drop(slot);
    }
}

impl fmt::Debug for MixerInputShutdown {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MixerInputShutdown")
            .finish_non_exhaustive()
    }
}

impl Future for MixerInputShutdown {
    type Output = Result<MixerInputRetirement, MixerRetirementError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.state {
            ShutdownState::Accepted(receiver) => match Pin::new(receiver).poll(cx) {
                Poll::Ready(Ok(result)) => {
                    self.state = ShutdownState::Finished;
                    Poll::Ready(result)
                }
                Poll::Ready(Err(_)) => {
                    self.state = ShutdownState::Finished;
                    Poll::Ready(Err(MixerRetirementError::OwnerUncertain))
                }
                Poll::Pending => Poll::Pending,
            },
            ShutdownState::Forced { slot, generation } => match slot.poll_forced(*generation, cx) {
                Poll::Ready(result) => {
                    if matches!(
                        result,
                        Ok(_)
                            | Err(MixerRetirementError::SourceDestructorPanicked
                                | MixerRetirementError::GenerationExhausted)
                    ) {
                        // SAFETY: the source was removed on every contained terminal path.
                        unsafe { ManuallyDrop::drop(slot) };
                    }
                    self.state = ShutdownState::Finished;
                    Poll::Ready(result)
                }
                Poll::Pending => Poll::Pending,
            },
            ShutdownState::RetainedError { error, .. } => Poll::Ready(Err(*error)),
            ShutdownState::Finished => Poll::Ready(Err(MixerRetirementError::Structural)),
        }
    }
}

fn terminal_shutdown_receipt(
    slot: &Arc<InputSlot>,
    generation: u64,
    word: u64,
) -> Option<MixerInputShutdown> {
    (slot_generation(word) == generation
        && matches!(
            slot_phase(word),
            SlotPhase::Retiring | SlotPhase::ForcedClean | SlotPhase::Quarantined
        ))
    .then(|| MixerInputShutdown {
        state: ShutdownState::Forced {
            slot: ManuallyDrop::new(Arc::clone(slot)),
            generation,
        },
    })
}

fn submit_shutdown(
    slot: Arc<InputSlot>,
    generation: u64,
    control: &Weak<ControlInner>,
    retained_driver_status: Option<&DriverStatus>,
    session: &Arc<SessionControl>,
) -> MixerInputShutdown {
    let live_control = control.upgrade();
    let Ok(session_gate) = session.gate.lock() else {
        return MixerInputShutdown {
            state: ShutdownState::RetainedError {
                _slot: ManuallyDrop::new(slot),
                error: MixerRetirementError::OwnerUncertain,
            },
        };
    };
    let initial = slot.word.load(Ordering::Acquire);
    if let Some(shutdown) = terminal_shutdown_receipt(&slot, generation, initial) {
        return shutdown;
    }
    let failure = retained_driver_status
        .and_then(DriverStatus::failure)
        .or_else(|| {
            live_control
                .as_ref()
                .and_then(|control| control.driver_status.failure())
        });
    let closed = failure.map_or_else(
        || slot.close(generation, false),
        |failure| slot.close_after_output_failure(generation, failure),
    );
    if !closed {
        return MixerInputShutdown {
            state: ShutdownState::RetainedError {
                _slot: ManuallyDrop::new(slot),
                error: MixerRetirementError::Structural,
            },
        };
    }
    if session.is_closing() {
        drop(session_gate);
        return MixerInputShutdown {
            state: ShutdownState::Forced {
                slot: ManuallyDrop::new(slot),
                generation,
            },
        };
    }
    #[cfg(test)]
    if let Some(hook) = lock_recover(&session.input_closed_hook).take() {
        let _ = hook.entered.send(());
        hook.release.wait();
    }
    let word = slot.word.load(Ordering::Acquire);
    // Output-death cleanup can advance this exact slot after the initial
    // snapshot or close CAS. Join its existing terminal receipt instead of
    // rejecting that valid transition or submitting a second retirement.
    if let Some(shutdown) = terminal_shutdown_receipt(&slot, generation, word) {
        return shutdown;
    }
    if slot_generation(word) != generation || slot_phase(word) != SlotPhase::Closing {
        return MixerInputShutdown {
            state: ShutdownState::RetainedError {
                _slot: ManuallyDrop::new(slot),
                error: MixerRetirementError::Structural,
            },
        };
    }
    let Some(control) = live_control else {
        drop(session_gate);
        return MixerInputShutdown {
            state: ShutdownState::Forced {
                slot: ManuallyDrop::new(slot),
                generation,
            },
        };
    };
    let (completion, receiver) = oneshot::channel();
    let request = RetirementRequest {
        record: ReservedRecord { slot, generation },
        completion,
    };
    let submitted = control.submit_retirement(request);
    drop(session_gate);
    match submitted {
        Ok(()) => MixerInputShutdown {
            state: ShutdownState::Accepted(receiver),
        },
        Err((MixerRetirementError::OwnerUncertain, request)) => MixerInputShutdown {
            state: ShutdownState::Forced {
                slot: ManuallyDrop::new(request.record.slot),
                generation,
            },
        },
        Err((error, request)) => MixerInputShutdown {
            state: ShutdownState::RetainedError {
                _slot: ManuallyDrop::new(request.record.slot),
                error,
            },
        },
    }
}

/// A real, generation-stamped mixer slot reserved before a callback exists.
pub struct MixerInputReservation {
    slot: ManuallyDrop<Arc<InputSlot>>,
    generation: u64,
    control: Weak<ControlInner>,
    session: Arc<SessionControl>,
}

impl fmt::Debug for MixerInputReservation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MixerInputReservation")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl MixerInputReservation {
    /// Abort this silent reservation and return a waking retirement future.
    #[must_use = "aborting a reservation returns the proof-bearing cleanup future"]
    pub fn abort(mut self) -> MixerInputShutdown {
        // SAFETY: this consumes the only stored strong owner.
        let slot = unsafe { ManuallyDrop::take(&mut self.slot) };
        let generation = self.generation;
        let control = self.control.clone();
        let session = Arc::clone(&self.session);
        std::mem::forget(self);
        submit_shutdown(slot, generation, &control, None, &session)
    }

    /// Install a preallocated input while holding exact start admission.
    ///
    /// # Errors
    ///
    /// Returns both the unchanged reservation and source when the owner is
    /// sealed/gone or the exact slot cannot be installed.
    fn install_preboxed(
        self,
        source: Box<dyn MixerInput>,
    ) -> Result<InstalledMixerInput, MixerInputInstallFailure> {
        let Some(control) = self.control.upgrade() else {
            return Err(MixerInputInstallFailure::new(
                MixerControlError::OwnerStopped,
                self,
                source,
            ));
        };
        let admission = match control.admit_start(&self.session) {
            Ok(admission) => admission,
            Err(error) => return Err(MixerInputInstallFailure::new(error, self, source)),
        };
        if let Err(source) = self.slot.install(self.generation, source) {
            return Err(MixerInputInstallFailure::new(
                MixerControlError::InternalInvariant,
                self,
                source,
            ));
        }
        let mut this = self;
        // SAFETY: ownership transfers into Installed and Drop is suppressed.
        let slot = unsafe { ManuallyDrop::take(&mut this.slot) };
        let generation = this.generation;
        let driver_status = Arc::clone(&control.driver_status);
        let weak_control = this.control.clone();
        let session = Arc::clone(&this.session);
        std::mem::forget(this);
        Ok(InstalledMixerInput {
            slot: ManuallyDrop::new(slot),
            generation,
            control: weak_control,
            driver_status,
            session,
            admission: Some(admission),
        })
    }

    /// Install and release-open a preallocated input as one admitted transaction.
    ///
    /// The start admission cannot escape this call, so service sealing cannot
    /// wait on an abandoned pre-open value.
    ///
    /// # Errors
    ///
    /// Returns [`MixerInputStartFailure::Rejected`] with the unchanged source
    /// and reservation when admission fails. A structural post-install failure
    /// returns [`MixerInputStartFailure::Cleanup`] with its stable cause and
    /// sole cleanup ownership.
    pub fn start_preboxed(
        self,
        source: Box<dyn MixerInput>,
    ) -> Result<RunningMixerInput, MixerInputStartFailure> {
        self.install_preboxed(source)
            .map_err(MixerInputStartFailure::Rejected)?
            .open()
            .map_err(|failure| {
                let (error, shutdown) = failure.into_parts();
                MixerInputStartFailure::Cleanup { error, shutdown }
            })
    }
}

impl Drop for MixerInputReservation {
    fn drop(&mut self) {
        // SAFETY: the field is initialized on every ordinary Drop path.
        let slot = unsafe { ManuallyDrop::take(&mut self.slot) };
        let shutdown = submit_shutdown(slot, self.generation, &self.control, None, &self.session);
        drop(shutdown);
    }
}

/// Installation failure retaining both the reserved slot and source.
pub struct MixerInputInstallFailure {
    error: MixerControlError,
    reservation: ManuallyDrop<MixerInputReservation>,
    source: ManuallyDrop<Box<dyn MixerInput>>,
}

impl fmt::Debug for MixerInputInstallFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MixerInputInstallFailure")
            .field("error", &self.error)
            .finish_non_exhaustive()
    }
}

impl MixerInputInstallFailure {
    fn new(
        error: MixerControlError,
        reservation: MixerInputReservation,
        source: Box<dyn MixerInput>,
    ) -> Self {
        Self {
            error,
            reservation: ManuallyDrop::new(reservation),
            source: ManuallyDrop::new(source),
        }
    }

    /// Stable control-plane cause.
    #[must_use]
    pub const fn error(&self) -> MixerControlError {
        self.error
    }

    /// Recover the exact reservation and source for caller-owned cleanup.
    #[must_use]
    pub fn into_parts(
        mut self,
    ) -> (
        MixerControlError,
        MixerInputReservation,
        Box<dyn MixerInput>,
    ) {
        // SAFETY: both fields are taken exactly once and Drop is suppressed.
        let reservation = unsafe { ManuallyDrop::take(&mut self.reservation) };
        let source = unsafe { ManuallyDrop::take(&mut self.source) };
        let error = self.error;
        (error, reservation, source)
    }
}

/// A failed synchronous start transaction with exact remaining ownership.
pub enum MixerInputStartFailure {
    /// Admission failed before the callback became renderable.
    Rejected(MixerInputInstallFailure),
    /// A structural post-install failure owns the committed cleanup future.
    Cleanup {
        /// Stable cause of the failed post-install open.
        error: MixerControlError,
        /// Sole cleanup ownership for the closed installed source.
        shutdown: MixerInputShutdown,
    },
}

impl fmt::Debug for MixerInputStartFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Rejected(failure) => formatter.debug_tuple("Rejected").field(failure).finish(),
            Self::Cleanup { error, .. } => formatter
                .debug_struct("Cleanup")
                .field("error", error)
                .finish_non_exhaustive(),
        }
    }
}

/// Input installed in a permanent Kira slot but still prestart-silent.
struct InstalledMixerInput {
    slot: ManuallyDrop<Arc<InputSlot>>,
    generation: u64,
    control: Weak<ControlInner>,
    driver_status: Arc<DriverStatus>,
    session: Arc<SessionControl>,
    admission: Option<StartAdmission>,
}

impl fmt::Debug for InstalledMixerInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("InstalledMixerInput")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl InstalledMixerInput {
    /// Release-open the exact installed generation.
    ///
    /// # Errors
    ///
    /// A failure is structural because the retained start admission prevents a
    /// normal service seal from racing this transition. The returned owner is
    /// already closed and still owns cleanup.
    fn open(mut self) -> Result<RunningMixerInput, MixerInputOpenFailure> {
        // SAFETY: ownership transfers out of the consumed Installed value.
        let slot = unsafe { ManuallyDrop::take(&mut self.slot) };
        let generation = self.generation;
        let control = self.control.clone();
        let driver_status = Arc::clone(&self.driver_status);
        let session = Arc::clone(&self.session);
        let live_control = control.upgrade();
        let initial_driver_snapshot = driver_status.snapshot();
        #[cfg(any(test, feature = "test-support"))]
        if let Some(control) = live_control.as_ref() {
            control.driver_status.pause_after_open_snapshot();
        }
        // The first snapshot is diagnostic only. Admission remains held while
        // a fresh snapshot linearizes physical publication, so a failure that
        // arrives during validation cannot turn a synchronous start rejection
        // into an already-doomed Running endpoint.
        let driver_snapshot = driver_status.snapshot();
        #[cfg(any(test, feature = "test-support"))]
        driver_status.pause_before_input_publish();
        let publish_snapshot = driver_status.snapshot();
        let driver_live = matches!(initial_driver_snapshot.phase, DriverPhase::Live)
            && initial_driver_snapshot.failure.is_none()
            && matches!(driver_snapshot.phase, DriverPhase::Live)
            && driver_snapshot.failure.is_none()
            && matches!(publish_snapshot.phase, DriverPhase::Live)
            && publish_snapshot.failure.is_none();
        if !driver_live || !slot.open(generation) {
            let output_failure = publish_snapshot
                .failure
                .or(driver_snapshot.failure)
                .or(initial_driver_snapshot.failure)
                .or_else(|| {
                    live_control
                        .as_ref()
                        .and_then(|control| control.driver_status.failure())
                });
            let error = if output_failure.is_some() || !driver_live {
                MixerControlError::OwnerStopped
            } else {
                MixerControlError::InternalInvariant
            };
            if let Some(failure) = output_failure {
                let _ = slot.close_after_output_failure(generation, failure);
            } else {
                let _ = slot.close(generation, true);
            }
            self.admission.take();
            std::mem::forget(self);
            return Err(MixerInputOpenFailure {
                error,
                owner: RunningMixerInput {
                    slot: ManuallyDrop::new(slot),
                    generation,
                    control,
                    driver_status,
                    session,
                },
            });
        }
        // Releasing this admission is the sole operation after publication.
        self.admission.take();
        std::mem::forget(self);
        Ok(RunningMixerInput {
            slot: ManuallyDrop::new(slot),
            generation,
            control,
            driver_status,
            session,
        })
    }
}

impl Drop for InstalledMixerInput {
    fn drop(&mut self) {
        // SAFETY: initialized on every ordinary Drop path.
        let slot = unsafe { ManuallyDrop::take(&mut self.slot) };
        let _ = slot.close(self.generation, false);
        self.admission.take();
        let shutdown = submit_shutdown(
            slot,
            self.generation,
            &self.control,
            Some(&self.driver_status),
            &self.session,
        );
        drop(shutdown);
    }
}

/// Structural open failure retaining exact cleanup ownership.
#[derive(Debug)]
struct MixerInputOpenFailure {
    error: MixerControlError,
    owner: RunningMixerInput,
}

impl MixerInputOpenFailure {
    /// Split the stable cause from exact cleanup ownership.
    fn into_parts(self) -> (MixerControlError, MixerInputShutdown) {
        (self.error, self.owner.shutdown())
    }

    #[cfg(test)]
    fn shutdown(self) -> MixerInputShutdown {
        self.owner.shutdown()
    }
}

/// Sole strong owner of one running logical mixer input.
pub struct RunningMixerInput {
    slot: ManuallyDrop<Arc<InputSlot>>,
    generation: u64,
    control: Weak<ControlInner>,
    driver_status: Arc<DriverStatus>,
    session: Arc<SessionControl>,
}

impl fmt::Debug for RunningMixerInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RunningMixerInput")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl RunningMixerInput {
    /// Silence this logical input from the next render entry onward.
    #[must_use]
    pub fn suspend(&self) -> bool {
        let Ok(_session_gate) = self.session.gate.lock() else {
            return false;
        };
        !self.session.is_closing()
            && self.driver_status.is_live()
            && self.slot.set_suspended(self.generation, true)
    }
    /// Reopen a suspended logical input from the next render entry onward.
    #[must_use]
    pub fn resume(&self) -> bool {
        let Ok(_session_gate) = self.session.gate.lock() else {
            return false;
        };
        !self.session.is_closing()
            && self.driver_status.is_live()
            && self.slot.set_suspended(self.generation, false)
    }
    /// Absorbingly close and commit off-render retirement.
    #[must_use = "logical shutdown returns the proof-bearing cleanup future"]
    pub fn shutdown(mut self) -> MixerInputShutdown {
        // SAFETY: the consumed owner transfers its only strong Arc.
        let slot = unsafe { ManuallyDrop::take(&mut self.slot) };
        let generation = self.generation;
        let control = self.control.clone();
        let driver_status = Arc::clone(&self.driver_status);
        let session = Arc::clone(&self.session);
        std::mem::forget(self);
        submit_shutdown(slot, generation, &control, Some(&driver_status), &session)
    }
    /// Whether rendering or the installed callback failed closed.
    #[must_use]
    pub fn is_failed(&self) -> bool {
        let word = self.slot.word.load(Ordering::Acquire);
        slot_generation(word) == self.generation && word & FAILED != 0
    }

    /// Global output cause observed by this exact running generation.
    #[must_use]
    pub fn output_failure(&self) -> Option<MixerOutputFailure> {
        let word = self.slot.word.load(Ordering::Acquire);
        let attached = (slot_generation(word) == self.generation && word & FAILED != 0)
            .then(|| decode_slot_failure(word))
            .flatten();
        attached.or_else(|| self.driver_status.failure())
    }
}

impl Drop for RunningMixerInput {
    fn drop(&mut self) {
        // SAFETY: initialized on every ordinary Drop path.
        let slot = unsafe { ManuallyDrop::take(&mut self.slot) };
        let shutdown = submit_shutdown(
            slot,
            self.generation,
            &self.control,
            Some(&self.driver_status),
            &self.session,
        );
        drop(shutdown);
    }
}

/// Weak, cloneable authority over the application mixer master gain.
///
/// The authority does not retain the mixer service or its unique physical
/// output join capability. Every accepted mutation runs on the sole mixer
/// owner and applies to Kira's main track, above every session subtree.
#[derive(Clone)]
pub struct MixerMasterGainAuthority {
    control: Weak<ControlInner>,
}

impl fmt::Debug for MixerMasterGainAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MixerMasterGainAuthority")
            .field("service_live", &(self.control.strong_count() != 0))
            .finish_non_exhaustive()
    }
}

impl MixerMasterGainAuthority {
    fn update(&self, update: MixerGainUpdate) -> Result<MixerGainState, MixerControlError> {
        let control = self
            .control
            .upgrade()
            .ok_or(MixerControlError::OwnerStopped)?;
        send_request(&control, |response| {
            OwnerCommand::UpdateMasterGain(update, response)
        })?
        .map_err(Into::into)
    }

    /// Set and remember a finite linear gain from [`MIN_CONTROL_GAIN`] through
    /// [`MAX_CONTROL_GAIN`], inclusive.
    ///
    /// If this authority is muted, the remembered value changes while its
    /// effective gain remains zero.
    ///
    /// # Errors
    ///
    /// Returns [`MixerControlError::InvalidGain`] before enqueueing for a
    /// non-finite or out-of-range value. Sealed, failed, saturated, and stopped
    /// services retain their existing typed control outcomes.
    pub fn set_linear(&self, linear: f32) -> Result<MixerGainState, MixerControlError> {
        self.update(MixerGainUpdate::Linear(validate_control_gain(linear)?))
    }

    /// Set the independent mute state and return the state applied by the
    /// mixer owner.
    ///
    /// # Errors
    ///
    /// Returns a typed bounded-control or stopped-service error.
    pub fn set_muted(&self, muted: bool) -> Result<MixerGainState, MixerControlError> {
        self.update(MixerGainUpdate::Muted(muted))
    }

    /// Atomically set the remembered linear gain and independent mute state.
    ///
    /// # Errors
    ///
    /// Returns [`MixerControlError::InvalidGain`] before enqueueing for a
    /// non-finite or out-of-range value, or a typed bounded-control/stopped
    /// service error if the update cannot be applied.
    pub fn set_state(&self, linear: f32, muted: bool) -> Result<MixerGainState, MixerControlError> {
        self.update(MixerGainUpdate::State(MixerGainState {
            linear: validate_control_gain(linear)?,
            muted,
        }))
    }

    /// Returns the first process-output failure observed by this authority.
    ///
    /// The diagnostic is read-only and does not retain the mixer service or
    /// its physical-output join authority. A first-writer cause remains stable
    /// while the mixer control owner is reachable. `None` is not proof of
    /// liveness and can also mean that owner has already gone away.
    #[must_use]
    pub fn output_failure(&self) -> Option<MixerOutputFailure> {
        self.control
            .upgrade()
            .and_then(|control| control.driver_status.failure())
    }
}

/// Weak, cloneable authority over one exact session-generation root gain.
///
/// Every accepted mutation runs on the sole mixer owner and applies above the
/// session's Script, Native, and Speech tracks. The encapsulated non-wrapping
/// generation prevents an authority from controlling a later session that
/// reuses the same public [`AudioSessionId`].
#[derive(Clone)]
pub struct MixerSessionGainAuthority {
    control: Weak<ControlInner>,
    session: Arc<SessionControl>,
}

impl fmt::Debug for MixerSessionGainAuthority {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MixerSessionGainAuthority")
            .field("id", &self.session.key.id)
            .finish_non_exhaustive()
    }
}

impl MixerSessionGainAuthority {
    fn update(&self, update: MixerGainUpdate) -> Result<MixerGainState, MixerControlError> {
        let control = self
            .control
            .upgrade()
            .ok_or(MixerControlError::OwnerStopped)?;
        send_session_request(&control, &self.session, |response| {
            OwnerCommand::UpdateSessionGain(self.session.key, update, response)
        })?
        .map_err(Into::into)
    }

    /// Set and remember a finite linear gain from [`MIN_CONTROL_GAIN`] through
    /// [`MAX_CONTROL_GAIN`], inclusive.
    ///
    /// If this authority is muted, the remembered value changes while its
    /// effective gain remains zero.
    ///
    /// # Errors
    ///
    /// Returns [`MixerControlError::InvalidGain`] before enqueueing for a
    /// non-finite or out-of-range value. Closing/stale sessions and failed,
    /// saturated, sealed, or stopped services retain typed control outcomes.
    pub fn set_linear(&self, linear: f32) -> Result<MixerGainState, MixerControlError> {
        self.update(MixerGainUpdate::Linear(validate_control_gain(linear)?))
    }

    /// Set the independent mute state and return the state applied by the
    /// mixer owner.
    ///
    /// # Errors
    ///
    /// Returns a typed bounded-control, stale-session, or stopped-service
    /// error.
    pub fn set_muted(&self, muted: bool) -> Result<MixerGainState, MixerControlError> {
        self.update(MixerGainUpdate::Muted(muted))
    }

    /// Atomically set the remembered linear gain and independent mute state.
    ///
    /// This is primarily useful when restoring a complete persisted policy:
    /// the mixer owner publishes one coherent state instead of exposing an
    /// intermediate linear-only or mute-only update.
    ///
    /// # Errors
    ///
    /// Returns [`MixerControlError::InvalidGain`] before enqueueing for a
    /// non-finite or out-of-range value. Other failures retain the same exact
    /// lifecycle outcomes as [`Self::set_linear`] and [`Self::set_muted`].
    pub fn set_state(&self, linear: f32, muted: bool) -> Result<MixerGainState, MixerControlError> {
        self.update(MixerGainUpdate::State(MixerGainState {
            linear: validate_control_gain(linear)?,
            muted,
        }))
    }

    /// Returns the first process-output failure observed by this authority.
    ///
    /// The diagnostic is shared by every session in the same process mixer. A
    /// first-writer cause remains stable while the mixer control owner is
    /// reachable. `None` is not proof of liveness and can also mean that owner
    /// has already gone away.
    #[must_use]
    pub fn output_failure(&self) -> Option<MixerOutputFailure> {
        self.control
            .upgrade()
            .and_then(|control| control.driver_status.failure())
    }
}

#[derive(Clone)]
struct MixerBusHandle {
    control: Weak<ControlInner>,
    session: Arc<SessionControl>,
    bus: SessionBus,
}

impl MixerBusHandle {
    fn try_reserve_input(&self) -> Result<MixerInputReservation, MixerControlError> {
        let control = self
            .control
            .upgrade()
            .ok_or(MixerControlError::OwnerStopped)?;
        let record = send_session_request(&control, &self.session, |response| {
            OwnerCommand::Reserve(self.session.key, self.bus, response)
        })?
        .map_err(MixerControlError::from)?;
        Ok(MixerInputReservation {
            slot: ManuallyDrop::new(record.slot),
            generation: record.generation,
            control: self.control.clone(),
            session: Arc::clone(&self.session),
        })
    }

    fn set_gain(&self, linear: f32) -> Result<(), MixerControlError> {
        if !linear.is_finite() || linear < 0.0 {
            return Err(MixerControlError::InvalidGain);
        }
        let control = self
            .control
            .upgrade()
            .ok_or(MixerControlError::OwnerStopped)?;
        send_session_request(&control, &self.session, |response| {
            OwnerCommand::SetGain(self.session.key, self.bus, linear, response)
        })?
        .map_err(Into::into)
    }

    fn format(&self) -> Result<MixerFormat, MixerControlError> {
        let control = self
            .control
            .upgrade()
            .ok_or(MixerControlError::OwnerStopped)?;
        let _session_gate = self
            .session
            .gate
            .lock()
            .map_err(|_| MixerControlError::OwnerStopped)?;
        if self.session.is_closing() {
            return Err(MixerControlError::UnknownSession);
        }
        let gate = control
            .gate
            .lock()
            .map_err(|_| MixerControlError::OwnerStopped)?;
        if gate.production_sealed || !control.driver_status.is_live() {
            Err(MixerControlError::OwnerStopped)
        } else {
            Ok(control.format)
        }
    }

    fn output_failure(&self) -> Option<MixerOutputFailure> {
        self.control
            .upgrade()
            .and_then(|control| control.driver_status.failure())
    }
}

macro_rules! typed_bus_handle {
    ($name:ident) => {
        #[doc = "A cloneable, scoped handle to one exact session bus."]
        #[derive(Clone)]
        pub struct $name(MixerBusHandle);

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_struct(stringify!($name))
                    .finish_non_exhaustive()
            }
        }

        impl $name {
            /// Reserve one real, preinstalled physical mixer slot.
            ///
            /// # Errors
            ///
            /// Returns a typed bounded-control, session, or capacity failure.
            pub fn try_reserve_input(&self) -> Result<MixerInputReservation, MixerControlError> {
                self.0.try_reserve_input()
            }
            /// Set this bus's finite linear gain through the mixer owner.
            ///
            /// # Errors
            ///
            /// Returns a typed validation or bounded-control failure.
            pub fn set_gain(&self, linear: f32) -> Result<(), MixerControlError> {
                self.0.set_gain(linear)
            }
            /// Exact format verified when the mixer backend started.
            ///
            /// # Errors
            ///
            /// Returns [`MixerControlError::UnknownSession`] after exact session
            /// retirement begins, or [`MixerControlError::OwnerStopped`] after
            /// service teardown.
            pub fn format(&self) -> Result<MixerFormat, MixerControlError> {
                self.0.format()
            }

            /// Returns the retained first physical-output failure, if any.
            /// This is read-only and does not retain the mixer owner.
            #[must_use]
            pub fn output_failure(&self) -> Option<MixerOutputFailure> {
                self.0.output_failure()
            }
        }
    };
}

typed_bus_handle!(MixerScriptBusHandle);
typed_bus_handle!(MixerNativeBusHandle);
typed_bus_handle!(MixerSpeechBusHandle);

enum SessionRetirementReceiptState {
    Awaiting(oneshot::Receiver<Result<(), MixerSessionRetirementError>>),
    Ready(Option<Result<(), MixerSessionRetirementError>>),
    Finished,
}

/// Cancellation-independent, waking proof of exact session retirement.
pub struct MixerSessionRetirement {
    state: SessionRetirementReceiptState,
}

impl fmt::Debug for MixerSessionRetirement {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MixerSessionRetirement")
            .finish_non_exhaustive()
    }
}

impl Future for MixerSessionRetirement {
    type Output = Result<(), MixerSessionRetirementError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match &mut self.state {
            SessionRetirementReceiptState::Awaiting(receiver) => {
                match Pin::new(receiver).poll(cx) {
                    Poll::Ready(Ok(result)) => {
                        self.state = SessionRetirementReceiptState::Finished;
                        Poll::Ready(result)
                    }
                    Poll::Ready(Err(_)) => {
                        self.state = SessionRetirementReceiptState::Finished;
                        Poll::Ready(Err(MixerSessionRetirementError::OwnerUncertain))
                    }
                    Poll::Pending => Poll::Pending,
                }
            }
            SessionRetirementReceiptState::Ready(result) => Poll::Ready(
                result
                    .take()
                    .unwrap_or(Err(MixerSessionRetirementError::Structural)),
            ),
            SessionRetirementReceiptState::Finished => {
                Poll::Ready(Err(MixerSessionRetirementError::Structural))
            }
        }
    }
}

fn submit_session_retirement(
    session: &Arc<SessionControl>,
    control: &Weak<ControlInner>,
    receiver: oneshot::Receiver<Result<(), MixerSessionRetirementError>>,
) -> MixerSessionRetirement {
    if let Err(error) = session.begin_close() {
        return MixerSessionRetirement {
            state: SessionRetirementReceiptState::Ready(Some(Err(error))),
        };
    }
    let request = SessionRetirementRequest { key: session.key };
    let Some(control) = control.upgrade() else {
        return MixerSessionRetirement {
            state: SessionRetirementReceiptState::Awaiting(receiver),
        };
    };
    match control.submit_session_retirement(request) {
        Ok(SessionRetirementSubmission::Queued | SessionRetirementSubmission::AwaitCompletion) => {
            MixerSessionRetirement {
                state: SessionRetirementReceiptState::Awaiting(receiver),
            }
        }
        Err(error) => MixerSessionRetirement {
            state: SessionRetirementReceiptState::Ready(Some(Err(error))),
        },
    }
}

/// Sole logical owner of one fixed session subtree.
///
/// This owner is deliberately not cloneable. Cloneable bus handles remain
/// scoped to this exact non-wrapping session generation and become inert as
/// soon as retirement begins.
pub struct MixerSessionOwner {
    control: Weak<ControlInner>,
    session: Option<Arc<SessionControl>>,
    retirement: Option<oneshot::Receiver<Result<(), MixerSessionRetirementError>>>,
}

impl fmt::Debug for MixerSessionOwner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MixerSessionOwner")
            .field("id", &self.session.as_ref().map(|session| session.key.id))
            .finish_non_exhaustive()
    }
}

impl MixerSessionOwner {
    /// Stable logical session id carried by this exact owner.
    ///
    /// The generation remains encapsulated by the owner and its scoped bus
    /// handles. Callers may use this id to reject accidental cross-session
    /// assembly before moving the unique owner into a higher-level authority.
    ///
    /// # Panics
    ///
    /// Panics only if the owner's private unique-lifetime invariant was
    /// corrupted inside this crate.
    #[must_use]
    pub fn session_id(&self) -> AudioSessionId {
        self.session
            .as_ref()
            .expect("retired session owner was used again")
            .key
            .id
    }

    /// Cloneable control authority for this exact session-generation root.
    ///
    /// The returned value is weak with respect to the mixer service and cannot
    /// control a later session that reuses this owner's public id.
    ///
    /// # Panics
    ///
    /// Panics only if the owner's private unique-lifetime invariant was
    /// corrupted inside this crate.
    #[must_use]
    pub fn gain_authority(&self) -> MixerSessionGainAuthority {
        MixerSessionGainAuthority {
            control: self.control.clone(),
            session: Arc::clone(
                self.session
                    .as_ref()
                    .expect("retired session owner was used again"),
            ),
        }
    }

    fn bus(&self, bus: SessionBus) -> MixerBusHandle {
        MixerBusHandle {
            control: self.control.clone(),
            session: Arc::clone(
                self.session
                    .as_ref()
                    .expect("retired session owner was used again"),
            ),
            bus,
        }
    }
    /// Exact Script-bus capability for Web Audio integration.
    #[must_use]
    pub fn script_bus(&self) -> MixerScriptBusHandle {
        MixerScriptBusHandle(self.bus(SessionBus::Script))
    }
    /// Exact Native-bus capability for client sounds.
    #[must_use]
    pub fn native_bus(&self) -> MixerNativeBusHandle {
        MixerNativeBusHandle(self.bus(SessionBus::Native))
    }
    /// Exact Speech-bus capability for accessibility playback.
    #[must_use]
    pub fn speech_bus(&self) -> MixerSpeechBusHandle {
        MixerSpeechBusHandle(self.bus(SessionBus::Speech))
    }

    /// Close this exact generation and return its proof-bearing retirement receipt.
    #[must_use = "session retirement is complete only after awaiting this receipt"]
    pub fn retire(mut self) -> MixerSessionRetirement {
        let (Some(session), Some(retirement)) = (self.session.take(), self.retirement.take())
        else {
            return MixerSessionRetirement {
                state: SessionRetirementReceiptState::Ready(Some(Err(
                    MixerSessionRetirementError::Structural,
                ))),
            };
        };
        submit_session_retirement(&session, &self.control, retirement)
    }
}

impl Drop for MixerSessionOwner {
    fn drop(&mut self) {
        if let (Some(session), Some(retirement)) = (self.session.take(), self.retirement.take()) {
            drop(submit_session_retirement(
                &session,
                &self.control,
                retirement,
            ));
        }
    }
}

/// Compatibility name for the now-unique session owner.
pub type MixerSessionHandle = MixerSessionOwner;

/// Weak, cloneable authority for adding sessions to one live process mixer.
///
/// A registrar never owns the mixer owner thread or its physical-output join
/// authority. It becomes inert when the originating service seals or dies,
/// and every accepted session is still assigned its non-wrapping generation
/// by that service's owner thread.
#[derive(Clone)]
pub struct MixerSessionRegistrar {
    control: Weak<ControlInner>,
}

impl fmt::Debug for MixerSessionRegistrar {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MixerSessionRegistrar")
            .field("service_live", &(self.control.strong_count() != 0))
            .finish_non_exhaustive()
    }
}

impl MixerSessionRegistrar {
    /// Add and fully preinstall one exact Script/Native/Speech session subtree.
    ///
    /// # Errors
    ///
    /// Returns a typed bounded-control, topology, sealed-service, or dead-owner
    /// error. No owner is published until every permanent slot is accepted.
    pub fn add_session(&self, id: AudioSessionId) -> Result<MixerSessionOwner, MixerControlError> {
        let control = self
            .control
            .upgrade()
            .ok_or(MixerControlError::OwnerStopped)?;
        add_session(&control, id)
    }
}

/// Bounded control plus unique joined ownership of one process mixer.
///
/// This join authority is intentionally thread-affine (`!Send` and `!Sync`).
/// Cloneable scoped bus handles carry cross-thread control. Keeping the service
/// thread-affine also prevents a [`MixerInput`] destructor from owning and
/// synchronously joining the owner that is waiting for that destructor.
pub struct MixerService {
    control: Option<Arc<ControlInner>>,
    owner: Option<JoinHandle<()>>,
    owner_status: Arc<OwnerStatus>,
    driver_status: Arc<DriverStatus>,
    output_failure_events: Option<MixerOutputFailureReceiver>,
    format: MixerFormat,
    _thread_affinity: PhantomData<Rc<()>>,
}

impl fmt::Debug for MixerService {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MixerService")
            .field("owner_live", &self.owner.is_some())
            .finish_non_exhaustive()
    }
}

impl MixerService {
    fn start_with_driver<D>(
        backend_settings: D::Settings,
        sample_rate: u32,
    ) -> Result<Self, MixerStartError<D::Error>>
    where
        D: JoinedOutputDriver + 'static,
        D::Settings: Send + 'static,
        D::Error: Send + 'static,
    {
        if sample_rate == 0 {
            return Err(MixerStartError::InvalidSampleRate);
        }
        Self::start_with_driver_and_limits::<D>(
            backend_settings,
            MixerFormat { sample_rate },
            MAX_SESSIONS,
            INPUTS_PER_BUS,
        )
    }

    fn start_with_driver_and_limits<D>(
        backend_settings: D::Settings,
        format: MixerFormat,
        max_sessions: usize,
        inputs_per_bus: usize,
    ) -> Result<Self, MixerStartError<D::Error>>
    where
        D: JoinedOutputDriver + 'static,
        D::Settings: Send + 'static,
        D::Error: Send + 'static,
    {
        let (commands, command_receiver) = mpsc::sync_channel(CONTROL_QUEUE_CAPACITY);
        let (retirements, retirement_receiver) = mpsc::sync_channel(RETIREMENT_QUEUE_CAPACITY);
        let (session_retirements, session_retirement_receiver) =
            mpsc::sync_channel(SESSION_RETIREMENT_QUEUE_CAPACITY.max(max_sessions));
        let driver_status = Arc::new(DriverStatus::new());
        let control = Arc::new(ControlInner {
            gate: Mutex::new(GateState {
                production_sealed: false,
                accepting_input_retirements: true,
                accepting_session_retirements: true,
                start_admissions: 0,
            }),
            gate_drained: Condvar::new(),
            commands,
            retirements,
            session_retirements,
            format,
            driver_status: Arc::clone(&driver_status),
        });
        let owner_status = Arc::new(OwnerStatus::new());
        let (output_failure_sender, output_failure_receiver) = futures_mpsc::channel(1);
        let (started_sender, started_receiver) = mpsc::sync_channel(1);
        let control_weak = Arc::downgrade(&control);
        let owner_status_thread = Arc::clone(&owner_status);
        let driver_status_thread = Arc::clone(&driver_status);
        let owner = thread::Builder::new()
            .name("smudgy-audio-owner".into())
            .spawn(move || {
                let mut output_failure_sender = Some(output_failure_sender);
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    run_owner::<D>(
                        backend_settings,
                        format,
                        max_sessions,
                        inputs_per_bus,
                        &command_receiver,
                        &retirement_receiver,
                        &session_retirement_receiver,
                        &started_sender,
                        &control_weak,
                        &owner_status_thread,
                        &driver_status_thread,
                        &mut output_failure_sender,
                    );
                }));
                if outcome.is_err() {
                    driver_status_thread.fail(MixerOutputFailure::OwnerPanicked);
                    let failure = driver_status_thread
                        .failure()
                        .unwrap_or(MixerOutputFailure::OwnerPanicked);
                    publish_output_failure(&mut output_failure_sender, failure);
                    if let Some(control) = control_weak.upgrade() {
                        let _ = control.seal_production();
                    }
                    owner_status_thread.clean.store(false, Ordering::Release);
                    owner_status_thread.retired.store(true, Ordering::Release);
                }
                forget_panic(outcome);
            })
            .map_err(MixerStartError::Thread)?;
        match started_receiver.recv() {
            Ok(Ok(_)) => Ok(Self {
                control: Some(control),
                owner: Some(owner),
                owner_status,
                driver_status,
                output_failure_events: Some(output_failure_receiver),
                format,
                _thread_affinity: PhantomData,
            }),
            Ok(Err(error)) => {
                drop(control);
                let _ = owner.join();
                Err(error)
            }
            Err(_) => {
                drop(control);
                let _ = owner.join();
                Err(MixerStartError::OwnerStopped)
            }
        }
    }

    /// Verified format shared by every logical input.
    #[must_use]
    pub fn format(&self) -> MixerFormat {
        self.format
    }

    /// Takes the unique terminal-output event receiver.
    ///
    /// A terminal failure that happened before this call remains buffered.
    /// Clean shutdown or ordinary service drop closes the receiver without an
    /// item. Subsequent calls return `None`.
    pub fn take_output_failure_events(&mut self) -> Option<MixerOutputFailureReceiver> {
        self.output_failure_events.take()
    }

    /// Returns a weak, cloneable session-registration authority.
    ///
    /// The registrar is `Send + Sync` but does not retain this thread-affine
    /// service or its unique output-join authority.
    #[must_use]
    pub fn session_registrar(&self) -> MixerSessionRegistrar {
        MixerSessionRegistrar {
            control: self.control.as_ref().map_or_else(Weak::new, Arc::downgrade),
        }
    }

    /// Returns a weak, cloneable authority over Kira's application main track.
    ///
    /// The authority becomes inert when this service seals or dies and never
    /// owns the unique physical-output join capability.
    #[must_use]
    pub fn master_gain_authority(&self) -> MixerMasterGainAuthority {
        MixerMasterGainAuthority {
            control: self.control.as_ref().map_or_else(Weak::new, Arc::downgrade),
        }
    }

    /// Add and fully preinstall the fixed Script/Native/Speech subtree.
    ///
    /// # Errors
    ///
    /// Returns a bounded control or topology error. No handle is published
    /// until every permanent slot sound has been accepted.
    pub fn add_session(&self, id: AudioSessionId) -> Result<MixerSessionOwner, MixerControlError> {
        let control = self
            .control
            .as_ref()
            .ok_or(MixerControlError::OwnerStopped)?;
        add_session(control, id)
    }

    /// Seal production, join the physical output owner, and force-resolve live slots.
    #[must_use]
    pub fn shutdown(mut self) -> MixerShutdown {
        if let Some(control) = self.control.as_ref()
            && control.seal_production().is_ok()
        {
            // Drain already-admitted opens while the driver is still live.
            // Once the admission gate is sealed, no opener can observe a
            // synthetic shutdown failure between its two liveness checks.
            self.driver_status.begin_close();
            let _ = control.commands.send(OwnerCommand::Shutdown);
        } else {
            self.driver_status.begin_close();
        }
        self.control.take();
        let joined = self.owner.take().is_some_and(|owner| owner.join().is_ok());
        let clean = joined
            && self.owner_status.retired.load(Ordering::Acquire)
            && self.owner_status.clean.load(Ordering::Acquire);
        MixerShutdown {
            clean,
            failure: self.driver_status.failure(),
        }
    }
}

fn add_session(
    control: &Arc<ControlInner>,
    id: AudioSessionId,
) -> Result<MixerSessionOwner, MixerControlError> {
    let session = send_request(control, |response| OwnerCommand::AddSession(id, response))?
        .map_err(MixerControlError::from)?;
    Ok(MixerSessionOwner {
        control: Arc::downgrade(control),
        session: Some(session.control),
        retirement: Some(session.retirement),
    })
}

impl Drop for MixerService {
    fn drop(&mut self) {
        if let Some(control) = self.control.take() {
            let _ = control.seal_production();
            self.driver_status.begin_close();
            let _ = control.commands.try_send(OwnerCommand::Shutdown);
        } else {
            self.driver_status.begin_close();
        }
        // Explicit shutdown is the proof-bearing joined path. Dropping the last
        // control Arc disconnects the owner if the bounded queue was full.
        self.owner.take();
    }
}

fn enqueue_request<T>(
    control: &Arc<ControlInner>,
    command: impl FnOnce(SyncSender<T>) -> OwnerCommand,
) -> Result<Receiver<T>, MixerControlError> {
    let gate = control
        .gate
        .lock()
        .map_err(|_| MixerControlError::OwnerStopped)?;
    if gate.production_sealed || !control.driver_status.is_live() {
        return Err(MixerControlError::OwnerStopped);
    }
    let (response_sender, response_receiver) = mpsc::sync_channel(1);
    match control.commands.try_send(command(response_sender)) {
        Ok(()) => {}
        Err(TrySendError::Full(_)) => return Err(MixerControlError::Saturated),
        Err(TrySendError::Disconnected(_)) => return Err(MixerControlError::OwnerStopped),
    }
    drop(gate);
    Ok(response_receiver)
}

fn send_request<T>(
    control: &Arc<ControlInner>,
    command: impl FnOnce(SyncSender<T>) -> OwnerCommand,
) -> Result<T, MixerControlError> {
    enqueue_request(control, command)?
        .recv()
        .map_err(|_| MixerControlError::OwnerStopped)
}

fn send_session_request<T>(
    control: &Arc<ControlInner>,
    session: &Arc<SessionControl>,
    command: impl FnOnce(SyncSender<T>) -> OwnerCommand,
) -> Result<T, MixerControlError> {
    let session_gate = session
        .gate
        .lock()
        .map_err(|_| MixerControlError::OwnerStopped)?;
    if session.is_closing() {
        return Err(MixerControlError::UnknownSession);
    }
    let response_receiver = enqueue_request(control, command)?;
    #[cfg(test)]
    session.pause_after_request_enqueued();
    let result = response_receiver
        .recv()
        .map_err(|_| MixerControlError::OwnerStopped);
    drop(session_gate);
    result
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_owner<D>(
    backend_settings: D::Settings,
    format: MixerFormat,
    max_sessions: usize,
    inputs_per_bus: usize,
    commands: &Receiver<OwnerCommand>,
    retirements: &Receiver<RetirementRequest>,
    session_retirements: &Receiver<SessionRetirementRequest>,
    started: &SyncSender<Result<PhysicalOutputFormat, MixerStartError<D::Error>>>,
    control: &Weak<ControlInner>,
    status: &OwnerStatus,
    driver_status: &Arc<DriverStatus>,
    output_failure_sender: &mut Option<futures_mpsc::Sender<MixerOutputFailure>>,
) where
    D: JoinedOutputDriver,
{
    let cleanup_capacity = max_sessions
        .saturating_mul(SESSION_BUS_COUNT)
        .saturating_mul(inputs_per_bus)
        .max(1);
    let (cleanup_sender, cleanup_receiver) = mpsc::sync_channel(cleanup_capacity);
    let (cleanup_results, cleanup_result_receiver) = mpsc::sync_channel(cleanup_capacity);
    let cleanup_owner = match thread::Builder::new()
        .name("smudgy-audio-cleanup".into())
        .spawn(move || run_cleanup_worker(&cleanup_receiver, &cleanup_results))
    {
        Ok(owner) => owner,
        Err(error) => {
            let _ = started.send(Err(MixerStartError::Thread(error)));
            status.retired.store(true, Ordering::Release);
            return;
        }
    };
    let constructed = catch_unwind(AssertUnwindSafe(|| {
        MixerCore::<D>::with_limits(
            backend_settings,
            Arc::clone(driver_status),
            format,
            max_sessions,
            inputs_per_bus,
        )
    }));
    let (mut mixer, physical_format) = match constructed {
        Ok(Ok(result)) => result,
        Ok(Err(error)) => {
            let _ = started.send(Err(error));
            drop(cleanup_sender);
            let _ = cleanup_owner.join();
            status.retired.store(true, Ordering::Release);
            return;
        }
        Err(payload) => {
            driver_status.fail(MixerOutputFailure::BackendFailure);
            std::mem::forget(payload);
            let _ = started.send(Err(MixerStartError::DriverFailed(
                MixerOutputFailure::BackendFailure,
            )));
            drop(cleanup_sender);
            let _ = cleanup_owner.join();
            status.retired.store(true, Ordering::Release);
            return;
        }
    };
    let published = started.send(Ok(physical_format)).is_ok();
    let mut pending = Vec::new();
    if published {
        let active = catch_unwind(AssertUnwindSafe(|| {
            run_active_owner(
                &mut mixer,
                commands,
                retirements,
                session_retirements,
                &cleanup_sender,
                &cleanup_result_receiver,
                control,
                &mut pending,
                driver_status,
                output_failure_sender,
            );
        }));
        if active.is_err() {
            driver_status.fail(MixerOutputFailure::OwnerPanicked);
            let failure = driver_status
                .failure()
                .unwrap_or(MixerOutputFailure::OwnerPanicked);
            publish_output_failure(output_failure_sender, failure);
        }
        forget_panic(active);
    }
    let clean = terminal_owner_cleanup(
        &mut mixer,
        retirements,
        session_retirements,
        control,
        &mut pending,
        cleanup_sender,
        &cleanup_result_receiver,
        cleanup_owner,
        driver_status,
    );
    status.clean.store(clean, Ordering::Release);
    status.retired.store(true, Ordering::Release);
}

fn publish_output_failure(
    sender: &mut Option<futures_mpsc::Sender<MixerOutputFailure>>,
    failure: MixerOutputFailure,
) {
    if let Some(mut sender) = sender.take() {
        let _ = sender.try_send(failure);
    }
}

fn handle_driver_failure<D: JoinedOutputDriver>(
    mixer: &mut MixerCore<D>,
    control: &Weak<ControlInner>,
    cleanup_sender: &SyncSender<CleanupTask>,
    output_failure_sender: &mut Option<futures_mpsc::Sender<MixerOutputFailure>>,
    failure: MixerOutputFailure,
) {
    publish_output_failure(output_failure_sender, failure);
    log::error!("physical audio output failed: {failure:?}");
    if let Some(control) = control.upgrade()
        && control.seal_production().is_err()
    {
        mixer.cleanup_clean = false;
    }
    notify_failed_inputs(mixer, failure, cleanup_sender);
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn run_active_owner<D: JoinedOutputDriver>(
    mixer: &mut MixerCore<D>,
    commands: &Receiver<OwnerCommand>,
    retirements: &Receiver<RetirementRequest>,
    session_retirements: &Receiver<SessionRetirementRequest>,
    cleanup_sender: &SyncSender<CleanupTask>,
    cleanup_results: &Receiver<CleanupResult>,
    control: &Weak<ControlInner>,
    pending: &mut Vec<RetirementRequest>,
    driver_status: &DriverStatus,
    output_failure_sender: &mut Option<futures_mpsc::Sender<MixerOutputFailure>>,
) {
    loop {
        #[cfg(any(test, feature = "test-support"))]
        assert!(
            !driver_status.panic_owner.swap(false, Ordering::AcqRel),
            "injected mixer owner panic"
        );
        drain_cleanup_results(mixer, cleanup_results);
        match mixer.maintain_output(Instant::now()) {
            DriverMaintenance::Continue => {}
            DriverMaintenance::Terminal(failure) => {
                driver_status.fail(failure);
            }
        }
        if let Some(failure) = driver_status.failure() {
            handle_driver_failure(
                mixer,
                control,
                cleanup_sender,
                output_failure_sender,
                failure,
            );
            break;
        }
        drain_retirements(retirements, pending);
        scan_retirements(mixer, pending, cleanup_sender);
        #[cfg(test)]
        driver_status.pause_after_input_retirement_scan();
        drain_session_retirements(mixer, session_retirements);
        // A per-input shutdown admitted before a session close publishes to
        // `retirements` before that session request can be submitted under the
        // same session gate. Re-draining after observing session work therefore
        // captures every exact completion authority before forced cleanup.
        drain_retirements(retirements, pending);
        scan_retirements(mixer, pending, cleanup_sender);
        #[cfg(test)]
        driver_status.pause_before_session_forced_cleanup();
        if !progress_session_retirements(mixer, cleanup_sender, pending, driver_status) {
            // Retirement progression can be the operation that discovers a
            // mixer-managed callback stall. Route that newly latched failure
            // through the same single terminal publication path used by the
            // maintenance scan before leaving the active loop. Structural
            // retirement failures do not latch driver status and still exit
            // without inventing an output failure.
            if let Some(failure) = driver_status.failure() {
                handle_driver_failure(
                    mixer,
                    control,
                    cleanup_sender,
                    output_failure_sender,
                    failure,
                );
            }
            break;
        }
        let mut shutting_down = false;
        match commands.recv_timeout(RETIREMENT_SCAN_INTERVAL) {
            Ok(OwnerCommand::AddSession(id, response)) => {
                let result = if driver_status.is_live() {
                    mixer.add_session(id)
                } else {
                    Err(MixerMutationError::DriverStopped)
                };
                let fatal = matches!(result, Err(MixerMutationError::InternalInvariant));
                mixer.cleanup_clean &= !fatal;
                if let Err(send_error) = response.send(result)
                    && send_error.0.is_ok()
                {
                    shutting_down = true;
                }
                shutting_down |= fatal;
            }
            Ok(OwnerCommand::Reserve(key, bus, response)) => {
                let result = if driver_status.is_live() {
                    mixer.reserve(key, bus)
                } else {
                    Err(MixerMutationError::DriverStopped)
                };
                if matches!(
                    result,
                    Err(MixerMutationError::InternalInvariant
                        | MixerMutationError::GenerationExhausted)
                ) {
                    mixer.cleanup_clean = false;
                }
                if let Err(send_error) = response.send(result)
                    && let Ok(record) = send_error.0
                    && mixer.restore_reserved(record).is_err()
                {
                    mixer.cleanup_clean = false;
                    shutting_down = true;
                }
            }
            Ok(OwnerCommand::SetGain(key, bus, linear, response)) => {
                let result = if driver_status.is_live() {
                    mixer.set_gain(key, bus, linear)
                } else {
                    Err(MixerMutationError::DriverStopped)
                };
                let _ = response.send(result);
            }
            Ok(OwnerCommand::UpdateMasterGain(update, response)) => {
                let result = if driver_status.is_live() {
                    mixer.update_master_gain(update)
                } else {
                    Err(MixerMutationError::DriverStopped)
                };
                let _ = response.send(result);
            }
            Ok(OwnerCommand::UpdateSessionGain(key, update, response)) => {
                let result = if driver_status.is_live() {
                    mixer.update_session_gain(key, update)
                } else {
                    Err(MixerMutationError::DriverStopped)
                };
                let _ = response.send(result);
            }
            Ok(OwnerCommand::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                shutting_down = true;
            }
            Err(RecvTimeoutError::Timeout) => {}
        }
        if shutting_down || control.upgrade().is_none() {
            break;
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn terminal_owner_cleanup<D: JoinedOutputDriver>(
    mixer: &mut MixerCore<D>,
    retirements: &Receiver<RetirementRequest>,
    session_retirements: &Receiver<SessionRetirementRequest>,
    control: &Weak<ControlInner>,
    pending: &mut Vec<RetirementRequest>,
    cleanup_sender: SyncSender<CleanupTask>,
    cleanup_results: &Receiver<CleanupResult>,
    cleanup_owner: JoinHandle<()>,
    driver_status: &DriverStatus,
) -> bool {
    if let Some(control) = control.upgrade() {
        if control.seal_production().is_err() {
            mixer.cleanup_clean = false;
        }
        control.stop_retirement_acceptance();
    }
    drain_retirements(retirements, pending);
    drain_session_retirements(mixer, session_retirements);
    drain_retirements(retirements, pending);
    if let Some(failure) = driver_status.failure() {
        notify_failed_inputs(mixer, failure, &cleanup_sender);
    } else {
        driver_status.begin_close();
    }
    mixer.close_all_slots();
    let backend_clean = retire_backend(mixer) && driver_status.is_joined_retired();
    if backend_clean {
        scan_retirements(mixer, pending, &cleanup_sender);
        for request in pending.drain(..) {
            let result = Err(MixerRetirementError::Structural);
            mixer.cleanup_clean = false;
            request.record.slot.force_quarantine();
            send_completion(request.completion, result);
        }
        let mut forced_jobs = mixer.prepare_forced_jobs().into_iter();
        while let Some(job) = forced_jobs.next() {
            let session_key = job.record.slot.address.session;
            if let Err(error) = cleanup_sender.send(CleanupTask::Retire(job)) {
                mixer.cleanup_clean = false;
                mixer.mark_session_cleanup_failed(session_key);
                retain_failed_cleanup_task(error.0);
                for job in forced_jobs {
                    mixer.mark_session_cleanup_failed(job.record.slot.address.session);
                    retain_failed_cleanup_job(job);
                }
                break;
            }
        }
    } else {
        mixer.cleanup_clean = false;
        for request in pending.drain(..) {
            request.record.slot.force_quarantine();
            send_completion(
                request.completion,
                Err(MixerRetirementError::OwnerUncertain),
            );
        }
        mixer.force_quarantine_all();
    }
    drop(cleanup_sender);
    while let Ok(result) = cleanup_results.recv() {
        finish_cleanup_result(mixer, result);
    }
    let cleanup_joined = cleanup_owner.join().is_ok();
    mixer.finish_session_retirements_after_backend(backend_clean && cleanup_joined);
    backend_clean && cleanup_joined && mixer.cleanup_clean
}

fn drain_retirements(
    retirements: &Receiver<RetirementRequest>,
    pending: &mut Vec<RetirementRequest>,
) {
    pending.extend(retirements.try_iter());
}

fn drain_session_retirements<D: JoinedOutputDriver>(
    mixer: &mut MixerCore<D>,
    retirements: &Receiver<SessionRetirementRequest>,
) {
    for request in retirements.try_iter() {
        if mixer.begin_session_retirement(request).is_err() {
            mixer.cleanup_clean = false;
        }
    }
}

fn progress_session_retirements<D: JoinedOutputDriver>(
    mixer: &mut MixerCore<D>,
    cleanup_sender: &SyncSender<CleanupTask>,
    pending: &[RetirementRequest],
    driver_status: &DriverStatus,
) -> bool {
    mixer.close_drained_session_inputs();
    let jobs = mixer.prepare_session_forced_jobs(pending);
    for job in jobs {
        let key = job.record.slot.address.session;
        if let Err(TrySendError::Full(task) | TrySendError::Disconnected(task)) =
            cleanup_sender.try_send(CleanupTask::Retire(job))
        {
            mixer.cleanup_clean = false;
            mixer.mark_session_cleanup_failed(key);
            retain_failed_cleanup_task(task);
        }
    }
    mixer.drop_ready_session_tracks();
    mixer.complete_rendered_session_retirements(driver_status)
}

fn scan_retirements<D: JoinedOutputDriver>(
    mixer: &mut MixerCore<D>,
    pending: &mut Vec<RetirementRequest>,
    cleanup_sender: &SyncSender<CleanupTask>,
) {
    let mut index = 0;
    while index < pending.len() {
        let request = &pending[index];
        if !request
            .record
            .slot
            .is_retire_ready(request.record.generation)
        {
            index += 1;
            continue;
        }
        let request = pending.swap_remove(index);
        let prepared = match request
            .record
            .slot
            .prepare_retirement(request.record.generation)
        {
            Ok(prepared) => prepared,
            Err(error) => {
                mixer.cleanup_clean = false;
                send_completion(request.completion, Err(error));
                continue;
            }
        };
        let job = CleanupJob {
            record: request.record,
            prepared,
            completion: Some(request.completion),
            terminal: false,
        };
        let session_key = job.record.slot.address.session;
        if let Err(TrySendError::Full(job) | TrySendError::Disconnected(job)) =
            cleanup_sender.try_send(CleanupTask::Retire(job))
        {
            mixer.cleanup_clean = false;
            mixer.mark_session_cleanup_failed(session_key);
            retain_failed_cleanup_task(job);
        }
    }
}

fn run_cleanup_worker(jobs: &Receiver<CleanupTask>, results: &SyncSender<CleanupResult>) {
    while let Ok(task) = jobs.recv() {
        match task {
            CleanupTask::Retire(mut job) => {
                // Retirement can take the sole observer before the owner's failure scan. Publish
                // that exact cause first so source/callback destruction cannot win the hosted
                // context's terminal latch and erase the physical-output failure.
                let observer = job.prepared.observer.take();
                if let (Some(observer), Some(failure)) =
                    (observer.as_ref(), job.prepared.pending_failure_notification)
                {
                    forget_panic(catch_unwind(AssertUnwindSafe(|| {
                        observer.output_failed(failure);
                    })));
                }
                forget_observer_panic(observer);
                if let Some(source) = job.prepared.source.take()
                    && forget_panic(catch_unwind(AssertUnwindSafe(|| drop(source)))).is_none()
                {
                    job.prepared.result = Err(MixerRetirementError::SourceDestructorPanicked);
                }
                if results
                    .send(CleanupResult {
                        record: job.record,
                        result: job.prepared.result,
                        completion: job.completion,
                        terminal: job.terminal,
                    })
                    .is_err()
                {
                    break;
                }
            }
            CleanupTask::Notify(notification) => {
                let FailureNotification { observer, failure } = notification;
                forget_panic(catch_unwind(AssertUnwindSafe(|| {
                    observer.output_failed(failure);
                })));
                forget_observer_panic(Some(observer));
            }
        }
    }
}

fn notify_failed_inputs<D: JoinedOutputDriver>(
    mixer: &mut MixerCore<D>,
    failure: MixerOutputFailure,
    cleanup_sender: &SyncSender<CleanupTask>,
) {
    let mut notification_transport_clean = true;
    let mut failed_sessions = Vec::new();
    mixer.for_each_slot(|slot| {
        let Some(observer) = slot.fail_live(failure) else {
            return;
        };
        let task = CleanupTask::Notify(FailureNotification { observer, failure });
        if let Err(TrySendError::Full(task) | TrySendError::Disconnected(task)) =
            cleanup_sender.try_send(task)
        {
            notification_transport_clean = false;
            failed_sessions.push(slot.address.session);
            retain_failed_cleanup_task(task);
        }
    });
    for key in failed_sessions {
        mixer.mark_session_cleanup_failed(key);
    }
    mixer.cleanup_clean &= notification_transport_clean;
}

fn retain_failed_cleanup_job(mut job: CleanupJob) {
    if let Some(source) = job.prepared.source.take() {
        std::mem::forget(source);
    }
    if let Some(observer) = job.prepared.observer.take() {
        std::mem::forget(observer);
    }
    job.record.slot.force_quarantine();
    if let Some(completion) = job.completion.take() {
        send_completion(completion, Err(MixerRetirementError::OwnerUncertain));
    }
    std::mem::forget(job);
}

fn retain_failed_cleanup_task(task: CleanupTask) {
    match task {
        CleanupTask::Retire(job) => retain_failed_cleanup_job(job),
        CleanupTask::Notify(notification) => std::mem::forget(notification),
    }
}

fn drain_cleanup_results<D: JoinedOutputDriver>(
    mixer: &mut MixerCore<D>,
    results: &Receiver<CleanupResult>,
) {
    for result in results.try_iter() {
        finish_cleanup_result(mixer, result);
    }
}

fn finish_cleanup_result<D: JoinedOutputDriver>(mixer: &mut MixerCore<D>, result: CleanupResult) {
    let CleanupResult {
        record,
        result,
        completion,
        terminal,
    } = result;
    let generation = record.generation;
    let session_key = record.slot.address.session;
    let session_control = mixer.session_control(session_key);
    #[cfg(test)]
    if let Some(session) = session_control.as_ref() {
        session.pause_before_cleanup_finish();
    }
    let terminal = terminal
        || session_control
            .as_ref()
            .is_none_or(|session| session.is_closing());
    let result = if terminal {
        record.slot.finish_terminal(generation, result)
    } else {
        match record.slot.finish_reusable(generation, result) {
            Ok((retirement, next_generation)) => {
                mixer.recycle(record, next_generation).map(|()| retirement)
            }
            Err(error) => Err(error),
        }
    };
    mixer.cleanup_clean &= result.is_ok();
    if result.is_err() {
        mixer.mark_session_cleanup_failed(session_key);
    }
    if let Some(completion) = completion {
        send_completion(completion, result);
    }
}

fn send_session_completion(
    completion: oneshot::Sender<Result<(), MixerSessionRetirementError>>,
    result: Result<(), MixerSessionRetirementError>,
) {
    forget_panic(catch_unwind(AssertUnwindSafe(|| {
        let _ = completion.send(result);
    })));
}

fn send_completion(
    completion: oneshot::Sender<Result<MixerInputRetirement, MixerRetirementError>>,
    result: Result<MixerInputRetirement, MixerRetirementError>,
) {
    forget_panic(catch_unwind(AssertUnwindSafe(|| {
        let _ = completion.send(result);
    })));
}

fn retire_backend<D: JoinedOutputDriver>(mixer: &mut MixerCore<D>) -> bool {
    let Some(manager) = mixer.manager.take() else {
        return false;
    };
    forget_panic(catch_unwind(AssertUnwindSafe(|| drop(manager)))).is_some()
}

fn forget_observer_panic(observer: Option<Arc<dyn MixerFailureObserver>>) {
    if let Some(observer) = observer {
        forget_panic(catch_unwind(AssertUnwindSafe(|| drop(observer))));
    }
}

fn forget_panic<T>(result: std::thread::Result<T>) -> Option<T> {
    match result {
        Ok(value) => Some(value),
        Err(payload) => {
            std::mem::forget(payload);
            None
        }
    }
}

fn lock_recover<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests;

#[cfg(feature = "test-support")]
#[doc(hidden)]
pub mod test_support;
