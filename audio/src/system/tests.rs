use std::{
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use super::*;
use crate::{MixerFrame, MixerInput, MixerInputStatus, MixerStartupFailure};
use futures::{StreamExt, executor::block_on};

const TEST_RATE: u32 = 48_000;

fn eventually(mut condition: impl FnMut() -> bool) {
    for _ in 0..2_000 {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(1));
    }
    panic!("condition did not become true");
}

#[test]
fn cpal_runtime_kinds_keep_advisories_distinct_from_rebuild_failures() {
    assert_eq!(
        runtime_event_from_cpal(CpalErrorKind::DeviceChanged),
        DriverRuntimeEvent::DeviceChanged
    );
    assert_eq!(
        runtime_event_from_cpal(CpalErrorKind::RealtimeDenied),
        DriverRuntimeEvent::RealtimeDenied
    );
    assert_eq!(
        runtime_event_from_cpal(CpalErrorKind::Xrun),
        DriverRuntimeEvent::Xrun
    );
    assert_eq!(
        runtime_event_from_cpal(CpalErrorKind::DeviceNotAvailable),
        DriverRuntimeEvent::DeviceNotAvailable
    );
    assert_eq!(
        runtime_event_from_cpal(CpalErrorKind::StreamInvalidated),
        DriverRuntimeEvent::StreamInvalidated
    );
}

#[test]
fn cpal_driver_is_the_only_callback_stall_watchdog() {
    assert_eq!(
        CpalOutputDriver::<FakeFactory>::CALLBACK_STALL_POLICY,
        CallbackStallPolicy::DriverManaged
    );
}

#[test]
fn recovery_warning_attempts_are_exponentially_spaced() {
    let warned = (0..=17)
        .filter(|attempt| recovery_attempt_is_warning(*attempt))
        .collect::<Vec<_>>();
    assert_eq!(warned, [1, 2, 4, 8, 16]);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum FakeFailure {
    #[default]
    None,
    SetupError,
    SetupPanic,
    EnumerateError,
    EnumeratePanic,
    PlanError,
    PlanPanic,
    BuildError,
    BuildPanic,
    DeathDuringBuild,
    PlayError,
    PlayPanic,
    DeathDuringPlay,
    DropPanic,
}

#[derive(Clone)]
#[allow(
    clippy::struct_excessive_bools,
    reason = "orthogonal fake callbacks and destructor faults must compose in one test plan"
)]
struct FakeConfig {
    ranges: Vec<OutputConfigRange>,
    failure: FakeFailure,
    callback_during_build: bool,
    callback_during_play: bool,
    retain_callback_after_drop: bool,
    host_drop_panics: bool,
    device_drop_panics: bool,
    recovery_protocol_failure: bool,
    recovery_enumerate_delay: Duration,
    stream_drop_delay: Duration,
}

impl Default for FakeConfig {
    fn default() -> Self {
        Self {
            ranges: vec![range(
                2,
                TEST_RATE,
                TEST_RATE,
                DriverSampleFormat::F32,
                OutputBufferRange::Range { min: 64, max: 512 },
            )],
            failure: FakeFailure::None,
            callback_during_build: false,
            callback_during_play: false,
            retain_callback_after_drop: false,
            host_drop_panics: false,
            device_drop_panics: false,
            recovery_protocol_failure: false,
            recovery_enumerate_delay: Duration::ZERO,
            stream_drop_delay: Duration::ZERO,
        }
    }
}

#[derive(Default)]
struct FakeCallbacks {
    data: Option<OutputDataCallback>,
    error: Option<OutputErrorCallback>,
}

struct FakeState {
    config: FakeConfig,
    device_available: AtomicBool,
    callbacks: Mutex<FakeCallbacks>,
    lifecycle: Mutex<Vec<(&'static str, String)>>,
    provisional_build_silent: AtomicBool,
    provisional_play_silent: AtomicBool,
    build_count: AtomicUsize,
    play_count: AtomicUsize,
    stream_drop_count: AtomicUsize,
}

impl FakeState {
    fn new(config: FakeConfig) -> Self {
        Self {
            config,
            device_available: AtomicBool::new(true),
            callbacks: Mutex::new(FakeCallbacks::default()),
            lifecycle: Mutex::new(Vec::new()),
            provisional_build_silent: AtomicBool::new(false),
            provisional_play_silent: AtomicBool::new(false),
            build_count: AtomicUsize::new(0),
            play_count: AtomicUsize::new(0),
            stream_drop_count: AtomicUsize::new(0),
        }
    }

    fn record(&self, operation: &'static str) {
        self.lifecycle.lock().unwrap().push((
            operation,
            thread::current().name().unwrap_or("unnamed").to_owned(),
        ));
    }

    fn invoke_f32(&self, samples: usize, initial: f32) -> Vec<f32> {
        let mut callback = self.callbacks.lock().unwrap().data.take().unwrap();
        let mut output = vec![initial; samples];
        callback(Some(OutputBuffer::F32(&mut output)));
        self.callbacks.lock().unwrap().data = Some(callback);
        output
    }

    fn invoke_i16(&self, samples: usize, initial: i16) -> Vec<i16> {
        let mut callback = self.callbacks.lock().unwrap().data.take().unwrap();
        let mut output = vec![initial; samples];
        callback(Some(OutputBuffer::I16(&mut output)));
        self.callbacks.lock().unwrap().data = Some(callback);
        output
    }

    fn invoke_u16(&self, samples: usize, initial: u16) -> Vec<u16> {
        let mut callback = self.callbacks.lock().unwrap().data.take().unwrap();
        let mut output = vec![initial; samples];
        callback(Some(OutputBuffer::U16(&mut output)));
        self.callbacks.lock().unwrap().data = Some(callback);
        output
    }

    fn invoke_provisional(&self) -> bool {
        match self
            .config
            .ranges
            .iter()
            .find(|range| {
                range.channels == 2
                    && range.min_sample_rate <= TEST_RATE
                    && TEST_RATE <= range.max_sample_rate
            })
            .map_or(DriverSampleFormat::F32, |range| range.sample_format)
        {
            DriverSampleFormat::F32 | DriverSampleFormat::Other => {
                self.invoke_f32(8, 1.0).iter().all(|sample| *sample == 0.0)
            }
            DriverSampleFormat::I16 => self.invoke_i16(8, 1).iter().all(|sample| *sample == 0),
            DriverSampleFormat::U16 => self
                .invoke_u16(8, 1)
                .iter()
                .all(|sample| *sample == 1 << 15),
        }
    }
}

#[derive(Clone)]
struct FakeFactory(Arc<FakeState>);

struct FakeHost(Arc<FakeState>);
struct FakeDevice(Arc<FakeState>);
struct FakeStream(Arc<FakeState>);

impl HostFactory for FakeFactory {
    type Host = FakeHost;

    fn create(self) -> Result<Self::Host, HostFailure> {
        self.0.record("host-create");
        match self.0.config.failure {
            FakeFailure::SetupError => Err(fake_failure("setup error")),
            FakeFailure::SetupPanic => panic!("setup panic"),
            _ => Ok(FakeHost(self.0)),
        }
    }
}

impl OutputHost for FakeHost {
    type Device = FakeDevice;

    fn default_output_device(&self) -> Result<Option<Self::Device>, HostFailure> {
        self.0.record("default-device");
        if self.0.build_count.load(Ordering::Acquire) != 0 {
            thread::sleep(self.0.config.recovery_enumerate_delay);
            if self.0.config.recovery_protocol_failure {
                return Err(HostFailure::new(
                    SystemOutputErrorKind::Protocol,
                    "deterministic recovery protocol failure",
                ));
            }
        }
        if !self.0.device_available.load(Ordering::Acquire) {
            return Ok(None);
        }
        match self.0.config.failure {
            FakeFailure::EnumerateError => Err(fake_failure("enumerate error")),
            FakeFailure::EnumeratePanic => panic!("enumerate panic"),
            _ => Ok(Some(FakeDevice(Arc::clone(&self.0)))),
        }
    }
}

impl Drop for FakeHost {
    fn drop(&mut self) {
        self.0.record("host-drop");
        assert!(!self.0.config.host_drop_panics, "host drop panic");
    }
}

impl OutputDevice for FakeDevice {
    type Stream = FakeStream;

    fn supported_output_configs(&self) -> Result<Vec<OutputConfigRange>, HostFailure> {
        self.0.record("supported-configs");
        match self.0.config.failure {
            FakeFailure::PlanError => Err(fake_failure("plan error")),
            FakeFailure::PlanPanic => panic!("plan panic"),
            _ => Ok(self.0.config.ranges.clone()),
        }
    }

    fn build_output_stream(
        &self,
        _config: OutputStreamConfig,
        data: OutputDataCallback,
        error: OutputErrorCallback,
    ) -> Result<Self::Stream, HostFailure> {
        self.0.record("stream-build");
        self.0.build_count.fetch_add(1, Ordering::AcqRel);
        match self.0.config.failure {
            FakeFailure::BuildError => return Err(fake_failure("build error")),
            FakeFailure::BuildPanic => panic!("build panic"),
            _ => {}
        }
        *self.0.callbacks.lock().unwrap() = FakeCallbacks {
            data: Some(data),
            error: Some(error),
        };
        if self.0.config.failure == FakeFailure::DeathDuringBuild {
            self.0.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
                CpalErrorKind::BackendError,
            ));
        }
        if self.0.config.callback_during_build {
            let silent = self.0.invoke_provisional();
            self.0
                .provisional_build_silent
                .store(silent, Ordering::Release);
        }
        Ok(FakeStream(Arc::clone(&self.0)))
    }
}

impl Drop for FakeDevice {
    fn drop(&mut self) {
        self.0.record("device-drop");
        assert!(!self.0.config.device_drop_panics, "device drop panic");
    }
}

impl OutputStream for FakeStream {
    fn play(&self) -> Result<(), HostFailure> {
        self.0.record("stream-play");
        self.0.play_count.fetch_add(1, Ordering::AcqRel);
        if self.0.config.callback_during_play {
            let silent = self.0.invoke_provisional();
            self.0
                .provisional_play_silent
                .store(silent, Ordering::Release);
        }
        match self.0.config.failure {
            FakeFailure::PlayError => Err(fake_failure("play error")),
            FakeFailure::PlayPanic => panic!("play panic"),
            FakeFailure::DeathDuringPlay => {
                self.0.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
                    CpalErrorKind::BackendError,
                ));
                Ok(())
            }
            _ => Ok(()),
        }
    }
}

impl Drop for FakeStream {
    fn drop(&mut self) {
        self.0.record("stream-drop");
        thread::sleep(self.0.config.stream_drop_delay);
        self.0.stream_drop_count.fetch_add(1, Ordering::AcqRel);
        if !self.0.config.retain_callback_after_drop {
            *self.0.callbacks.lock().unwrap() = FakeCallbacks::default();
        }
        assert_ne!(self.0.config.failure, FakeFailure::DropPanic, "drop panic");
    }
}

fn fake_failure(detail: &str) -> HostFailure {
    HostFailure::new(SystemOutputErrorKind::BackendFailure, detail)
}

fn range(
    channels: usize,
    min_sample_rate: u32,
    max_sample_rate: u32,
    sample_format: DriverSampleFormat,
    buffer_size: OutputBufferRange,
) -> OutputConfigRange {
    OutputConfigRange {
        channels,
        min_sample_rate,
        max_sample_rate,
        sample_format,
        buffer_size,
    }
}

struct FakeAttempt {
    result: Result<MixerService, MixerStartError<SystemOutputError>>,
    state: Arc<FakeState>,
}

fn attempt_fake(
    config: FakeConfig,
    sample_rate: u32,
    lease_flag: &'static AtomicBool,
) -> FakeAttempt {
    attempt_fake_with_hook(config, sample_rate, lease_flag, None)
}

fn attempt_fake_with_hook(
    config: FakeConfig,
    sample_rate: u32,
    lease_flag: &'static AtomicBool,
    proof_hook: Option<Arc<TestProofHook>>,
) -> FakeAttempt {
    let state = Arc::new(FakeState::new(config));
    let lease = match OutputLease::acquire(lease_flag) {
        Ok(lease) => lease,
        Err(error) => {
            return FakeAttempt {
                result: Err(MixerStartError::Backend(error)),
                state,
            };
        }
    };
    let result = MixerService::start_with_driver::<CpalOutputDriver<FakeFactory>>(
        SystemDriverSettings {
            factory: FakeFactory(Arc::clone(&state)),
            lease,
            sample_rate,
            idle: OutputIdlePolicy::AlwaysOpen,
            proof_hook,
        },
        sample_rate,
    );
    FakeAttempt { result, state }
}

fn fresh_lease_flag() -> &'static AtomicBool {
    Box::leak(Box::new(AtomicBool::new(false)))
}

fn start_fake(config: FakeConfig) -> (MixerService, Arc<FakeState>) {
    let attempt = attempt_fake(config, TEST_RATE, fresh_lease_flag());
    (attempt.result.unwrap(), attempt.state)
}

fn direct_fake_mixer(
    config: FakeConfig,
) -> (
    crate::MixerCore<CpalOutputDriver<FakeFactory>>,
    Arc<FakeState>,
) {
    let state = Arc::new(FakeState::new(config));
    let status = Arc::new(crate::DriverStatus::new());
    let lease = OutputLease::acquire(fresh_lease_flag()).unwrap();
    let (mixer, _) = crate::MixerCore::<CpalOutputDriver<FakeFactory>>::with_limits(
        SystemDriverSettings {
            factory: FakeFactory(Arc::clone(&state)),
            lease,
            sample_rate: TEST_RATE,
            idle: OutputIdlePolicy::AlwaysOpen,
            proof_hook: None,
        },
        Arc::clone(&status),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    (mixer, state)
}

#[derive(Default)]
struct ConstantInput(f32);

impl MixerInput for ConstantInput {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        output.fill(MixerFrame::from_mono(self.0));
        MixerInputStatus::Active
    }
}

struct StatefulInput {
    renders: Arc<AtomicUsize>,
    sequence: usize,
}

impl MixerInput for StatefulInput {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        self.sequence = self.sequence.saturating_add(1);
        self.renders.store(self.sequence, Ordering::Release);
        let sample = if self.sequence.is_multiple_of(2) {
            0.25
        } else {
            0.5
        };
        output.fill(MixerFrame::from_mono(sample));
        MixerInputStatus::Active
    }
}

fn install_constant(
    service: &MixerService,
    value: f32,
) -> (crate::MixerSessionOwner, crate::RunningMixerInput) {
    let session = service.add_session(AudioSessionId(1)).unwrap();
    let running = session
        .script_bus()
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(ConstantInput(value)))
        .unwrap();
    (session, running)
}

#[test]
fn planner_requires_exact_rate_stereo_pcm_and_clamps_the_hint() {
    let ranges = [
        range(
            1,
            TEST_RATE,
            TEST_RATE,
            DriverSampleFormat::F32,
            OutputBufferRange::Unknown,
        ),
        range(
            2,
            44_100,
            44_100,
            DriverSampleFormat::F32,
            OutputBufferRange::Unknown,
        ),
        range(
            2,
            TEST_RATE,
            TEST_RATE,
            DriverSampleFormat::Other,
            OutputBufferRange::Unknown,
        ),
        range(
            2,
            TEST_RATE,
            TEST_RATE,
            DriverSampleFormat::I16,
            OutputBufferRange::Range { min: 256, max: 512 },
        ),
        range(
            2,
            TEST_RATE,
            TEST_RATE,
            DriverSampleFormat::F32,
            OutputBufferRange::Range { min: 32, max: 64 },
        ),
    ];
    let plan = plan_output(ranges, TEST_RATE).unwrap();
    assert_eq!(plan.stream.sample_format, DriverSampleFormat::F32);
    assert_eq!(plan.stream.buffer_size, OutputBufferRequest::Fixed(64));
    assert_eq!(plan.physical.sample_format(), PhysicalSampleFormat::F32);
    assert_eq!(plan.physical.buffer_frames_hint(), Some(64));

    let i16 = plan_output([ranges[3]], TEST_RATE).unwrap();
    assert_eq!(i16.physical.buffer_frames_hint(), Some(256));
    assert!(plan_output([ranges[0]], TEST_RATE).is_err());
    assert!(plan_output([ranges[1]], TEST_RATE).is_err());
    assert!(plan_output([ranges[2]], TEST_RATE).is_err());
    assert!(
        plan_output(
            [range(
                2,
                TEST_RATE,
                TEST_RATE,
                DriverSampleFormat::F32,
                OutputBufferRange::Range {
                    min: MAX_PHYSICAL_CALLBACK_FRAMES + 1,
                    max: MAX_PHYSICAL_CALLBACK_FRAMES + 2,
                },
            )],
            TEST_RATE,
        )
        .is_err()
    );
}

#[test]
fn unknown_buffer_size_publishes_no_hint() {
    let plan = plan_output(
        [range(
            2,
            TEST_RATE,
            TEST_RATE,
            DriverSampleFormat::U16,
            OutputBufferRange::Unknown,
        )],
        TEST_RATE,
    )
    .unwrap();
    assert_eq!(plan.stream.buffer_size, OutputBufferRequest::Default);
    assert_eq!(plan.physical.buffer_frames_hint(), None);
}

#[test]
fn every_supported_pcm_encoding_renders_non_hint_sizes() {
    for format in [
        DriverSampleFormat::F32,
        DriverSampleFormat::I16,
        DriverSampleFormat::U16,
    ] {
        let (service, state) = start_fake(FakeConfig {
            ranges: vec![range(
                2,
                TEST_RATE,
                TEST_RATE,
                format,
                OutputBufferRange::Range { min: 64, max: 512 },
            )],
            ..FakeConfig::default()
        });
        let (session, running) = install_constant(&service, 0.25);
        match format {
            DriverSampleFormat::F32 => {
                let output = state.invoke_f32(514, 9.0);
                assert!(output.iter().all(|sample| (*sample - 0.25).abs() < 0.0001));
            }
            DriverSampleFormat::I16 => {
                let output = state.invoke_i16(514, 9);
                assert!(
                    output
                        .iter()
                        .all(|sample| *sample > 8_000 && *sample < 8_300)
                );
            }
            DriverSampleFormat::U16 => {
                let output = state.invoke_u16(514, 9);
                assert!(
                    output
                        .iter()
                        .all(|sample| *sample > 40_000 && *sample < 41_100)
                );
            }
            DriverSampleFormat::Other => unreachable!(),
        }
        let shutdown = running.shutdown();
        // A final physical quantum publishes the render-side close bit.
        match format {
            DriverSampleFormat::F32 => drop(state.invoke_f32(2, 9.0)),
            DriverSampleFormat::I16 => drop(state.invoke_i16(2, 9)),
            DriverSampleFormat::U16 => drop(state.invoke_u16(2, 9)),
            DriverSampleFormat::Other => unreachable!(),
        }
        assert!(block_on(shutdown).unwrap().is_clean());
        drop(session);
        assert!(service.shutdown().clean);
    }
}

#[test]
fn callback_establishes_equilibrium_and_rejects_invalid_geometry_without_allocating() {
    for samples in [0, 1, PHYSICAL_SCRATCH_SAMPLES + 2] {
        let (service, state) = start_fake(FakeConfig::default());
        let mut output = vec![9.0; samples];
        let mut callback = state.callbacks.lock().unwrap().data.take().unwrap();
        assert_no_alloc::assert_no_alloc(|| callback(Some(OutputBuffer::F32(&mut output))));
        assert!(output.iter().all(|sample| *sample == 0.0));
        state.callbacks.lock().unwrap().data = Some(callback);
        let shutdown = service.shutdown();
        assert!(shutdown.clean);
        assert_eq!(
            shutdown.failure,
            Some(MixerOutputFailure::InvalidCallbackGeometry)
        );
    }

    let (service, state) = start_fake(FakeConfig {
        ranges: vec![range(
            2,
            TEST_RATE,
            TEST_RATE,
            DriverSampleFormat::U16,
            OutputBufferRange::Unknown,
        )],
        ..FakeConfig::default()
    });
    let output = state.invoke_u16(1, 7);
    assert_eq!(output, [1 << 15]);
    assert!(service.shutdown().clean);

    let (service, state) = start_fake(FakeConfig::default());
    let output = state.invoke_u16(2, 7);
    assert_eq!(output, [1 << 15; 2]);
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(
        shutdown.failure,
        Some(MixerOutputFailure::InvalidCallbackGeometry)
    );
}

#[test]
fn raw_cpal_dispatch_latches_wrong_format_without_typed_expect() {
    let status = Arc::new(crate::DriverStatus::new());
    let state = Arc::new(CallbackState::new(
        DriverFailureSignal(Arc::downgrade(&status)),
        None,
        None,
    ));
    let mut callback = callback_for(&state, state.generation(), DriverSampleFormat::F32);
    let mut samples = [7_i16; 4];
    // SAFETY: the pointer, length, and declared sample format exactly describe
    // the live `samples` array for the duration of this call.
    let mut raw = unsafe {
        cpal::Data::from_parts(
            samples.as_mut_ptr().cast::<()>(),
            samples.len(),
            SampleFormat::I16,
        )
    };
    assert_no_alloc::assert_no_alloc(|| dispatch_raw_output(&mut raw, &mut callback));
    assert_eq!(samples, [0; 4]);
    assert_eq!(
        status.failure(),
        Some(MixerOutputFailure::InvalidCallbackGeometry)
    );

    let status = Arc::new(crate::DriverStatus::new());
    let state = Arc::new(CallbackState::new(
        DriverFailureSignal(Arc::downgrade(&status)),
        None,
        None,
    ));
    let mut callback = callback_for(&state, state.generation(), DriverSampleFormat::F32);
    let mut opaque_prefill = [0x69_u8; 8];
    // SAFETY: the pointer, length, and declared sample format exactly describe
    // the live opaque buffer for the duration of this call.
    let mut raw = unsafe {
        cpal::Data::from_parts(
            opaque_prefill.as_mut_ptr().cast::<()>(),
            opaque_prefill.len(),
            SampleFormat::DsdU8,
        )
    };
    assert_no_alloc::assert_no_alloc(|| dispatch_raw_output(&mut raw, &mut callback));
    assert_eq!(opaque_prefill, [0x69; 8]);
    assert_eq!(
        status.failure(),
        Some(MixerOutputFailure::InvalidCallbackGeometry)
    );
}

#[test]
fn maximum_valid_callback_is_allocation_free() {
    let (service, state) = start_fake(FakeConfig::default());
    let (session, running) = install_constant(&service, 0.125);
    let mut output = vec![9.0; PHYSICAL_SCRATCH_SAMPLES];
    let mut callback = state.callbacks.lock().unwrap().data.take().unwrap();
    assert_no_alloc::assert_no_alloc(|| callback(Some(OutputBuffer::F32(&mut output))));
    assert!(output.iter().all(|sample| (*sample - 0.125).abs() < 0.0001));
    state.callbacks.lock().unwrap().data = Some(callback);
    let shutdown = running.shutdown();
    drop(state.invoke_f32(2, 9.0));
    assert!(block_on(shutdown).unwrap().is_clean());
    drop(session);
    assert!(service.shutdown().clean);
}

#[test]
fn callbacks_during_build_and_play_are_provisionally_silent() {
    let (service, state) = start_fake(FakeConfig {
        callback_during_build: true,
        callback_during_play: true,
        ..FakeConfig::default()
    });
    assert!(state.provisional_build_silent.load(Ordering::Acquire));
    assert!(state.provisional_play_silent.load(Ordering::Acquire));
    assert!(service.shutdown().clean);
}

#[test]
fn host_failures_and_panics_have_stable_operations() {
    let cases = [
        (FakeFailure::SetupError, SystemOutputOperation::Setup),
        (FakeFailure::SetupPanic, SystemOutputOperation::Setup),
        (
            FakeFailure::EnumerateError,
            SystemOutputOperation::Enumerate,
        ),
        (
            FakeFailure::EnumeratePanic,
            SystemOutputOperation::Enumerate,
        ),
        (FakeFailure::PlanError, SystemOutputOperation::Plan),
        (FakeFailure::PlanPanic, SystemOutputOperation::Plan),
        (FakeFailure::BuildError, SystemOutputOperation::Build),
        (FakeFailure::BuildPanic, SystemOutputOperation::Build),
        (FakeFailure::PlayError, SystemOutputOperation::Play),
        (FakeFailure::PlayPanic, SystemOutputOperation::Play),
    ];
    for (failure, operation) in cases {
        let attempt = attempt_fake(
            FakeConfig {
                failure,
                ..FakeConfig::default()
            },
            TEST_RATE,
            fresh_lease_flag(),
        );
        let error = attempt.result.unwrap_err();
        let backend = match error {
            MixerStartError::Backend(error)
            | MixerStartError::CleanupUncertain(MixerStartupFailure::Backend(error)) => error,
            other => panic!("unexpected startup failure: {other:?}"),
        };
        assert_eq!(backend.operation(), operation);
        assert_eq!(backend.kind(), SystemOutputErrorKind::BackendFailure);
        assert!(!backend.detail().is_empty());
    }
}

#[test]
fn unavailable_cause_preserves_exact_start_error_and_cleanup_truth() {
    let thread = SystemMixerUnavailable::from(MixerStartError::Thread(
        std::io::Error::from_raw_os_error(5),
    ));
    assert!(thread.cleanup_proven());
    match thread.error() {
        MixerStartError::Thread(error) => assert_eq!(error.raw_os_error(), Some(5)),
        other => panic!("exact thread cause was not retained: {other:?}"),
    }

    let protocol = SystemOutputError::new(
        SystemOutputErrorKind::Protocol,
        SystemOutputOperation::Build,
        "deterministic protocol failure",
    );
    let uncertain = SystemMixerUnavailable::from(MixerStartError::CleanupUncertain(
        MixerStartupFailure::Backend(protocol),
    ));
    assert!(!uncertain.cleanup_proven());
    assert!(matches!(
        uncertain.error(),
        MixerStartError::CleanupUncertain(MixerStartupFailure::Backend(error))
            if error.kind() == SystemOutputErrorKind::Protocol
                && error.operation() == SystemOutputOperation::Build
                && error.detail() == "deterministic protocol failure"
    ));

    let owner_stopped = SystemMixerUnavailable::from(MixerStartError::OwnerStopped);
    assert!(!owner_stopped.cleanup_proven());
    assert!(matches!(
        owner_stopped.error(),
        MixerStartError::OwnerStopped
    ));
}

fn cleanup_uncertain_backend(error: MixerStartError<SystemOutputError>) -> SystemOutputError {
    match error {
        MixerStartError::CleanupUncertain(MixerStartupFailure::Backend(error)) => error,
        other => panic!("expected cleanup-uncertain backend failure, got {other:?}"),
    }
}

#[test]
fn staged_host_destructor_panics_preserve_primary_cause_and_retain_lease() {
    for (failure, operation) in [
        (
            FakeFailure::EnumerateError,
            SystemOutputOperation::Enumerate,
        ),
        (FakeFailure::PlanError, SystemOutputOperation::Plan),
    ] {
        let flag = fresh_lease_flag();
        let error = attempt_fake(
            FakeConfig {
                failure,
                host_drop_panics: true,
                ..FakeConfig::default()
            },
            TEST_RATE,
            flag,
        )
        .result
        .unwrap_err();
        assert_eq!(cleanup_uncertain_backend(error).operation(), operation);
        assert!(flag.load(Ordering::Acquire));
    }

    let flag = fresh_lease_flag();
    let error = attempt_fake(
        FakeConfig {
            host_drop_panics: true,
            ..FakeConfig::default()
        },
        TEST_RATE,
        flag,
    )
    .result
    .unwrap_err();
    assert_eq!(
        cleanup_uncertain_backend(error).operation(),
        SystemOutputOperation::Drop
    );
    assert!(flag.load(Ordering::Acquire));
    assert!(matches!(
        attempt_fake(FakeConfig::default(), TEST_RATE, flag).result,
        Err(MixerStartError::Backend(ref error))
            if error.kind() == SystemOutputErrorKind::OutputInUse
    ));
}

#[test]
fn staged_device_destructor_panics_preserve_primary_cause_and_retain_lease() {
    for (failure, operation) in [
        (FakeFailure::PlanError, SystemOutputOperation::Plan),
        (FakeFailure::PlanPanic, SystemOutputOperation::Plan),
        (FakeFailure::BuildError, SystemOutputOperation::Build),
    ] {
        let flag = fresh_lease_flag();
        let error = attempt_fake(
            FakeConfig {
                failure,
                device_drop_panics: true,
                ..FakeConfig::default()
            },
            TEST_RATE,
            flag,
        )
        .result
        .unwrap_err();
        assert_eq!(cleanup_uncertain_backend(error).operation(), operation);
        assert!(flag.load(Ordering::Acquire));
    }

    let flag = fresh_lease_flag();
    let error = attempt_fake(
        FakeConfig {
            device_drop_panics: true,
            ..FakeConfig::default()
        },
        TEST_RATE,
        flag,
    )
    .result
    .unwrap_err();
    assert_eq!(
        cleanup_uncertain_backend(error).operation(),
        SystemOutputOperation::Drop
    );
    assert!(flag.load(Ordering::Acquire));
}

#[test]
fn recoverable_death_during_start_keeps_the_mixer_live_and_retries() {
    for failure in [FakeFailure::DeathDuringBuild, FakeFailure::DeathDuringPlay] {
        let attempt = attempt_fake(
            FakeConfig {
                failure,
                ..FakeConfig::default()
            },
            TEST_RATE,
            fresh_lease_flag(),
        );
        let service = attempt.result.unwrap();
        eventually(|| attempt.state.build_count.load(Ordering::Acquire) >= 2);
        let _session = service.add_session(AudioSessionId(75)).unwrap();
        let shutdown = service.shutdown();
        assert!(shutdown.clean);
        assert_eq!(shutdown.failure, None);
    }
}

#[test]
fn host_device_build_play_and_drop_are_owner_thread_affine() {
    let (service, state) = start_fake(FakeConfig::default());
    assert!(service.shutdown().clean);
    let lifecycle = state.lifecycle.lock().unwrap();
    for (operation, thread_name) in lifecycle.iter() {
        assert_eq!(
            thread_name, "smudgy-audio-owner",
            "{operation} ran on the wrong thread"
        );
    }
    for required in [
        "host-create",
        "default-device",
        "supported-configs",
        "stream-build",
        "stream-play",
        "stream-drop",
        "device-drop",
        "host-drop",
    ] {
        assert!(
            lifecycle
                .iter()
                .any(|(operation, _)| *operation == required)
        );
    }
}

struct BlockingInput {
    entered: Arc<AtomicBool>,
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl MixerInput for BlockingInput {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        self.entered.store(true, Ordering::Release);
        let (lock, ready) = &*self.gate;
        let mut released = lock.lock().unwrap();
        while !*released {
            released = ready.wait(released).unwrap();
        }
        output.fill(MixerFrame::ZERO);
        MixerInputStatus::Active
    }
}

#[test]
fn shutdown_waits_for_an_active_callback_before_releasing_the_lease() {
    let flag = fresh_lease_flag();
    let attempt = attempt_fake(FakeConfig::default(), TEST_RATE, flag);
    let service = attempt.result.unwrap();
    let state = attempt.state;
    let session = service.add_session(AudioSessionId(9)).unwrap();
    let entered = Arc::new(AtomicBool::new(false));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let _running = session
        .script_bus()
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(BlockingInput {
            entered: Arc::clone(&entered),
            gate: Arc::clone(&gate),
        }))
        .unwrap();
    let callback_state = Arc::clone(&state);
    let callback = thread::spawn(move || drop(callback_state.invoke_f32(256, 1.0)));
    while !entered.load(Ordering::Acquire) {
        thread::yield_now();
    }
    let gate_release = Arc::clone(&gate);
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(20));
        let (lock, ready) = &*gate_release;
        *lock.lock().unwrap() = true;
        ready.notify_one();
    });
    let shutdown = service.shutdown();
    callback.join().unwrap();
    release.join().unwrap();
    assert!(shutdown.clean);
    assert_eq!(state.stream_drop_count.load(Ordering::Acquire), 1);
    assert!(!flag.load(Ordering::Acquire));
}

#[test]
fn stalled_active_callback_has_no_false_retirement_deadline() {
    let flag = fresh_lease_flag();
    let attempt = attempt_fake(FakeConfig::default(), TEST_RATE, flag);
    let service = attempt.result.unwrap();
    let state = attempt.state;
    let session = service.add_session(AudioSessionId(10)).unwrap();
    let entered = Arc::new(AtomicBool::new(false));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let _running = session
        .script_bus()
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(BlockingInput {
            entered: Arc::clone(&entered),
            gate: Arc::clone(&gate),
        }))
        .unwrap();
    let callback_state = Arc::clone(&state);
    let callback = thread::spawn(move || drop(callback_state.invoke_f32(256, 1.0)));
    while !entered.load(Ordering::Acquire) {
        thread::yield_now();
    }
    let gate_release = Arc::clone(&gate);
    let release = thread::spawn(move || {
        thread::sleep(Duration::from_millis(150));
        let (lock, ready) = &*gate_release;
        *lock.lock().unwrap() = true;
        ready.notify_one();
    });
    let shutdown = service.shutdown();
    callback.join().unwrap();
    release.join().unwrap();
    assert!(shutdown.clean);
    assert!(!flag.load(Ordering::Acquire));
}

#[test]
fn late_weak_upgrade_cannot_move_callback_state_destruction_off_owner() {
    let flag = fresh_lease_flag();
    let (before_sender, before_receiver) = std::sync::mpsc::sync_channel(1);
    let before_release = Arc::new((Mutex::new(false), Condvar::new()));
    let (attempt_sender, attempt_receiver) = std::sync::mpsc::sync_channel(1);
    let (upgrade_sender, upgrade_receiver) = std::sync::mpsc::sync_channel(1);
    let upgrade_release = Arc::new((Mutex::new(false), Condvar::new()));
    let hook = Arc::new(TestProofHook {
        before_unwrap_entered: before_sender,
        before_unwrap_release: Arc::clone(&before_release),
        unwrap_attempted: attempt_sender,
        unwrap_notified: AtomicBool::new(false),
        late_upgrade_entered: upgrade_sender,
        late_upgrade_release: Arc::clone(&upgrade_release),
    });
    let attempt = attempt_fake_with_hook(
        FakeConfig {
            retain_callback_after_drop: true,
            ..FakeConfig::default()
        },
        TEST_RATE,
        flag,
        Some(hook),
    );
    let service = attempt.result.unwrap();
    let state = attempt.state;
    let coordinator = thread::spawn(move || {
        before_receiver.recv().unwrap();
        let callback = thread::spawn(move || drop(state.invoke_f32(2, 1.0)));
        upgrade_receiver.recv().unwrap();
        let (lock, ready) = &*before_release;
        *lock.lock().unwrap() = true;
        ready.notify_one();
        assert!(!attempt_receiver.recv().unwrap());
        let (lock, ready) = &*upgrade_release;
        *lock.lock().unwrap() = true;
        ready.notify_one();
        callback.join().unwrap();
    });
    let shutdown = service.shutdown();
    coordinator.join().unwrap();
    assert!(shutdown.clean);
    assert!(!flag.load(Ordering::Acquire));
}

#[test]
fn clean_shutdown_releases_singleton_for_immediate_repeat() {
    let flag = fresh_lease_flag();
    let first = attempt_fake(FakeConfig::default(), TEST_RATE, flag)
        .result
        .unwrap();
    assert!(first.shutdown().clean);
    assert!(!flag.load(Ordering::Acquire));
    let second = attempt_fake(FakeConfig::default(), TEST_RATE, flag)
        .result
        .unwrap();
    assert!(second.shutdown().clean);
    assert!(!flag.load(Ordering::Acquire));
}

#[test]
fn uncertain_stream_drop_quarantines_callback_state_and_singleton() {
    let flag = fresh_lease_flag();
    let service = attempt_fake(
        FakeConfig {
            failure: FakeFailure::DropPanic,
            ..FakeConfig::default()
        },
        TEST_RATE,
        flag,
    )
    .result
    .unwrap();
    let shutdown = service.shutdown();
    assert!(!shutdown.clean);
    assert!(flag.load(Ordering::Acquire));
    let error = attempt_fake(FakeConfig::default(), TEST_RATE, flag)
        .result
        .unwrap_err();
    assert!(matches!(
        error,
        MixerStartError::Backend(ref error)
            if error.operation() == SystemOutputOperation::Setup
                && error.kind() == SystemOutputErrorKind::OutputInUse
    ));
}

#[test]
fn runtime_invalidation_reopens_output_without_sealing_admission() {
    let (mut service, state) = start_fake(FakeConfig::default());
    let mut failures = service.take_output_failure_events().unwrap();
    state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
        CpalErrorKind::StreamInvalidated,
    ));
    let _session = service.add_session(AudioSessionId(77)).unwrap();
    eventually(|| {
        state.build_count.load(Ordering::Acquire) == 2
            && state.play_count.load(Ordering::Acquire) == 2
    });
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, None);
    assert_eq!(block_on(failures.next()), None);
}

#[test]
fn missing_default_device_keeps_mixer_live_advances_cleanup_and_recovers_later() {
    let (service, state) = start_fake(FakeConfig::default());
    let session = service.add_session(AudioSessionId(80)).unwrap();
    let running = session
        .script_bus()
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(ConstantInput(0.25)))
        .unwrap();
    state.device_available.store(false, Ordering::Release);
    state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
        CpalErrorKind::DeviceNotAvailable,
    ));
    eventually(|| state.stream_drop_count.load(Ordering::Acquire) == 1);

    // Recovery is an endpoint condition, not a mixer failure. New sessions
    // and proof-bearing input cleanup continue against the null sink.
    let _sibling = service.add_session(AudioSessionId(81)).unwrap();
    assert_eq!(service.master_gain_authority().output_failure(), None);
    let (retired_sender, retired_receiver) = std::sync::mpsc::sync_channel(1);
    thread::spawn(move || {
        let _ = retired_sender.send(block_on(running.shutdown()));
    });
    assert!(
        retired_receiver
            .recv_timeout(Duration::from_secs(1))
            .unwrap()
            .unwrap()
            .is_clean()
    );

    state.device_available.store(true, Ordering::Release);
    eventually(|| {
        state.build_count.load(Ordering::Acquire) == 2
            && state.play_count.load(Ordering::Acquire) == 2
    });
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, None);
}

#[test]
fn stateful_source_advances_silently_through_device_loss_and_reopen() {
    let (service, state) = start_fake(FakeConfig::default());
    let session = service.add_session(AudioSessionId(82)).unwrap();
    let renders = Arc::new(AtomicUsize::new(0));
    let _running = session
        .script_bus()
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(StatefulInput {
            renders: Arc::clone(&renders),
            sequence: 0,
        }))
        .unwrap();
    eventually(|| {
        let _ = state.invoke_f32(256, 0.0);
        renders.load(Ordering::Acquire) != 0
    });
    let before_loss = renders.load(Ordering::Acquire);

    state.device_available.store(false, Ordering::Release);
    state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::with_message(
        CpalErrorKind::DeviceNotAvailable,
        "WASAPI endpoint vanished (HRESULT 0x88890004)",
    ));
    eventually(|| state.stream_drop_count.load(Ordering::Acquire) == 1);
    eventually(|| renders.load(Ordering::Acquire) > before_loss);
    let after_null_render = renders.load(Ordering::Acquire);

    state.device_available.store(true, Ordering::Release);
    eventually(|| {
        state.build_count.load(Ordering::Acquire) == 2
            && state.play_count.load(Ordering::Acquire) == 2
    });
    eventually(|| {
        let _ = state.invoke_f32(256, 0.0);
        renders.load(Ordering::Acquire) > after_null_render
    });

    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, None);
}

#[test]
fn recovery_catch_up_is_bounded_and_keeps_reopened_data_paused() {
    let state = Arc::new(FakeState::new(FakeConfig::default()));
    let status = Arc::new(crate::DriverStatus::new());
    let lease = OutputLease::acquire(fresh_lease_flag()).unwrap();
    let (mut mixer, _) = crate::MixerCore::<CpalOutputDriver<FakeFactory>>::with_limits(
        SystemDriverSettings {
            factory: FakeFactory(Arc::clone(&state)),
            lease,
            sample_rate: TEST_RATE,
            idle: OutputIdlePolicy::AlwaysOpen,
            proof_hook: None,
        },
        status,
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
        CpalErrorKind::DeviceNotAvailable,
    ));
    assert_eq!(
        mixer.maintain_output(Instant::now()),
        DriverMaintenance::Continue
    );

    let gap_start = Instant::now()
        .checked_sub(RECOVERY_MAX_NULL_ADVANCE.saturating_mul(4))
        .unwrap();
    {
        let driver = &mut mixer.manager.as_mut().unwrap().backend_mut().driver;
        let RecoveryState::Waiting {
            next_attempt,
            last_null_tick,
            ..
        } = &mut driver.recovery
        else {
            panic!("first maintenance pass did not enter recovery waiting");
        };
        *next_attempt = Instant::now();
        *last_null_tick = gap_start;
    }

    assert_eq!(
        mixer.maintain_output(Instant::now()),
        DriverMaintenance::Continue
    );
    {
        let driver = &mut mixer.manager.as_mut().unwrap().backend_mut().driver;
        let RecoveryState::CatchingUp { last_null_tick, .. } = &driver.recovery else {
            panic!("successful reopen did not remain in catch-up");
        };
        let advanced = last_null_tick.saturating_duration_since(gap_start);
        assert!(advanced > Duration::ZERO);
        assert!(advanced <= RECOVERY_MAX_NULL_ADVANCE);
        assert!(
            driver
                .callback
                .as_ref()
                .unwrap()
                .paused
                .load(Ordering::SeqCst)
        );
    }
    assert!(state.invoke_f32(8, 1.0).iter().all(|sample| *sample == 0.0));

    for _ in 0..8 {
        assert_eq!(
            mixer.maintain_output(Instant::now()),
            DriverMaintenance::Continue
        );
        let caught_up = {
            let driver = &mut mixer.manager.as_mut().unwrap().backend_mut().driver;
            matches!(&driver.recovery, RecoveryState::Active)
        };
        if caught_up {
            break;
        }
        assert!(state.invoke_f32(8, 1.0).iter().all(|sample| *sample == 0.0));
    }
    {
        let driver = &mut mixer.manager.as_mut().unwrap().backend_mut().driver;
        assert!(matches!(&driver.recovery, RecoveryState::Active));
        assert!(
            !driver
                .callback
                .as_ref()
                .unwrap()
                .paused
                .load(Ordering::SeqCst)
        );
    }
    assert!(crate::retire_backend(&mut mixer));
}

#[test]
fn provisional_endpoint_flapping_preserves_escalating_capped_backoff() {
    let (mut mixer, state) = direct_fake_mixer(FakeConfig::default());
    state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
        CpalErrorKind::DeviceNotAvailable,
    ));
    assert_eq!(
        mixer.maintain_output(Instant::now()),
        DriverMaintenance::Continue
    );

    let delays = [
        Duration::from_millis(100),
        Duration::from_millis(200),
        Duration::from_millis(400),
        Duration::from_millis(800),
        Duration::from_millis(1_600),
        Duration::from_millis(3_200),
        RECOVERY_MAX_BACKOFF,
        RECOVERY_MAX_BACKOFF,
    ];
    for (index, delay) in delays.into_iter().enumerate() {
        {
            let driver = &mut mixer.manager.as_mut().unwrap().backend_mut().driver;
            let RecoveryState::Waiting {
                attempt,
                next_attempt,
                backoff,
                ..
            } = &mut driver.recovery
            else {
                panic!("flapping endpoint did not wait before retry {index}");
            };
            assert_eq!(*attempt, u32::try_from(index).unwrap());
            assert_eq!(*backoff, delay);
            *next_attempt = Instant::now();
        }

        assert_eq!(
            mixer.maintain_output(Instant::now()),
            DriverMaintenance::Continue
        );
        {
            let driver = &mut mixer.manager.as_mut().unwrap().backend_mut().driver;
            let RecoveryState::CatchingUp {
                attempt, backoff, ..
            } = &driver.recovery
            else {
                panic!("retry {index} did not publish a provisional endpoint");
            };
            assert_eq!(*attempt, u32::try_from(index + 1).unwrap());
            assert_eq!(*backoff, delay);
        }
        state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
            CpalErrorKind::DeviceNotAvailable,
        ));
        assert_eq!(
            mixer.maintain_output(Instant::now()),
            DriverMaintenance::Continue
        );

        let builds = state.build_count.load(Ordering::Acquire);
        let next_attempt = {
            let driver = &mut mixer.manager.as_mut().unwrap().backend_mut().driver;
            let RecoveryState::Waiting {
                attempt,
                next_attempt,
                backoff,
                ..
            } = &driver.recovery
            else {
                panic!("failed provisional endpoint did not return to waiting");
            };
            assert_eq!(*attempt, u32::try_from(index + 1).unwrap());
            assert_eq!(*backoff, delay.saturating_mul(2).min(RECOVERY_MAX_BACKOFF));
            *next_attempt
        };
        assert_eq!(
            mixer.maintain_output(next_attempt.checked_sub(Duration::from_nanos(1)).unwrap()),
            DriverMaintenance::Continue
        );
        assert_eq!(state.build_count.load(Ordering::Acquire), builds);
    }
    assert!(crate::retire_backend(&mut mixer));
}

#[test]
fn activation_runtime_error_keeps_backoff_and_defers_the_next_retry() {
    let (mut mixer, state) = direct_fake_mixer(FakeConfig::default());
    state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
        CpalErrorKind::StreamInvalidated,
    ));
    assert_eq!(
        mixer.maintain_output(Instant::now()),
        DriverMaintenance::Continue
    );
    {
        let driver = &mut mixer.manager.as_mut().unwrap().backend_mut().driver;
        let RecoveryState::Waiting { next_attempt, .. } = &mut driver.recovery else {
            panic!("initial invalidation did not enter recovery");
        };
        *next_attempt = Instant::now();
    }
    assert_eq!(
        mixer.maintain_output(Instant::now()),
        DriverMaintenance::Continue
    );
    let driver = &mut mixer.manager.as_mut().unwrap().backend_mut().driver;
    assert!(matches!(
        &driver.recovery,
        RecoveryState::CatchingUp {
            attempt: 1,
            backoff: RECOVERY_INITIAL_BACKOFF,
            ..
        }
    ));
    driver
        .callback
        .as_ref()
        .unwrap()
        .error_admission
        .store(EndpointPhase::Failed as usize, Ordering::SeqCst);

    assert_eq!(
        mixer.maintain_output(Instant::now()),
        DriverMaintenance::Continue
    );
    let builds = state.build_count.load(Ordering::Acquire);
    let next_attempt = {
        let driver = &mut mixer.manager.as_mut().unwrap().backend_mut().driver;
        let RecoveryState::Waiting {
            attempt,
            next_attempt,
            backoff,
            ..
        } = &driver.recovery
        else {
            panic!("activation error did not schedule another attempt");
        };
        assert_eq!(*attempt, 1);
        assert_eq!(*backoff, RECOVERY_INITIAL_BACKOFF.saturating_mul(2));
        *next_attempt
    };
    assert_eq!(
        mixer.maintain_output(next_attempt.checked_sub(Duration::from_nanos(1)).unwrap()),
        DriverMaintenance::Continue
    );
    assert_eq!(state.build_count.load(Ordering::Acquire), builds);
    assert!(crate::retire_backend(&mut mixer));
}

#[test]
fn long_owner_suspend_discards_wall_clock_backlog_and_reactivates_once() {
    let state = Arc::new(FakeState::new(FakeConfig::default()));
    let status = Arc::new(crate::DriverStatus::new());
    let lease = OutputLease::acquire(fresh_lease_flag()).unwrap();
    let (mut mixer, _) = crate::MixerCore::<CpalOutputDriver<FakeFactory>>::with_limits(
        SystemDriverSettings {
            factory: FakeFactory(Arc::clone(&state)),
            lease,
            sample_rate: TEST_RATE,
            idle: OutputIdlePolicy::AlwaysOpen,
            proof_hook: None,
        },
        status,
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
        CpalErrorKind::DeviceNotAvailable,
    ));
    assert_eq!(
        mixer.maintain_output(Instant::now()),
        DriverMaintenance::Continue
    );
    assert_eq!(
        mixer.maintain_output(Instant::now()),
        DriverMaintenance::Continue
    );

    let suspended_since = Instant::now().checked_sub(Duration::from_hours(4)).unwrap();
    {
        let driver = &mut mixer.manager.as_mut().unwrap().backend_mut().driver;
        assert!(matches!(&driver.recovery, RecoveryState::CatchingUp { .. }));
        driver.last_maintenance_at = suspended_since;
        let RecoveryState::CatchingUp { last_null_tick, .. } = &mut driver.recovery else {
            unreachable!();
        };
        *last_null_tick = suspended_since;
    }

    // One owner pass after a true long suspend rebases the non-runnable gap;
    // it does not preserve hours of backlog for 100 ms-at-a-time catch-up.
    assert_eq!(
        mixer.maintain_output(Instant::now()),
        DriverMaintenance::Continue
    );
    let driver = &mut mixer.manager.as_mut().unwrap().backend_mut().driver;
    assert!(matches!(&driver.recovery, RecoveryState::Active));
    assert!(
        !driver
            .callback
            .as_ref()
            .unwrap()
            .paused
            .load(Ordering::SeqCst)
    );
    assert!(crate::retire_backend(&mut mixer));
}

#[test]
fn recovery_capable_driver_owns_stall_while_retiring_session_waits() {
    let state = Arc::new(FakeState::new(FakeConfig {
        recovery_enumerate_delay: Duration::from_millis(220),
        stream_drop_delay: Duration::from_millis(120),
        ..FakeConfig::default()
    }));
    let status = Arc::new(crate::DriverStatus::new());
    let lease = OutputLease::acquire(fresh_lease_flag()).unwrap();
    let (mut mixer, _) = crate::MixerCore::<CpalOutputDriver<FakeFactory>>::with_limits(
        SystemDriverSettings {
            factory: FakeFactory(Arc::clone(&state)),
            lease,
            sample_rate: TEST_RATE,
            idle: OutputIdlePolicy::AlwaysOpen,
            proof_hook: None,
        },
        Arc::clone(&status),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let retiring = mixer.add_session(AudioSessionId(83)).unwrap();
    let _ = state.invoke_f32(256, 0.0);
    retiring.control.begin_close().unwrap();
    let completed = retiring.retirement;
    assert!(
        mixer
            .begin_session_retirement(crate::SessionRetirementRequest {
                key: retiring.control.key,
            })
            .is_ok()
    );
    mixer.close_drained_session_inputs();
    mixer.drop_ready_session_tracks();
    let observed_epoch = status.callback_epoch();
    let retirement = mixer
        .sessions
        .get_mut(&AudioSessionId(83))
        .unwrap()
        .retirement
        .as_mut()
        .unwrap();
    retirement.tracks_dropped_at = Some(
        Instant::now()
            .checked_sub(crate::SESSION_TRACK_RETIREMENT_TIMEOUT)
            .unwrap(),
    );
    retirement.callback_stall = Some(crate::CallbackStallObservation {
        epoch: observed_epoch,
        observed_at: Instant::now()
            .checked_sub(crate::SESSION_TRACK_RETIREMENT_TIMEOUT)
            .unwrap(),
    });
    mixer.reported_sub_track_count = Some(1);

    state.device_available.store(false, Ordering::Release);
    state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
        CpalErrorKind::DeviceNotAvailable,
    ));
    assert_eq!(
        mixer.maintain_output(Instant::now()),
        DriverMaintenance::Continue
    );
    let after_drop_epoch = status.callback_epoch();
    assert!(after_drop_epoch > observed_epoch);
    let retirement = mixer
        .sessions
        .get_mut(&AudioSessionId(83))
        .unwrap()
        .retirement
        .as_mut()
        .unwrap();
    retirement.callback_stall = Some(crate::CallbackStallObservation {
        epoch: after_drop_epoch,
        observed_at: Instant::now()
            .checked_sub(crate::SESSION_TRACK_RETIREMENT_TIMEOUT)
            .unwrap(),
    });
    // The session already had a terminal-looking generic stall observation,
    // but CPAL has now entered its own recovery state and is the sole authority.
    assert!(mixer.complete_rendered_session_retirements(&status));
    assert_eq!(status.failure(), None);

    let attempt_started = Instant::now();
    assert_eq!(
        mixer.maintain_output(Instant::now()),
        DriverMaintenance::Continue
    );
    assert!(attempt_started.elapsed() >= Duration::from_millis(200));
    assert_eq!(state.build_count.load(Ordering::Acquire), 1);
    assert!(status.callback_epoch() > after_drop_epoch);
    assert!(mixer.complete_rendered_session_retirements(&status));
    assert_eq!(status.failure(), None);

    mixer.reported_sub_track_count = Some(0);
    assert!(mixer.complete_rendered_session_retirements(&status));
    assert_eq!(block_on(completed).unwrap(), Ok(()));
    assert!(crate::retire_backend(&mut mixer));
}

#[test]
fn permanent_runtime_error_terminalizes_instead_of_retrying_forever() {
    let (mut service, state) = start_fake(FakeConfig::default());
    let master = service.master_gain_authority();
    state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
        CpalErrorKind::InvalidInput,
    ));
    eventually(|| master.output_failure() == Some(MixerOutputFailure::BackendFailure));
    // The owner buffers the one terminal transition even before the unique
    // receiver is taken, including when no sessions exist.
    let mut failures = service.take_output_failure_events().unwrap();
    assert_eq!(
        block_on(failures.next()),
        Some(MixerOutputFailure::BackendFailure)
    );
    assert_eq!(state.build_count.load(Ordering::Acquire), 1);
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
}

#[test]
fn recovery_protocol_failure_terminalizes_instead_of_retrying() {
    let (mut service, state) = start_fake(FakeConfig {
        recovery_protocol_failure: true,
        ..FakeConfig::default()
    });
    let mut failures = service.take_output_failure_events().unwrap();
    state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
        CpalErrorKind::DeviceNotAvailable,
    ));
    assert_eq!(
        block_on(failures.next()),
        Some(MixerOutputFailure::BackendFailure)
    );
    assert_eq!(state.build_count.load(Ordering::Acquire), 1);
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
    assert_eq!(block_on(failures.next()), None);
}

#[test]
fn owner_panic_publishes_one_terminal_output_event() {
    let (mut service, _state) = start_fake(FakeConfig::default());
    let mut failures = service.take_output_failure_events().unwrap();
    service
        .driver_status
        .panic_owner
        .store(true, Ordering::Release);
    assert_eq!(
        block_on(failures.next()),
        Some(MixerOutputFailure::OwnerPanicked)
    );
    let shutdown = service.shutdown();
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::OwnerPanicked));
}

#[test]
fn clean_shutdown_closes_terminal_output_events_without_an_item() {
    let (mut service, _state) = start_fake(FakeConfig::default());
    let mut failures = service.take_output_failure_events().unwrap();
    assert!(service.take_output_failure_events().is_none());
    assert!(service.shutdown().clean);
    assert_eq!(block_on(failures.next()), None);
}

#[test]
fn silent_callback_stall_rebuilds_the_endpoint() {
    let (service, state) = start_fake(FakeConfig::default());
    let deadline = Instant::now() + Duration::from_secs(5);
    while state.build_count.load(Ordering::Acquire) < 2 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        state.build_count.load(Ordering::Acquire) >= 2,
        "the callback watchdog did not rebuild the stalled stream"
    );
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, None);
}

#[test]
fn recovery_never_opens_a_replacement_after_unproven_stream_teardown() {
    let (service, state) = start_fake(FakeConfig {
        failure: FakeFailure::DropPanic,
        ..FakeConfig::default()
    });
    let master = service.master_gain_authority();
    state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
        CpalErrorKind::StreamInvalidated,
    ));
    eventually(|| master.output_failure() == Some(MixerOutputFailure::BackendFailure));
    assert_eq!(state.build_count.load(Ordering::Acquire), 1);
    let shutdown = service.shutdown();
    assert!(!shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
}

#[test]
fn advisory_runtime_notifications_leave_output_and_admission_live() {
    let (service, state) = start_fake(FakeConfig::default());
    for kind in [
        CpalErrorKind::DeviceChanged,
        CpalErrorKind::RealtimeDenied,
        CpalErrorKind::Xrun,
    ] {
        state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(kind));
    }
    let _session = service.add_session(AudioSessionId(79)).unwrap();
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, None);
}

struct ObservedInput {
    observer: Arc<ObservedFailure>,
}

impl MixerInput for ObservedInput {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        output.fill(MixerFrame::ZERO);
        MixerInputStatus::Active
    }

    fn output_failure_observer(&self) -> Option<Arc<dyn crate::MixerFailureObserver>> {
        Some(self.observer.clone())
    }
}

struct ObservedFailure {
    sender: Mutex<Option<std::sync::mpsc::SyncSender<(MixerOutputFailure, String)>>>,
}

impl crate::MixerFailureObserver for ObservedFailure {
    fn output_failed(&self, failure: MixerOutputFailure) {
        if let Some(sender) = self.sender.lock().unwrap().take() {
            let _ = sender.send((
                failure,
                thread::current().name().unwrap_or("unnamed").to_owned(),
            ));
        }
    }
}

#[test]
fn recoverable_device_loss_preserves_installed_inputs_without_failure_notification() {
    let (service, state) = start_fake(FakeConfig::default());
    let session = service.add_session(AudioSessionId(78)).unwrap();
    let (sender, receiver) = std::sync::mpsc::sync_channel(1);
    let observer = Arc::new(ObservedFailure {
        sender: Mutex::new(Some(sender)),
    });
    let _running = session
        .script_bus()
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(ObservedInput { observer }))
        .unwrap();
    state.callbacks.lock().unwrap().error.as_mut().unwrap()(cpal::Error::new(
        CpalErrorKind::DeviceNotAvailable,
    ));
    eventually(|| state.build_count.load(Ordering::Acquire) == 2);
    assert!(matches!(
        receiver.try_recv(),
        Err(std::sync::mpsc::TryRecvError::Empty)
    ));
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, None);
}

#[test]
fn overlapping_callback_entry_is_refused_and_latched() {
    let status = Arc::new(crate::DriverStatus::new());
    let state = CallbackState::new(DriverFailureSignal(Arc::downgrade(&status)), None, None);
    let active = state.enter(state.generation()).unwrap();
    assert!(state.enter(state.generation()).is_none());
    assert_eq!(status.failure(), Some(MixerOutputFailure::BackendFailure));
    drop(active);
    assert_eq!(state.active.load(Ordering::Acquire), 0);
}

#[test]
fn callback_paused_between_first_check_and_claim_cannot_enter_new_generation() {
    let status = Arc::new(crate::DriverStatus::new());
    let (entered, entered_receiver) = std::sync::mpsc::sync_channel(1);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let hook = Arc::new(TestAdmissionHook {
        entered,
        release: Arc::clone(&release),
        armed: AtomicBool::new(true),
    });
    let state = Arc::new(CallbackState::new(
        DriverFailureSignal(Arc::downgrade(&status)),
        None,
        Some(hook),
    ));
    let old_generation = state.generation();
    let callback_state = Arc::clone(&state);
    let callback = thread::spawn(move || callback_state.enter(old_generation).is_some());
    entered_receiver.recv().unwrap();

    state.pause();
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    let new_generation = state.begin_stream_generation().unwrap();
    let (lock, ready) = &*release;
    *lock.lock().unwrap() = true;
    ready.notify_one();

    assert!(!callback.join().unwrap());
    assert_eq!(state.active.load(Ordering::SeqCst), 0);
    assert_eq!(state.generation(), new_generation);
}

#[test]
fn error_callback_paused_between_first_check_and_claim_cannot_enter_new_generation() {
    let status = Arc::new(crate::DriverStatus::new());
    let (entered, entered_receiver) = std::sync::mpsc::sync_channel(1);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let hook = Arc::new(TestAdmissionHook {
        entered,
        release: Arc::clone(&release),
        armed: AtomicBool::new(true),
    });
    let state = Arc::new(CallbackState::new(
        DriverFailureSignal(Arc::downgrade(&status)),
        None,
        Some(hook),
    ));
    let old_generation = state.generation();
    let callback_state = Arc::clone(&state);
    let callback = thread::spawn(move || {
        callback_state
            .enter_error_callback(old_generation, DriverRuntimeEvent::BackendError)
            .is_some()
    });
    entered_receiver.recv().unwrap();

    state.pause();
    assert_eq!(
        state.error_admission.load(Ordering::SeqCst) & !ERROR_ADMISSION_PHASE_MASK,
        0
    );
    let new_generation = state.begin_stream_generation().unwrap();
    let (lock, ready) = &*release;
    *lock.lock().unwrap() = true;
    ready.notify_one();

    assert!(!callback.join().unwrap());
    assert_eq!(
        state.error_admission.load(Ordering::SeqCst) & !ERROR_ADMISSION_PHASE_MASK,
        0
    );
    assert_eq!(state.generation(), new_generation);
    assert_eq!(
        EndpointPhase::from_admission(state.error_admission.load(Ordering::SeqCst)),
        Some(EndpointPhase::Provisional)
    );
}

#[test]
fn recovery_handoff_cannot_admit_data_ahead_of_a_concurrent_error() {
    let status = Arc::new(crate::DriverStatus::new());
    let (entered, entered_receiver) = std::sync::mpsc::sync_channel(1);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let hook = Arc::new(TestAdmissionHook {
        entered,
        release: Arc::clone(&release),
        armed: AtomicBool::new(true),
    });
    let state = Arc::new(CallbackState::new(
        DriverFailureSignal(Arc::downgrade(&status)),
        None,
        Some(hook),
    ));
    state.pause();
    let generation = state.begin_stream_generation().unwrap();
    assert!(state.activate_error_callbacks(generation));
    let mut data = callback_for(&state, generation, DriverSampleFormat::F32);
    let (mut error, mut errors) = runtime_error_channel(&state, generation);

    let activation_state = Arc::clone(&state);
    let activation = thread::spawn(move || activation_state.activate_generation(generation));
    entered_receiver.recv().unwrap();

    // The native error callback wins the same packed admission-word CAS that
    // recovery activation is waiting to perform.
    error(cpal::Error::new(CpalErrorKind::DeviceNotAvailable));
    let mut output = [1.0; 8];
    data(Some(OutputBuffer::F32(&mut output)));
    assert!(output.iter().all(|sample| *sample == 0.0));

    let (lock, ready) = &*release;
    *lock.lock().unwrap() = true;
    ready.notify_one();
    assert_eq!(
        activation.join().unwrap(),
        GenerationActivation::RuntimeError
    );
    assert!(state.paused.load(Ordering::SeqCst));
    assert_eq!(
        state.take_recovery_event(),
        Some(DriverRuntimeEvent::DeviceNotAvailable)
    );
    assert_eq!(
        errors.consumer.pop().unwrap().kind(),
        CpalErrorKind::DeviceNotAvailable
    );
}

#[test]
fn retired_stream_generation_cannot_render_or_terminalize_the_replacement() {
    let status = Arc::new(crate::DriverStatus::new());
    let state = Arc::new(CallbackState::new(
        DriverFailureSignal(Arc::downgrade(&status)),
        None,
        None,
    ));
    let old_generation = state.generation();
    let mut old_data = callback_for(&state, old_generation, DriverSampleFormat::F32);
    let (mut old_error, mut old_errors) = runtime_error_channel(&state, old_generation);
    state.pause();
    let new_generation = state.begin_stream_generation().unwrap();
    assert!(state.activate_error_callbacks(new_generation));
    assert_eq!(
        state.activate_generation(new_generation),
        GenerationActivation::Activated
    );

    let mut output = [1.0; 8];
    old_data(Some(OutputBuffer::F32(&mut output)));
    assert!(output.iter().all(|sample| *sample == 0.0));
    old_data(None);
    let mut opaque_prefill = [0x69_u8; 8];
    // SAFETY: the pointer, length, and declared sample format exactly describe
    // the live opaque buffer for the duration of this call.
    let mut raw = unsafe {
        cpal::Data::from_parts(
            opaque_prefill.as_mut_ptr().cast::<()>(),
            opaque_prefill.len(),
            SampleFormat::DsdU8,
        )
    };
    dispatch_raw_output(&mut raw, &mut old_data);
    assert_eq!(opaque_prefill, [0x69; 8]);
    old_error(cpal::Error::new(CpalErrorKind::InvalidInput));
    assert_eq!(state.take_recovery_event(), None);
    assert_eq!(status.failure(), None);
    assert!(old_errors.consumer.pop().is_err());
}

#[test]
fn error_queue_retirement_waits_for_an_accepted_callback_to_finish() {
    let status = Arc::new(crate::DriverStatus::new());
    let state = Arc::new(CallbackState::new(
        DriverFailureSignal(Arc::downgrade(&status)),
        None,
        None,
    ));
    let active = state
        .enter_error_callback(state.generation(), DriverRuntimeEvent::BackendError)
        .unwrap();
    let waiter_state = Arc::clone(&state);
    let (finished, receiver) = std::sync::mpsc::sync_channel(1);
    let waiter = thread::spawn(move || {
        waiter_state.wait_for_error_callbacks();
        finished.send(()).unwrap();
    });
    assert!(receiver.recv_timeout(Duration::from_millis(20)).is_err());
    drop(active);
    receiver.recv_timeout(Duration::from_secs(1)).unwrap();
    waiter.join().unwrap();
}

#[test]
fn runtime_error_queue_preserves_exact_first_detail_and_counts_overflow() {
    let status = Arc::new(crate::DriverStatus::new());
    let state = Arc::new(CallbackState::new(
        DriverFailureSignal(Arc::downgrade(&status)),
        None,
        None,
    ));
    let generation = state.generation();
    let (mut callback, mut errors) = runtime_error_channel(&state, generation);
    let exact = "WASAPI device invalidated (HRESULT 0x88890004)";
    callback(cpal::Error::with_message(
        CpalErrorKind::DeviceChanged,
        exact,
    ));
    for index in 1..=RUNTIME_ERROR_QUEUE_CAPACITY {
        callback(cpal::Error::with_message(
            CpalErrorKind::DeviceChanged,
            format!("overflow probe {index}"),
        ));
    }

    let first = errors.consumer.pop().unwrap();
    assert_eq!(first.kind(), CpalErrorKind::DeviceChanged);
    assert_eq!(first.message(), Some(exact));
    assert_eq!(first.to_string(), exact);
    assert_eq!(errors.overflow.load(Ordering::Acquire), 1);
    assert_eq!(
        state.take_recovery_event(),
        Some(DriverRuntimeEvent::BackendError)
    );
    drop(first);
    drop(callback);
    while let Ok(error) = errors.consumer.pop() {
        drop(error);
    }
}

#[test]
fn ordinary_service_drop_releases_singleton_after_autonomous_proven_cleanup() {
    let flag = fresh_lease_flag();
    let service = attempt_fake(FakeConfig::default(), TEST_RATE, flag)
        .result
        .unwrap();
    drop(service);
    for _ in 0..1_000 {
        if !flag.load(Ordering::Acquire) {
            return;
        }
        thread::sleep(Duration::from_millis(1));
    }
    panic!("autonomous owner cleanup did not release the proven singleton lease");
}

#[test]
fn public_errors_and_private_settings_have_the_intended_traits() {
    fn assert_error<T: std::error::Error + Send + Sync + 'static>() {}
    fn assert_send<T: Send>() {}
    trait AmbiguousIfSend<Marker> {
        fn assert_not_send() {}
    }
    trait AmbiguousIfSync<Marker> {
        fn assert_not_sync() {}
    }
    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
    impl<T: ?Sized> AmbiguousIfSync<()> for T {}
    impl<T: ?Sized + Sync> AmbiguousIfSync<u8> for T {}
    assert_error::<SystemOutputError>();
    assert_send::<SystemDriverSettings<FakeFactory>>();
    let _ = <SystemMixerService as AmbiguousIfSend<_>>::assert_not_send;
    let _ = <SystemMixerService as AmbiguousIfSync<_>>::assert_not_sync;
}

#[test]
fn system_wrapper_forwards_only_the_scoped_service_surface() {
    fn assert_clone_send_sync<T: Clone + Send + Sync>() {}
    assert_clone_send_sync::<crate::MixerSessionRegistrar>();

    let attempt = attempt_fake(FakeConfig::default(), TEST_RATE, fresh_lease_flag());
    let mut service = SystemMixerService(attempt.result.unwrap());
    let mut failures = service.take_output_failure_events().unwrap();
    assert_eq!(service.format().sample_rate(), TEST_RATE);
    let session = service
        .session_registrar()
        .add_session(AudioSessionId(1234))
        .unwrap();
    assert_eq!(session.script_bus().format().unwrap(), service.format());
    assert!(service.shutdown().clean);
    assert_eq!(block_on(failures.next()), None);
}

#[test]
#[ignore = "manual default-device lifecycle smoke"]
fn manual_default_device_silent_open_suspend_resume_close_and_repeat() {
    let service = SystemMixerService::start(TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(999)).unwrap();
    let running = session
        .script_bus()
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(ConstantInput(0.0)))
        .unwrap();
    thread::sleep(Duration::from_millis(50));
    assert!(running.suspend());
    thread::sleep(Duration::from_millis(20));
    assert!(running.resume());
    thread::sleep(Duration::from_millis(50));
    assert!(block_on(running.shutdown()).unwrap().is_clean());
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert!(shutdown.failure.is_none());

    let repeat = SystemMixerService::start(TEST_RATE).unwrap();
    let shutdown = repeat.shutdown();
    assert!(shutdown.clean);
    assert!(shutdown.failure.is_none());
}

const RAMP_STEP: f32 = 1e-6;

fn start_fake_idle(config: FakeConfig, after: Duration) -> (MixerService, Arc<FakeState>) {
    let state = Arc::new(FakeState::new(config));
    let lease = OutputLease::acquire(fresh_lease_flag()).unwrap();
    let service = MixerService::start_with_driver::<CpalOutputDriver<FakeFactory>>(
        SystemDriverSettings {
            factory: FakeFactory(Arc::clone(&state)),
            lease,
            sample_rate: TEST_RATE,
            idle: OutputIdlePolicy::DetachWhenIdle { after },
            proof_hook: None,
        },
        TEST_RATE,
    )
    .unwrap();
    (service, state)
}

/// Renders a level the test flips at will. Above zero it ramps by one step
/// per frame, so the first sample a stream delivers identifies exactly which
/// rendered frame it came from.
struct DemandInput {
    level: Arc<std::sync::atomic::AtomicU32>,
    ramp: u32,
}

impl MixerInput for DemandInput {
    #[allow(
        clippy::cast_precision_loss,
        reason = "the ramp stays far below the f32 mantissa within any test"
    )]
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        let level = f32::from_bits(self.level.load(Ordering::Acquire));
        for frame in output {
            let sample = if level == 0.0 {
                0.0
            } else {
                self.ramp += 1;
                level + self.ramp as f32 * RAMP_STEP
            };
            *frame = MixerFrame::from_mono(sample);
        }
        MixerInputStatus::Active
    }
}

fn install_demand(
    service: &MixerService,
    level: f32,
) -> (
    crate::MixerSessionOwner,
    crate::RunningMixerInput,
    Arc<std::sync::atomic::AtomicU32>,
) {
    let level = Arc::new(std::sync::atomic::AtomicU32::new(level.to_bits()));
    let session = service.add_session(AudioSessionId(1)).unwrap();
    let running = session
        .script_bus()
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(DemandInput {
            level: Arc::clone(&level),
            ramp: 0,
        }))
        .unwrap();
    (session, running, level)
}

fn lifecycle_count(state: &FakeState, operation: &str) -> usize {
    state
        .lifecycle
        .lock()
        .unwrap()
        .iter()
        .filter(|(recorded, _)| *recorded == operation)
        .count()
}

#[test]
fn demand_driven_start_verifies_the_endpoint_without_a_stream() {
    let (service, state) = start_fake_idle(FakeConfig::default(), Duration::from_millis(50));
    let (session, running, _level) = install_demand(&service, 0.0);
    thread::sleep(Duration::from_millis(150));
    assert_eq!(state.build_count.load(Ordering::Acquire), 0);
    assert_eq!(state.play_count.load(Ordering::Acquire), 0);
    // Input retirement is proven by owner-side null rendering while no
    // native stream exists.
    assert!(block_on(running.shutdown()).unwrap().is_clean());
    drop(session);
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, None);
    assert_eq!(lifecycle_count(&state, "stream-build"), 0);
    assert_eq!(lifecycle_count(&state, "stream-play"), 0);
    for required in [
        "host-create",
        "default-device",
        "supported-configs",
        "device-drop",
        "host-drop",
    ] {
        assert_eq!(lifecycle_count(&state, required), 1, "{required}");
    }
}

#[test]
fn demand_driven_output_attaches_on_the_first_audible_frame_without_losing_it() {
    let (service, state) = start_fake_idle(FakeConfig::default(), Duration::from_secs(10));
    let (session, running, level) = install_demand(&service, 0.0);
    thread::sleep(Duration::from_millis(100));
    assert_eq!(state.build_count.load(Ordering::Acquire), 0);
    level.store(0.5f32.to_bits(), Ordering::Release);
    eventually(|| state.play_count.load(Ordering::Acquire) == 1);
    let mut first = None;
    eventually(|| {
        let output = state.invoke_f32(512, 9.0);
        first = output.iter().copied().find(|sample| *sample != 0.0);
        first.is_some()
    });
    let first = first.unwrap();
    assert!(
        (first - (0.5 + RAMP_STEP)).abs() < RAMP_STEP / 2.0,
        "the first delivered sample {first} is not the first audible frame"
    );
    assert_eq!(state.build_count.load(Ordering::Acquire), 1);
    let shutdown = running.shutdown();
    drop(state.invoke_f32(2, 9.0));
    assert!(block_on(shutdown).unwrap().is_clean());
    drop(session);
    assert!(service.shutdown().clean);
}

#[test]
fn demand_driven_output_releases_after_continuous_silence_and_reattaches() {
    let (service, state) = start_fake_idle(
        FakeConfig {
            retain_callback_after_drop: true,
            ..FakeConfig::default()
        },
        Duration::from_millis(100),
    );
    let (session, running, level) = install_demand(&service, 0.5);
    eventually(|| state.play_count.load(Ordering::Acquire) == 1);
    // Audible callbacks hold the stream well past the idle threshold.
    let held_until = Instant::now() + Duration::from_millis(300);
    while Instant::now() < held_until {
        drop(state.invoke_f32(64, 9.0));
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(state.stream_drop_count.load(Ordering::Acquire), 0);
    level.store(0.0f32.to_bits(), Ordering::Release);
    // Silent callbacks are not demand. They stop before the threshold so no
    // fake invocation is in flight when the owner destroys the stream; the
    // real stream destructor joins the native callback, the fake does not.
    let silent_until = Instant::now() + Duration::from_millis(40);
    while Instant::now() < silent_until {
        drop(state.invoke_f32(64, 9.0));
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(state.stream_drop_count.load(Ordering::Acquire), 0);
    eventually(|| state.stream_drop_count.load(Ordering::Acquire) == 1);
    assert_eq!(state.build_count.load(Ordering::Acquire), 1);
    level.store(0.5f32.to_bits(), Ordering::Release);
    eventually(|| state.play_count.load(Ordering::Acquire) == 2);
    eventually(|| {
        state
            .invoke_f32(64, 9.0)
            .iter()
            .any(|sample| *sample != 0.0)
    });
    let shutdown = running.shutdown();
    drop(state.invoke_f32(2, 9.0));
    assert!(block_on(shutdown).unwrap().is_clean());
    drop(session);
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(state.stream_drop_count.load(Ordering::Acquire), 2);
}

#[test]
fn demand_driven_attach_failure_recovers_with_backoff() {
    let (service, state) = start_fake_idle(FakeConfig::default(), Duration::from_secs(10));
    let (session, running, level) = install_demand(&service, 0.0);
    state.device_available.store(false, Ordering::Release);
    level.store(0.5f32.to_bits(), Ordering::Release);
    // The startup enumeration plus at least two failed attach attempts.
    eventually(|| lifecycle_count(&state, "default-device") >= 3);
    assert_eq!(state.build_count.load(Ordering::Acquire), 0);
    state.device_available.store(true, Ordering::Release);
    eventually(|| state.play_count.load(Ordering::Acquire) == 1);
    eventually(|| {
        state
            .invoke_f32(64, 9.0)
            .iter()
            .any(|sample| *sample != 0.0)
    });
    let shutdown = running.shutdown();
    drop(state.invoke_f32(2, 9.0));
    assert!(block_on(shutdown).unwrap().is_clean());
    drop(session);
    assert!(service.shutdown().clean);
}
