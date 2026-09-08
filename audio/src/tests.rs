use std::{
    future::Future,
    pin::Pin,
    sync::{
        Arc, Barrier, Condvar, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc,
    },
    task::{Context, Poll, Wake, Waker},
    thread,
    time::{Duration, Instant},
};

use super::*;
use futures::{StreamExt, executor::block_on};

const TEST_RATE: u32 = 48_000;

#[test]
fn mixer_frame_owns_stereo_samples_and_maps_to_the_backend() {
    let frames = [MixerFrame::new(0.25, -0.5), MixerFrame::from_mono(0.75)];
    assert!((frames[0].left() - 0.25).abs() < f32::EPSILON);
    assert!((frames[0].right() + 0.5).abs() < f32::EPSILON);
    assert_eq!(MixerFrame::ZERO, MixerFrame::new(0.0, 0.0));

    let mut backend = [KiraFrame::ZERO; 2];
    copy_mixer_frames(&mut backend, &frames);
    assert_eq!(backend[0], KiraFrame::new(0.25, -0.5));
    assert_eq!(backend[1], KiraFrame::from_mono(0.75));
}

#[derive(Clone, Default)]
struct RenderProbe {
    renderer: Arc<Mutex<Option<JoinedRenderer>>>,
    failure: Arc<Mutex<Option<DriverFailureSignal>>>,
}

impl RenderProbe {
    fn render(&self, output: &mut [f32]) {
        let mut renderer = self.renderer.lock().expect("renderer lock poisoned");
        let renderer = renderer.as_mut().expect("renderer is not live");
        renderer.render(output, 2).expect("valid fake render");
    }

    fn is_live(&self) -> bool {
        self.renderer
            .lock()
            .expect("renderer lock poisoned")
            .is_some()
    }

    fn fail_output(&self) -> bool {
        self.failure
            .lock()
            .expect("failure lock poisoned")
            .as_ref()
            .is_some_and(|signal| signal.report(MixerOutputFailure::BackendFailure))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestBackendError {
    Setup,
    Start,
}

#[derive(Clone)]
struct TestBackendSettings {
    probe: RenderProbe,
    actual_rate: u32,
    fail_setup: bool,
    fail_start: bool,
    backend_closes: Arc<AtomicUsize>,
    backend_drops: Arc<AtomicUsize>,
    lifecycle_threads: Arc<Mutex<Vec<thread::ThreadId>>>,
    enforce_owner_thread: bool,
}

struct TestBackend {
    probe: RenderProbe,
    fail_start: bool,
    backend_closes: Arc<AtomicUsize>,
    backend_drops: Arc<AtomicUsize>,
    lifecycle_threads: Arc<Mutex<Vec<thread::ThreadId>>>,
    enforce_owner_thread: bool,
}

fn record_backend_owner(threads: &Mutex<Vec<thread::ThreadId>>, enforce_owner_thread: bool) {
    if enforce_owner_thread {
        assert_eq!(thread::current().name(), Some("smudgy-audio-owner"));
    }
    lock_recover(threads).push(thread::current().id());
}

impl JoinedOutputDriver for TestBackend {
    type Settings = TestBackendSettings;
    type Error = TestBackendError;
    const CALLBACK_STALL_POLICY: CallbackStallPolicy = CallbackStallPolicy::Manual;

    fn setup(
        settings: Self::Settings,
        _internal_buffer_size: usize,
        failures: DriverFailureSignal,
    ) -> Result<(Self, PhysicalOutputFormat), Self::Error> {
        record_backend_owner(&settings.lifecycle_threads, settings.enforce_owner_thread);
        if settings.fail_setup {
            return Err(TestBackendError::Setup);
        }
        *settings
            .probe
            .failure
            .lock()
            .expect("failure lock poisoned") = Some(failures);
        Ok((
            Self {
                probe: settings.probe,
                fail_start: settings.fail_start,
                backend_closes: settings.backend_closes,
                backend_drops: settings.backend_drops,
                lifecycle_threads: settings.lifecycle_threads,
                enforce_owner_thread: settings.enforce_owner_thread,
            },
            PhysicalOutputFormat {
                sample_rate: settings.actual_rate,
                channels: 2,
                sample_format: PhysicalSampleFormat::F32,
                buffer_frames_hint: Some(INTERNAL_BUFFER_FRAMES),
            },
        ))
    }

    fn start(&mut self, renderer: JoinedRenderer) -> Result<(), Self::Error> {
        record_backend_owner(&self.lifecycle_threads, self.enforce_owner_thread);
        if self.fail_start {
            return Err(TestBackendError::Start);
        }
        let replaced = self
            .probe
            .renderer
            .lock()
            .expect("renderer lock poisoned")
            .replace(renderer);
        assert!(replaced.is_none());
        Ok(())
    }

    fn play(&mut self) -> Result<(), Self::Error> {
        record_backend_owner(&self.lifecycle_threads, self.enforce_owner_thread);
        Ok(())
    }

    fn close_and_join(&mut self) -> bool {
        record_backend_owner(&self.lifecycle_threads, self.enforce_owner_thread);
        self.backend_closes.fetch_add(1, Ordering::Relaxed);
        self.probe
            .renderer
            .lock()
            .expect("renderer lock poisoned")
            .take();
        true
    }
}

impl Drop for TestBackend {
    fn drop(&mut self) {
        record_backend_owner(&self.lifecycle_threads, self.enforce_owner_thread);
        self.backend_drops.fetch_add(1, Ordering::Relaxed);
    }
}

struct MixerManagedTestBackend(TestBackend);

impl JoinedOutputDriver for MixerManagedTestBackend {
    type Settings = TestBackendSettings;
    type Error = TestBackendError;

    fn setup(
        settings: Self::Settings,
        internal_buffer_size: usize,
        failures: DriverFailureSignal,
    ) -> Result<(Self, PhysicalOutputFormat), Self::Error> {
        let (driver, format) = TestBackend::setup(settings, internal_buffer_size, failures)?;
        Ok((Self(driver), format))
    }

    fn start(&mut self, renderer: JoinedRenderer) -> Result<(), Self::Error> {
        self.0.start(renderer)
    }

    fn play(&mut self) -> Result<(), Self::Error> {
        self.0.play()
    }

    fn close_and_join(&mut self) -> bool {
        self.0.close_and_join()
    }
}

struct UnjoinedBackend {
    probe: RenderProbe,
    panic_close: bool,
}

struct UnjoinedSettings {
    probe: RenderProbe,
    panic_close: bool,
}

impl JoinedOutputDriver for UnjoinedBackend {
    type Settings = UnjoinedSettings;
    type Error = TestBackendError;
    const CALLBACK_STALL_POLICY: CallbackStallPolicy = CallbackStallPolicy::Manual;

    fn setup(
        settings: Self::Settings,
        _internal_buffer_size: usize,
        failures: DriverFailureSignal,
    ) -> Result<(Self, PhysicalOutputFormat), Self::Error> {
        *settings
            .probe
            .failure
            .lock()
            .expect("failure lock poisoned") = Some(failures);
        Ok((
            Self {
                probe: settings.probe,
                panic_close: settings.panic_close,
            },
            PhysicalOutputFormat {
                sample_rate: TEST_RATE,
                channels: 2,
                sample_format: PhysicalSampleFormat::F32,
                buffer_frames_hint: Some(INTERNAL_BUFFER_FRAMES),
            },
        ))
    }

    fn start(&mut self, renderer: JoinedRenderer) -> Result<(), Self::Error> {
        self.probe
            .renderer
            .lock()
            .expect("renderer lock poisoned")
            .replace(renderer);
        Ok(())
    }

    fn play(&mut self) -> Result<(), Self::Error> {
        Ok(())
    }

    fn close_and_join(&mut self) -> bool {
        assert!(!self.panic_close, "injected unjoined close panic");
        false
    }
}

fn settings(probe: RenderProbe) -> TestBackendSettings {
    TestBackendSettings {
        probe,
        actual_rate: TEST_RATE,
        fail_setup: false,
        fail_start: false,
        backend_closes: Arc::new(AtomicUsize::new(0)),
        backend_drops: Arc::new(AtomicUsize::new(0)),
        lifecycle_threads: Arc::new(Mutex::new(Vec::new())),
        enforce_owner_thread: true,
    }
}

struct ConstantInput {
    frame: MixerFrame,
    calls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

impl MixerInput for ConstantInput {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        self.calls.fetch_add(1, Ordering::Relaxed);
        output.fill(self.frame);
        MixerInputStatus::Active
    }
}

impl Drop for ConstantInput {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

fn constant(value: f32) -> (Box<dyn MixerInput>, Arc<AtomicUsize>, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    (
        Box::new(ConstantInput {
            frame: MixerFrame::from_mono(value),
            calls: calls.clone(),
            drops: drops.clone(),
        }),
        calls,
        drops,
    )
}

fn start_reserved(
    reservation: MixerInputReservation,
    source: Box<dyn MixerInput>,
) -> RunningMixerInput {
    reservation
        .start_preboxed(source)
        .expect("admitted start must succeed")
}

fn assert_samples(output: &[f32], expected: f32) {
    for &sample in output {
        assert!(
            (sample - expected).abs() < 1.0e-6,
            "expected {expected}, got {sample}"
        );
    }
}

fn assert_linear(actual: f32, expected: f32) {
    assert_eq!(actual.to_bits(), expected.to_bits());
}

fn eventually(mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !condition() {
        assert!(Instant::now() < deadline, "condition did not become true");
        thread::yield_now();
    }
}

fn drive_session_retirement(
    probe: &RenderProbe,
    retirement: &mut MixerSessionRetirement,
) -> Result<(), MixerSessionRetirementError> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let wake_probe = Arc::new(WakeProbe::default());
    let waker = Waker::from(wake_probe);
    let mut context = Context::from_waker(&waker);
    loop {
        if let Poll::Ready(result) = Pin::new(&mut *retirement).poll(&mut context) {
            return result;
        }
        assert!(
            Instant::now() < deadline,
            "session retirement did not become ready"
        );
        let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
        probe.render(&mut output);
        thread::yield_now();
    }
}

struct FailureObserverProbe {
    bus: MixerScriptBusHandle,
    notifications: mpsc::SyncSender<MixerOutputFailure>,
    calls: AtomicUsize,
    panic_before_publish: bool,
    panic_after_publish: bool,
}

impl MixerFailureObserver for FailureObserverProbe {
    fn output_failed(&self, failure: MixerOutputFailure) {
        self.calls.fetch_add(1, Ordering::AcqRel);
        assert_eq!(self.bus.set_gain(0.5), Err(MixerControlError::OwnerStopped));
        assert!(
            !self.panic_before_publish,
            "injected pre-publication failure-observer panic"
        );
        let _ = self.notifications.try_send(failure);
        assert!(!self.panic_after_publish, "injected failure-observer panic");
    }
}

struct ObservedInput {
    observer: Arc<FailureObserverProbe>,
}

impl MixerInput for ObservedInput {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        output.fill(MixerFrame::from_mono(0.25));
        MixerInputStatus::Active
    }

    fn output_failure_observer(&self) -> Option<Arc<dyn MixerFailureObserver>> {
        Some(Arc::clone(&self.observer) as Arc<dyn MixerFailureObserver>)
    }
}

struct RetirementRaceObserver {
    notifications: mpsc::SyncSender<MixerOutputFailure>,
    calls: Arc<AtomicUsize>,
}

impl MixerFailureObserver for RetirementRaceObserver {
    fn output_failed(&self, failure: MixerOutputFailure) {
        self.calls.fetch_add(1, Ordering::AcqRel);
        self.notifications
            .try_send(failure)
            .expect("the one-shot failure notification remains bounded");
    }
}

struct RetirementRaceInput {
    observer: Arc<RetirementRaceObserver>,
    drops: Arc<AtomicUsize>,
    notified_before_drop: Arc<AtomicBool>,
}

impl MixerInput for RetirementRaceInput {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        output.fill(MixerFrame::ZERO);
        MixerInputStatus::Active
    }

    fn output_failure_observer(&self) -> Option<Arc<dyn MixerFailureObserver>> {
        Some(Arc::clone(&self.observer) as Arc<dyn MixerFailureObserver>)
    }
}

impl Drop for RetirementRaceInput {
    fn drop(&mut self) {
        self.notified_before_drop.store(
            self.observer.calls.load(Ordering::Acquire) == 1,
            Ordering::Release,
        );
        self.drops.fetch_add(1, Ordering::AcqRel);
    }
}

struct RetainedFailureObserver {
    calls: Arc<AtomicUsize>,
    drops: Arc<AtomicUsize>,
}

impl MixerFailureObserver for RetainedFailureObserver {
    fn output_failed(&self, _failure: MixerOutputFailure) {
        self.calls.fetch_add(1, Ordering::AcqRel);
    }
}

impl Drop for RetainedFailureObserver {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::AcqRel);
    }
}

struct RetainedObservedInput {
    observer: Arc<RetainedFailureObserver>,
    drops: Arc<AtomicUsize>,
}

impl MixerInput for RetainedObservedInput {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        output.fill(MixerFrame::ZERO);
        MixerInputStatus::Active
    }

    fn output_failure_observer(&self) -> Option<Arc<dyn MixerFailureObserver>> {
        Some(Arc::clone(&self.observer) as Arc<dyn MixerFailureObserver>)
    }
}

impl Drop for RetainedObservedInput {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::AcqRel);
    }
}

fn test_control(
    commands: SyncSender<OwnerCommand>,
    retirements: SyncSender<RetirementRequest>,
) -> Arc<ControlInner> {
    let driver_status = Arc::new(DriverStatus::new());
    assert!(driver_status.mark_live());
    let (session_retirements, _session_retirement_receiver) = mpsc::sync_channel(1);
    test_control_with_status(commands, retirements, session_retirements, driver_status)
}

fn test_control_with_status(
    commands: SyncSender<OwnerCommand>,
    retirements: SyncSender<RetirementRequest>,
    session_retirements: SyncSender<SessionRetirementRequest>,
    driver_status: Arc<DriverStatus>,
) -> Arc<ControlInner> {
    Arc::new(ControlInner {
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
        format: MixerFormat {
            sample_rate: TEST_RATE,
        },
        driver_status,
    })
}

#[test]
fn join_authority_is_not_send_but_scoped_bus_handles_are_send() {
    trait AmbiguousIfSend<Marker> {
        fn assert_not_send() {}
    }
    trait AmbiguousIfClone<Marker> {
        fn assert_not_clone() {}
    }
    fn assert_send<T: Send>() {}
    fn assert_send_sync<T: Send + Sync>() {}

    impl<T: ?Sized> AmbiguousIfSend<()> for T {}
    impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
    impl<T: ?Sized> AmbiguousIfClone<()> for T {}
    impl<T: Clone> AmbiguousIfClone<u8> for T {}
    let _ = <MixerService as AmbiguousIfSend<_>>::assert_not_send;
    let _ = <MixerSessionOwner as AmbiguousIfClone<_>>::assert_not_clone;
    assert_send::<MixerSessionOwner>();
    assert_send::<MixerScriptBusHandle>();
    assert_send::<MixerNativeBusHandle>();
    assert_send::<MixerSpeechBusHandle>();
    assert_send_sync::<MixerMasterGainAuthority>();
    assert_send_sync::<MixerSessionGainAuthority>();
}

#[test]
fn service_publishes_exact_format_and_joins_backend() {
    let probe = RenderProbe::default();
    let backend_drops = Arc::new(AtomicUsize::new(0));
    let service = MixerService::start_with_driver::<TestBackend>(
        TestBackendSettings {
            backend_drops: backend_drops.clone(),
            ..settings(probe.clone())
        },
        TEST_RATE,
    )
    .unwrap();
    assert_eq!(service.format().sample_rate(), TEST_RATE);
    assert_eq!(service.format().number_of_channels(), 2);
    assert_eq!(
        service.format().max_frames_per_callback(),
        INTERNAL_BUFFER_FRAMES
    );
    assert!(probe.is_live());

    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, None);
    assert!(!probe.is_live());
    assert_eq!(backend_drops.load(Ordering::Relaxed), 1);
}

#[test]
fn driver_lifecycle_stays_on_the_single_joined_owner_thread() {
    let probe = RenderProbe::default();
    let settings = settings(probe);
    let lifecycle_threads = Arc::clone(&settings.lifecycle_threads);
    let service = MixerService::start_with_driver::<TestBackend>(settings, TEST_RATE).unwrap();
    assert!(service.shutdown().clean);

    let threads = lock_recover(&lifecycle_threads);
    assert_eq!(threads.len(), 5);
    assert!(threads.iter().all(|thread| *thread == threads[0]));
    assert_ne!(threads[0], thread::current().id());
}

#[test]
fn permanent_slots_mix_two_script_inputs_and_native_with_independent_gains() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(7)).unwrap();
    let script = session.script_bus();
    let native = session.native_bus();

    let (first, _, _) = constant(0.125);
    let (second, _, _) = constant(0.25);
    let (tone, _, _) = constant(0.5);
    let first = start_reserved(script.try_reserve_input().unwrap(), first);
    let second = start_reserved(script.try_reserve_input().unwrap(), second);
    let tone = start_reserved(native.try_reserve_input().unwrap(), tone);

    script.set_gain(0.5).unwrap();
    native.set_gain(0.25).unwrap();
    let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
    // Kira has one already-rendered internal quantum when the owner accepts
    // the track commands; the following quantum observes the new bus gains.
    probe.render(&mut output);
    probe.render(&mut output);
    assert_samples(&output, 0.3125);

    assert!(first.suspend());
    probe.render(&mut output);
    assert_samples(&output, 0.25);
    assert!(first.resume());
    probe.render(&mut output);
    assert_samples(&output, 0.3125);

    assert!(block_on(second.shutdown()).unwrap().is_clean());
    probe.render(&mut output);
    assert_samples(&output, 0.1875);
    assert!(block_on(first.shutdown()).unwrap().is_clean());
    assert!(block_on(tone.shutdown()).unwrap().is_clean());
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, None);
}

fn render_settled(probe: &RenderProbe, output: &mut [f32]) {
    // Kira can already have one internal quantum buffered when an owner-side
    // track command is accepted. The second callback observes the mutation.
    probe.render(output);
    probe.render(output);
}

#[test]
fn master_and_session_gain_compose_across_all_three_buses_without_rt_allocation() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let master = service.master_gain_authority();
    let session = service.add_session(AudioSessionId(70)).unwrap();
    let session_gain = session.gain_authority();

    let (script, _, _) = constant(0.125);
    let (native, _, _) = constant(0.25);
    let (speech, _, _) = constant(0.5);
    let script = start_reserved(session.script_bus().try_reserve_input().unwrap(), script);
    let native = start_reserved(session.native_bus().try_reserve_input().unwrap(), native);
    let speech = start_reserved(session.speech_bus().try_reserve_input().unwrap(), speech);

    assert_eq!(
        session_gain.set_linear(0.5).unwrap(),
        MixerGainState {
            linear: 0.5,
            muted: false,
        }
    );
    assert_eq!(
        master.set_linear(0.25).unwrap(),
        MixerGainState {
            linear: 0.25,
            muted: false,
        }
    );
    let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
    render_settled(&probe, &mut output);
    assert_samples(&output, (0.125 + 0.25 + 0.5) * 0.5 * 0.25);
    assert_no_alloc::assert_no_alloc(|| probe.render(&mut output));
    assert_samples(&output, (0.125 + 0.25 + 0.5) * 0.5 * 0.25);

    // Exercise the render-side consumption of fresh Kira track commands, not
    // only the steady state after those commands have already been observed.
    session_gain.set_linear(0.4).unwrap();
    master.set_linear(0.2).unwrap();
    assert_no_alloc::assert_no_alloc(|| probe.render(&mut output));
    probe.render(&mut output);
    assert_samples(&output, (0.125 + 0.25 + 0.5) * 0.4 * 0.2);

    assert!(block_on(script.shutdown()).unwrap().is_clean());
    assert!(block_on(native.shutdown()).unwrap().is_clean());
    assert!(block_on(speech.shutdown()).unwrap().is_clean());
    assert!(service.shutdown().clean);
}

#[test]
fn session_gain_isolates_siblings_while_master_fans_out_to_both() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let master = service.master_gain_authority();
    let first_session = service.add_session(AudioSessionId(71)).unwrap();
    let second_session = service.add_session(AudioSessionId(72)).unwrap();
    let first_gain = first_session.gain_authority();

    let (first, first_calls, _) = constant(0.2);
    let (second, second_calls, _) = constant(0.4);
    let first = start_reserved(
        first_session.script_bus().try_reserve_input().unwrap(),
        first,
    );
    let second = start_reserved(
        second_session.speech_bus().try_reserve_input().unwrap(),
        second,
    );
    let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];

    master.set_linear(0.5).unwrap();
    render_settled(&probe, &mut output);
    assert_samples(&output, (0.2 + 0.4) * 0.5);

    first_gain.set_linear(0.25).unwrap();
    render_settled(&probe, &mut output);
    assert_samples(&output, (0.2 * 0.25 + 0.4) * 0.5);

    let first_before_mute = first_calls.load(Ordering::Acquire);
    let second_before_mute = second_calls.load(Ordering::Acquire);
    let muted = first_gain.set_muted(true).unwrap();
    assert_linear(muted.linear(), 0.25);
    assert!(muted.is_muted());
    assert_linear(muted.effective_linear(), 0.0);
    render_settled(&probe, &mut output);
    assert_samples(&output, 0.4 * 0.5);
    assert!(first_calls.load(Ordering::Acquire) > first_before_mute);
    assert!(second_calls.load(Ordering::Acquire) > second_before_mute);

    master.set_linear(1.0).unwrap();
    render_settled(&probe, &mut output);
    assert_samples(&output, 0.4);
    master.set_linear(0.0).unwrap();
    render_settled(&probe, &mut output);
    assert_samples(&output, 0.0);

    assert!(block_on(first.shutdown()).unwrap().is_clean());
    assert!(block_on(second.shutdown()).unwrap().is_clean());
    assert!(service.shutdown().clean);
}

#[test]
fn gain_updates_preserve_remembered_volume_and_apply_in_owner_order() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let master = service.master_gain_authority();
    let session = service.add_session(AudioSessionId(73)).unwrap();
    let gain = session.gain_authority();
    let (source, _, _) = constant(0.8);
    let running = start_reserved(session.native_bus().try_reserve_input().unwrap(), source);
    let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];

    let muted_default = gain.set_muted(true).unwrap();
    assert_linear(muted_default.linear(), DEFAULT_CONTROL_GAIN);
    assert!(muted_default.is_muted());
    render_settled(&probe, &mut output);
    assert_samples(&output, 0.0);

    let changed_while_muted = gain.set_linear(0.375).unwrap();
    assert_linear(changed_while_muted.linear(), 0.375);
    assert!(changed_while_muted.is_muted());
    assert_linear(changed_while_muted.effective_linear(), 0.0);
    render_settled(&probe, &mut output);
    assert_samples(&output, 0.0);

    let restored = gain.set_muted(false).unwrap();
    assert_linear(restored.linear(), 0.375);
    assert!(!restored.is_muted());
    render_settled(&probe, &mut output);
    assert_samples(&output, 0.8 * 0.375);

    let master_muted = master.set_muted(true).unwrap();
    assert_linear(master_muted.linear(), DEFAULT_CONTROL_GAIN);
    master.set_linear(0.5).unwrap();
    render_settled(&probe, &mut output);
    assert_samples(&output, 0.0);
    let master_restored = master.set_muted(false).unwrap();
    assert_linear(master_restored.linear(), 0.5);
    render_settled(&probe, &mut output);
    assert_samples(&output, 0.8 * 0.375 * 0.5);

    let canonical_zero = gain.set_linear(-0.0).unwrap();
    assert_eq!(canonical_zero.linear().to_bits(), 0.0_f32.to_bits());
    render_settled(&probe, &mut output);
    assert_samples(&output, 0.0);
    assert_linear(gain.set_linear(1.0).unwrap().linear(), 1.0);
    render_settled(&probe, &mut output);
    assert_samples(&output, 0.8 * 0.5);

    assert!(block_on(running.shutdown()).unwrap().is_clean());
    assert!(service.shutdown().clean);
}

#[test]
fn reservation_is_real_prestart_silence_and_render_allocates_nothing() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(9)).unwrap();
    let reservation = session.script_bus().try_reserve_input().unwrap();

    let mut output = [1.0; INTERNAL_BUFFER_FRAMES * 2];
    assert_no_alloc::assert_no_alloc(|| probe.render(&mut output));
    assert_samples(&output, 0.0);

    let (source, calls, drops) = constant(0.375);
    let running = reservation.start_preboxed(source).unwrap();
    assert_no_alloc::assert_no_alloc(|| probe.render(&mut output));
    assert_samples(&output, 0.375);
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_no_alloc::assert_no_alloc(|| probe.render(&mut output));
    assert_eq!(calls.load(Ordering::Relaxed), 2);

    assert!(block_on(running.shutdown()).unwrap().is_clean());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, None);
}

#[test]
fn exact_capacity_rejects_without_mutation_and_reuses_after_retirement() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(1)).unwrap();
    assert!(matches!(
        service.add_session(AudioSessionId(1)),
        Err(MixerControlError::DuplicateSession)
    ));
    assert!(matches!(
        service.add_session(AudioSessionId(2)),
        Err(MixerControlError::SessionCapacity)
    ));

    let reservation = session.script_bus().try_reserve_input().unwrap();
    assert!(matches!(
        session.script_bus().try_reserve_input(),
        Err(MixerControlError::InputCapacity)
    ));
    assert!(block_on(reservation.abort()).unwrap().is_clean());

    let reused = session.script_bus().try_reserve_input().unwrap();
    assert!(block_on(reused.abort()).unwrap().is_clean());
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, None);
}

#[test]
fn session_registrar_is_weak_send_sync_and_reports_exact_capacity() {
    fn assert_clone_send_sync<T: Clone + Send + Sync>() {}
    assert_clone_send_sync::<MixerSessionRegistrar>();

    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let registrar = service.session_registrar();
    let owner = registrar.add_session(AudioSessionId(101)).unwrap();
    assert_eq!(
        registrar.add_session(AudioSessionId(102)).unwrap_err(),
        MixerControlError::SessionCapacity
    );
    drop(owner);
    assert!(service.shutdown().clean);
}

#[test]
fn session_registrar_readds_same_id_without_reviving_stale_generation() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let registrar = service.session_registrar();
    let owner = registrar.add_session(AudioSessionId(103)).unwrap();
    let stale = owner.script_bus();
    let mut retirement = owner.retire();
    drive_session_retirement(&probe, &mut retirement).unwrap();

    let replacement = registrar.add_session(AudioSessionId(103)).unwrap();
    assert_eq!(
        stale.try_reserve_input().unwrap_err(),
        MixerControlError::UnknownSession
    );
    let reservation = replacement.script_bus().try_reserve_input().unwrap();
    assert!(block_on(reservation.abort()).unwrap().is_clean());
    drop(replacement);
    assert!(service.shutdown().clean);
}

#[test]
fn stale_session_registrar_cannot_retain_or_rebind_a_replacement_service() {
    let first_probe = RenderProbe::default();
    let first =
        MixerService::start_with_driver::<TestBackend>(settings(first_probe), TEST_RATE).unwrap();
    let stale = first.session_registrar();
    assert!(first.shutdown().clean);
    assert_eq!(
        stale.add_session(AudioSessionId(104)).unwrap_err(),
        MixerControlError::OwnerStopped
    );

    let second_probe = RenderProbe::default();
    let second =
        MixerService::start_with_driver::<TestBackend>(settings(second_probe), TEST_RATE).unwrap();
    let owner = second
        .session_registrar()
        .add_session(AudioSessionId(104))
        .unwrap();
    assert_eq!(
        stale.add_session(AudioSessionId(104)).unwrap_err(),
        MixerControlError::OwnerStopped
    );
    drop(owner);
    assert!(second.shutdown().clean);
}

#[test]
fn session_registrar_rejects_after_live_service_sealing() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let registrar = service.session_registrar();
    assert!(probe.fail_output());
    assert_eq!(
        registrar.add_session(AudioSessionId(105)).unwrap_err(),
        MixerControlError::OwnerStopped
    );
    assert!(service.shutdown().clean);
}

#[test]
fn session_registrar_and_service_shutdown_have_one_exact_linearization() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe), TEST_RATE).unwrap();
    let registrar = service.session_registrar();
    let start = Arc::new(Barrier::new(2));
    let (result_sender, result_receiver) = mpsc::sync_channel(1);

    thread::scope(|scope| {
        let worker_start = Arc::clone(&start);
        scope.spawn(move || {
            worker_start.wait();
            result_sender
                .send(registrar.add_session(AudioSessionId(106)))
                .unwrap();
        });
        start.wait();
        let shutdown = service.shutdown();
        assert!(shutdown.clean);

        match result_receiver.recv().unwrap() {
            Ok(owner) => {
                assert_eq!(owner.session_id(), AudioSessionId(106));
                assert_eq!(
                    owner.script_bus().try_reserve_input().unwrap_err(),
                    MixerControlError::OwnerStopped
                );
            }
            Err(error) => assert_eq!(error, MixerControlError::OwnerStopped),
        }
    });
}

#[test]
fn cloned_session_registrars_serialize_concurrent_adds_on_the_exact_owner() {
    const SESSION_COUNT: usize = 8;

    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        SESSION_COUNT,
        1,
    )
    .unwrap();
    let registrar = service.session_registrar();
    let owners = thread::scope(|scope| {
        let workers = (0..SESSION_COUNT)
            .map(|id| {
                let registrar = registrar.clone();
                scope.spawn(move || {
                    registrar
                        .add_session(AudioSessionId(200 + u64::try_from(id).unwrap()))
                        .unwrap()
                })
            })
            .collect::<Vec<_>>();
        workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>()
    });
    let mut ids = owners
        .iter()
        .map(MixerSessionOwner::session_id)
        .collect::<Vec<_>>();
    ids.sort_by_key(|id| id.0);
    assert_eq!(
        ids,
        (0..SESSION_COUNT)
            .map(|id| AudioSessionId(200 + u64::try_from(id).unwrap()))
            .collect::<Vec<_>>()
    );
    drop(owners);
    assert!(service.shutdown().clean);
}

#[test]
fn exact_session_retirement_waits_for_render_ack_and_readds_same_id_safely() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(20)).unwrap();
    let stale_bus = session.script_bus();
    let mut retirement = session.retire();

    assert_eq!(
        stale_bus.try_reserve_input().unwrap_err(),
        MixerControlError::UnknownSession
    );
    assert_eq!(
        stale_bus.set_gain(0.5),
        Err(MixerControlError::UnknownSession)
    );
    assert!(matches!(
        service.add_session(AudioSessionId(20)),
        Err(MixerControlError::SessionRetirementPending)
    ));
    assert!(matches!(
        service.add_session(AudioSessionId(200)),
        Err(MixerControlError::SessionRetirementPending)
    ));
    eventually(|| stale_bus.0.session.tracks_dropped.load(Ordering::Acquire));
    let safe_waker = Waker::from(Arc::new(WakeProbe::default()));
    let mut context = Context::from_waker(&safe_waker);
    assert!(matches!(
        Pin::new(&mut retirement).poll(&mut context),
        Poll::Pending
    ));

    drive_session_retirement(&probe, &mut retirement).unwrap();
    let replacement = service.add_session(AudioSessionId(20)).unwrap();
    let reservation = replacement.script_bus().try_reserve_input().unwrap();
    assert!(block_on(reservation.abort()).unwrap().is_clean());
    assert_eq!(
        stale_bus.try_reserve_input().unwrap_err(),
        MixerControlError::UnknownSession
    );
    assert!(service.shutdown().clean);
}

#[test]
fn session_close_waits_a_preclose_start_admission_without_blocking_the_receipt() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(26)).unwrap();
    let stale_bus = session.script_bus();
    let (source, _, drops) = constant(0.25);
    let installed = stale_bus
        .try_reserve_input()
        .unwrap()
        .install_preboxed(source)
        .unwrap();

    let mut retirement = session.retire();
    let safe_waker = Waker::from(Arc::new(WakeProbe::default()));
    let mut context = Context::from_waker(&safe_waker);
    assert!(matches!(
        Pin::new(&mut retirement).poll(&mut context),
        Poll::Pending
    ));
    assert_eq!(
        stale_bus.try_reserve_input().unwrap_err(),
        MixerControlError::UnknownSession
    );

    let running = installed
        .open()
        .expect("a start admitted before close must finish publication");
    drive_session_retirement(&probe, &mut retirement).unwrap();
    assert_eq!(drops.load(Ordering::Acquire), 1);
    let replacement = service.add_session(AudioSessionId(31)).unwrap();
    let (replacement_source, _, _) = constant(0.5);
    let replacement_input = start_reserved(
        replacement.script_bus().try_reserve_input().unwrap(),
        replacement_source,
    );
    assert!(!running.suspend());
    assert!(!running.resume());
    assert!(block_on(running.shutdown()).unwrap().is_clean());
    let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
    probe.render(&mut output);
    assert_samples(&output, 0.5);
    assert!(block_on(replacement_input.shutdown()).unwrap().is_clean());
    assert!(service.shutdown().clean);
}

#[test]
fn cleanup_already_in_flight_terminalizes_when_session_close_wins() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(27)).unwrap();
    let stale_bus = session.script_bus();
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    let drops = Arc::new(AtomicUsize::new(0));
    let running = start_reserved(
        stale_bus.try_reserve_input().unwrap(),
        Box::new(BlockingDestructor {
            entered: entered_sender,
            release: Arc::clone(&release),
            drops: Arc::clone(&drops),
        }),
    );
    let shutdown = running.shutdown();
    entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("cleanup worker did not enter the source destructor");

    let mut retirement = session.retire();
    assert_eq!(
        stale_bus.try_reserve_input().unwrap_err(),
        MixerControlError::UnknownSession
    );
    let safe_waker = Waker::from(Arc::new(WakeProbe::default()));
    let mut context = Context::from_waker(&safe_waker);
    assert!(matches!(
        Pin::new(&mut retirement).poll(&mut context),
        Poll::Pending
    ));
    let (released, ready) = &*release;
    *lock_recover(released) = true;
    ready.notify_all();

    assert!(block_on(shutdown).unwrap().is_clean());
    drive_session_retirement(&probe, &mut retirement).unwrap();
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert!(service.add_session(AudioSessionId(27)).is_ok());
    assert!(service.shutdown().clean);
}

#[test]
fn accepted_input_shutdown_is_correlated_before_session_forced_cleanup() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(32)).unwrap();
    let (source, _, drops) = constant(0.25);
    let running = start_reserved(session.script_bus().try_reserve_input().unwrap(), source);
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let release = Arc::new(Barrier::new(2));
    *lock_recover(&service.driver_status.retirement_scan_hook) =
        Some(Arc::new(RetirementScanHook {
            entered: entered_sender,
            release: Arc::clone(&release),
            armed: AtomicBool::new(true),
        }));
    entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("owner did not pause after its first input-retirement scan");

    let input_retirement = running.shutdown();
    let mut session_retirement = session.retire();
    release.wait();
    assert!(block_on(input_retirement).unwrap().is_clean());
    drive_session_retirement(&probe, &mut session_retirement).unwrap();
    assert_eq!(drops.load(Ordering::Acquire), 1);

    let replacement = service.add_session(AudioSessionId(32)).unwrap();
    drop(replacement);
    assert!(service.shutdown().clean);
}

#[test]
fn cleanup_finish_does_not_deadlock_a_racing_session_reserve() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(38)).unwrap();
    let bus = session.script_bus();
    let (source, _, _) = constant(0.25);
    let running = start_reserved(bus.try_reserve_input().unwrap(), source);
    let first_slot = Arc::as_ptr(&running.slot);
    let first_generation = running.generation;

    let session_control = Arc::clone(session.session.as_ref().unwrap());
    let (cleanup_entered, cleanup_entered_receiver) = mpsc::sync_channel(1);
    let cleanup_release = Arc::new(Barrier::new(2));
    *lock_recover(&session_control.cleanup_finish_hook) = Some(Arc::new(RetirementScanHook {
        entered: cleanup_entered,
        release: Arc::clone(&cleanup_release),
        armed: AtomicBool::new(true),
    }));

    let first_shutdown = running.shutdown();
    cleanup_entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("owner did not pause after receiving the cleanup result");

    let (request_entered, request_entered_receiver) = mpsc::sync_channel(1);
    let request_release = Arc::new(Barrier::new(2));
    *lock_recover(&session_control.request_enqueued_hook) = Some(Arc::new(RetirementScanHook {
        entered: request_entered,
        release: Arc::clone(&request_release),
        armed: AtomicBool::new(true),
    }));
    let (reserved_sender, reserved_receiver) = mpsc::sync_channel(1);
    let reserve = thread::spawn(move || {
        let _ = reserved_sender.send(bus.try_reserve_input());
    });
    request_entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("racing reserve was not enqueued");

    request_release.wait();
    cleanup_release.wait();
    let reservation = reserved_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("racing reserve deadlocked cleanup finalization")
        .expect("cleaned slot was not recycled");
    reserve.join().unwrap();
    assert_eq!(Arc::as_ptr(&reservation.slot), first_slot);
    assert!(reservation.generation > first_generation);
    assert!(block_on(first_shutdown).unwrap().is_clean());

    let mut session_retirement = session.retire();
    let replacement_shutdown = reservation.abort();
    assert!(block_on(replacement_shutdown).unwrap().is_clean());
    drive_session_retirement(&probe, &mut session_retirement).unwrap();
    assert!(service.shutdown().clean);
}

#[test]
fn pending_active_input_receipt_cannot_be_stolen_by_session_forced_cleanup() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(33)).unwrap();
    let (render_entered, render_entered_receiver) = mpsc::sync_channel(1);
    let render_release = Arc::new(Barrier::new(2));
    let drops = Arc::new(AtomicUsize::new(0));
    let running = start_reserved(
        session.script_bus().try_reserve_input().unwrap(),
        Box::new(BlockingInput {
            entered: render_entered,
            release: Arc::clone(&render_release),
            drops: Arc::clone(&drops),
        }),
    );
    let render_probe = probe.clone();
    let render = thread::spawn(move || {
        let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
        render_probe.render(&mut output);
    });
    render_entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("render did not acquire ACTIVE");

    let (first_entered, first_entered_receiver) = mpsc::sync_channel(1);
    let first_release = Arc::new(Barrier::new(2));
    *lock_recover(&service.driver_status.retirement_scan_hook) =
        Some(Arc::new(RetirementScanHook {
            entered: first_entered,
            release: Arc::clone(&first_release),
            armed: AtomicBool::new(true),
        }));
    let (forced_entered, forced_entered_receiver) = mpsc::sync_channel(1);
    let forced_release = Arc::new(Barrier::new(2));
    *lock_recover(&service.driver_status.session_forced_hook) =
        Some(Arc::new(RetirementScanHook {
            entered: forced_entered,
            release: Arc::clone(&forced_release),
            armed: AtomicBool::new(true),
        }));
    first_entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("owner did not pause after its first input scan");

    let input_retirement = running.shutdown();
    let mut session_retirement = session.retire();
    first_release.wait();
    forced_entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("owner did not pause before forced session cleanup");
    render_release.wait();
    render.join().unwrap();
    forced_release.wait();

    assert!(block_on(input_retirement).unwrap().is_clean());
    drive_session_retirement(&probe, &mut session_retirement).unwrap();
    assert_eq!(drops.load(Ordering::Acquire), 1);
    let replacement = service.add_session(AudioSessionId(33)).unwrap();
    drop(replacement);
    assert!(service.shutdown().clean);
}

#[test]
fn retiring_one_session_terminalizes_inputs_without_disturbing_a_sibling() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        2,
        1,
    )
    .unwrap();
    let retiring = service.add_session(AudioSessionId(21)).unwrap();
    let sibling = service.add_session(AudioSessionId(22)).unwrap();
    let (retiring_source, _, retiring_drops) = constant(0.25);
    let retiring_input = start_reserved(
        retiring.script_bus().try_reserve_input().unwrap(),
        retiring_source,
    );
    let sibling_bus = sibling.script_bus();
    let (notifications, received) = mpsc::sync_channel(1);
    let sibling_observer = Arc::new(FailureObserverProbe {
        bus: sibling_bus.clone(),
        notifications,
        calls: AtomicUsize::new(0),
        panic_before_publish: false,
        panic_after_publish: false,
    });
    let sibling_input = start_reserved(
        sibling_bus.try_reserve_input().unwrap(),
        Box::new(ObservedInput {
            observer: Arc::clone(&sibling_observer),
        }),
    );
    sibling_bus.set_gain(0.5).unwrap();

    let mut retirement = retiring.retire();
    assert!(!retiring_input.resume());
    drive_session_retirement(&probe, &mut retirement).unwrap();
    assert_eq!(retiring_drops.load(Ordering::Acquire), 1);
    let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
    probe.render(&mut output);
    assert_samples(&output, 0.125);

    assert!(probe.fail_output());
    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        MixerOutputFailure::BackendFailure
    );
    assert_eq!(sibling_observer.calls.load(Ordering::Acquire), 1);

    drop(retiring_input);
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
    let sibling_retirement = block_on(sibling_input.shutdown()).unwrap();
    assert!(sibling_retirement.failed_before_retirement);
    assert_eq!(sibling_observer.calls.load(Ordering::Acquire), 1);
}

#[test]
fn session_retirement_waits_for_an_active_render_before_source_and_tracks() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(31)).unwrap();
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let release = Arc::new(Barrier::new(2));
    let drops = Arc::new(AtomicUsize::new(0));
    let running = start_reserved(
        session.script_bus().try_reserve_input().unwrap(),
        Box::new(BlockingInput {
            entered: entered_sender,
            release: Arc::clone(&release),
            drops: Arc::clone(&drops),
        }),
    );
    let render_probe = probe.clone();
    let render = thread::spawn(move || {
        let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
        render_probe.render(&mut output);
    });
    entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("render did not enter the exact session slot");

    let mut retirement = session.retire();
    let safe_waker = Waker::from(Arc::new(WakeProbe::default()));
    let mut context = Context::from_waker(&safe_waker);
    assert!(matches!(
        Pin::new(&mut retirement).poll(&mut context),
        Poll::Pending
    ));
    assert_eq!(drops.load(Ordering::Acquire), 0);
    release.wait();
    render.join().unwrap();
    eventually(|| {
        !matches!(
            slot_phase(running.slot.word.load(Ordering::Acquire)),
            SlotPhase::Running
        )
    });

    drive_session_retirement(&probe, &mut retirement).unwrap();
    assert_eq!(drops.load(Ordering::Acquire), 1);
    drop(running);
    assert!(service.shutdown().clean);
}

#[test]
fn preclose_reservation_cannot_start_and_session_drop_is_autonomous() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(23)).unwrap();
    let stale_bus = session.script_bus();
    let reservation = stale_bus.try_reserve_input().unwrap();
    drop(session);

    let (source, _, drops) = constant(0.125);
    let MixerInputStartFailure::Rejected(failure) = reservation.start_preboxed(source).unwrap_err()
    else {
        panic!("session close must reject before installation");
    };
    assert_eq!(failure.error(), MixerControlError::UnknownSession);
    let (_, reservation, source) = failure.into_parts();
    drop(source);
    drop(reservation.abort());
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert_eq!(
        stale_bus.try_reserve_input().unwrap_err(),
        MixerControlError::UnknownSession
    );

    let deadline = Instant::now() + Duration::from_secs(2);
    let replacement = loop {
        let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
        probe.render(&mut output);
        match service.add_session(AudioSessionId(23)) {
            Ok(session) => break session,
            Err(MixerControlError::SessionRetirementPending) => {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            Err(error) => panic!("unexpected replacement error: {error:?}"),
        }
    };
    drop(replacement);
    assert!(service.shutdown().clean);
}

#[test]
fn accepted_session_retirement_survives_dropped_receipt_and_service_seal() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(24)).unwrap();
    drop(session.retire());
    let deadline = Instant::now() + Duration::from_secs(2);
    let replacement = loop {
        let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
        probe.render(&mut output);
        match service.add_session(AudioSessionId(24)) {
            Ok(session) => break session,
            Err(MixerControlError::SessionRetirementPending) => {
                assert!(Instant::now() < deadline);
                thread::yield_now();
            }
            Err(error) => panic!("unexpected replacement error: {error:?}"),
        }
    };
    let retirement = replacement.retire();
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(block_on(retirement), Ok(()));
}

#[test]
fn stale_session_gain_cannot_control_same_id_reuse_and_replacement_resets_default() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let original = service.add_session(AudioSessionId(240)).unwrap();
    let stale = original.gain_authority();
    assert_linear(stale.set_linear(0.25).unwrap().linear(), 0.25);
    let mut retirement = original.retire();
    assert_eq!(
        stale.set_muted(true),
        Err(MixerControlError::UnknownSession)
    );
    assert_eq!(drive_session_retirement(&probe, &mut retirement), Ok(()));

    let replacement = service.add_session(AudioSessionId(240)).unwrap();
    let replacement_gain = replacement.gain_authority();
    assert_eq!(
        stale.set_linear(0.5),
        Err(MixerControlError::UnknownSession)
    );
    let replacement_default = replacement_gain.set_muted(true).unwrap();
    assert_linear(replacement_default.linear(), DEFAULT_CONTROL_GAIN);
    assert!(replacement_default.is_muted());
    assert_linear(replacement_gain.set_muted(false).unwrap().linear(), 1.0);

    let mut replacement_retirement = replacement.retire();
    assert_eq!(
        drive_session_retirement(&probe, &mut replacement_retirement),
        Ok(())
    );
    assert!(service.shutdown().clean);
}

#[test]
fn accepted_session_retirement_settles_after_device_failure_cleanup() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(29)).unwrap();
    let retirement = session.retire();
    assert!(probe.fail_output());
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
    assert_eq!(block_on(retirement), Ok(()));
}

#[test]
fn late_session_retirement_uses_clean_terminal_output_proof() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(290)).unwrap();

    assert!(probe.fail_output());
    eventually(|| service.owner_status.retired.load(Ordering::Acquire));

    assert_eq!(block_on(session.retire()), Ok(()));
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
}

#[test]
fn gain_authorities_fail_typed_after_output_death_and_joined_shutdown() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let master = service.master_gain_authority();
    let session = service.add_session(AudioSessionId(291)).unwrap();
    let session_gain = session.gain_authority();
    assert_linear(master.set_linear(0.5).unwrap().linear(), 0.5);
    assert_linear(session_gain.set_linear(0.25).unwrap().linear(), 0.25);

    assert!(probe.fail_output());
    assert_eq!(master.set_linear(1.0), Err(MixerControlError::OwnerStopped));
    assert_eq!(
        session_gain.set_muted(true),
        Err(MixerControlError::OwnerStopped)
    );
    assert_eq!(
        master.output_failure(),
        Some(MixerOutputFailure::BackendFailure)
    );
    assert_eq!(
        session_gain.output_failure(),
        Some(MixerOutputFailure::BackendFailure)
    );
    let retirement = session.retire();
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
    assert_eq!(block_on(retirement), Ok(()));
    assert_eq!(master.set_muted(true), Err(MixerControlError::OwnerStopped));
    assert_eq!(
        session_gain.set_linear(0.5),
        Err(MixerControlError::OwnerStopped)
    );
    assert_eq!(master.output_failure(), None);
    assert_eq!(session_gain.output_failure(), None);
}

#[test]
fn unproven_backend_join_never_false_acks_session_retirement() {
    for panic_close in [false, true] {
        let service = MixerService::start_with_driver_and_limits::<UnjoinedBackend>(
            UnjoinedSettings {
                probe: RenderProbe::default(),
                panic_close,
            },
            MixerFormat {
                sample_rate: TEST_RATE,
            },
            1,
            1,
        )
        .unwrap();
        let session = service.add_session(AudioSessionId(34)).unwrap();
        let retirement = session.retire();
        let shutdown = service.shutdown();
        assert!(!shutdown.clean);
        assert_eq!(
            block_on(retirement),
            Err(MixerSessionRetirementError::OwnerUncertain)
        );
    }
}

#[test]
fn late_session_retirement_never_false_acks_unproven_backend_join() {
    for panic_close in [false, true] {
        let service = MixerService::start_with_driver_and_limits::<UnjoinedBackend>(
            UnjoinedSettings {
                probe: RenderProbe::default(),
                panic_close,
            },
            MixerFormat {
                sample_rate: TEST_RATE,
            },
            1,
            1,
        )
        .unwrap();
        let session = service.add_session(AudioSessionId(340)).unwrap();

        assert!(!service.shutdown().clean);
        assert_eq!(
            block_on(session.retire()),
            Err(MixerSessionRetirementError::OwnerUncertain)
        );
    }
}

#[test]
fn session_retirement_contains_hostile_waker_and_survives_owner_panic() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(30)).unwrap();
    let mut retirement = session.retire();
    let panic_waker = Waker::from(Arc::new(PanicWake));
    let mut context = Context::from_waker(&panic_waker);
    assert!(matches!(
        Pin::new(&mut retirement).poll(&mut context),
        Poll::Pending
    ));
    service
        .driver_status
        .panic_owner
        .store(true, Ordering::Release);
    eventually(|| service.driver_status.failure() == Some(MixerOutputFailure::OwnerPanicked));

    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::OwnerPanicked));
    assert_eq!(block_on(retirement), Ok(()));
}

#[test]
fn hostile_source_cleanup_reports_unclean_only_after_track_removal() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe.clone()),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(25)).unwrap();
    let running = start_reserved(
        session.script_bus().try_reserve_input().unwrap(),
        Box::new(PanickingDestructor),
    );
    let mut retirement = session.retire();
    assert_eq!(
        drive_session_retirement(&probe, &mut retirement),
        Err(MixerSessionRetirementError::CleanupFailed)
    );
    drop(running);
    let replacement = service.add_session(AudioSessionId(25)).unwrap();
    drop(replacement);
    assert!(!service.shutdown().clean);
}

#[test]
fn dropped_shutdown_observer_does_not_cancel_cleanup_or_reuse() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(2)).unwrap();
    let bus = session.script_bus();
    let (source, _, drops) = constant(0.25);
    let running = start_reserved(bus.try_reserve_input().unwrap(), source);
    drop(running.shutdown());

    eventually(|| drops.load(Ordering::Acquire) == 1);
    let reused = loop {
        match bus.try_reserve_input() {
            Ok(reservation) => break reservation,
            Err(MixerControlError::InputCapacity) => thread::yield_now(),
            Err(error) => panic!("unexpected reserve error: {error:?}"),
        }
    };
    assert!(block_on(reused.abort()).unwrap().is_clean());
    assert!(service.shutdown().clean);
}

#[test]
fn bounded_control_full_and_disconnect_are_typed_without_mutation() {
    let (full_commands, full_receiver) = mpsc::sync_channel(0);
    let (full_retirements, _full_retirement_receiver) = mpsc::sync_channel(1);
    let full_control = test_control(full_commands, full_retirements);
    let session = Arc::new(SessionControl::new(SessionKey {
        id: AudioSessionId(30),
        generation: 1,
    }));
    let handle = MixerBusHandle {
        control: Arc::downgrade(&full_control),
        session,
        bus: SessionBus::Script,
    };
    assert_eq!(handle.set_gain(0.5), Err(MixerControlError::Saturated));
    drop(full_receiver);
    assert_eq!(handle.set_gain(0.5), Err(MixerControlError::OwnerStopped));
}

#[test]
fn bounded_gain_validation_precedes_enqueue_for_both_authorities() {
    let (commands, command_receiver) = mpsc::sync_channel(0);
    let (retirements, _retirement_receiver) = mpsc::sync_channel(1);
    let control = test_control(commands, retirements);
    let session = Arc::new(SessionControl::new(SessionKey {
        id: AudioSessionId(300),
        generation: 1,
    }));
    let master = MixerMasterGainAuthority {
        control: Arc::downgrade(&control),
    };
    let session_gain = MixerSessionGainAuthority {
        control: Arc::downgrade(&control),
        session,
    };

    for invalid in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY, -0.001, 1.001] {
        assert_eq!(
            master.set_linear(invalid),
            Err(MixerControlError::InvalidGain)
        );
        assert_eq!(
            session_gain.set_linear(invalid),
            Err(MixerControlError::InvalidGain)
        );
    }
    assert!(matches!(
        command_receiver.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    assert_eq!(
        master.set_linear(MIN_CONTROL_GAIN),
        Err(MixerControlError::Saturated)
    );
    assert_eq!(
        session_gain.set_linear(MAX_CONTROL_GAIN),
        Err(MixerControlError::Saturated)
    );
    assert_eq!(master.set_muted(true), Err(MixerControlError::Saturated));
    drop(command_receiver);
    assert_eq!(master.set_muted(true), Err(MixerControlError::OwnerStopped));
    assert_eq!(
        session_gain.set_muted(true),
        Err(MixerControlError::OwnerStopped)
    );
}

#[test]
fn session_retirement_queue_failure_is_typed_and_leaves_old_clones_inert() {
    for disconnected in [false, true] {
        let (commands, _command_receiver) = mpsc::sync_channel(1);
        let (retirements, _retirement_receiver) = mpsc::sync_channel(1);
        let (session_retirements, session_retirement_receiver) = mpsc::sync_channel(0);
        if disconnected {
            drop(session_retirement_receiver);
        }
        let driver_status = Arc::new(DriverStatus::new());
        assert!(driver_status.mark_live());
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
            format: MixerFormat {
                sample_rate: TEST_RATE,
            },
            driver_status,
        });
        let session = Arc::new(SessionControl::new(SessionKey {
            id: AudioSessionId(28),
            generation: 1,
        }));
        let (completion, retirement) = oneshot::channel();
        let _completion = (!disconnected).then_some(completion);
        let stale_bus = MixerScriptBusHandle(MixerBusHandle {
            control: Arc::downgrade(&control),
            session: Arc::clone(&session),
            bus: SessionBus::Script,
        });
        let owner = MixerSessionOwner {
            control: Arc::downgrade(&control),
            session: Some(session),
            retirement: Some(retirement),
        };
        let mut retirement = owner.retire();
        let safe_waker = Waker::from(Arc::new(WakeProbe::default()));
        let mut context = Context::from_waker(&safe_waker);
        assert_eq!(
            Pin::new(&mut retirement).poll(&mut context),
            Poll::Ready(Err(if disconnected {
                MixerSessionRetirementError::OwnerUncertain
            } else {
                MixerSessionRetirementError::QueueInvariant
            }))
        );
        assert_eq!(
            stale_bus.try_reserve_input().unwrap_err(),
            MixerControlError::UnknownSession
        );
    }
}

#[test]
fn dropped_reserve_response_restores_the_same_physical_capacity() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(31)).unwrap();
    let control = service.control.as_ref().unwrap();
    // A rendezvous channel proves the receiver is gone before the owner can
    // publish the reservation; a buffered response could win the race and
    // make this a test of receiver timing instead of rollback.
    let (response, abandoned) = mpsc::sync_channel(0);
    control
        .commands
        .send(OwnerCommand::Reserve(
            session.session.as_ref().unwrap().key,
            SessionBus::Script,
            response,
        ))
        .unwrap();
    drop(abandoned);

    let reservation = session.script_bus().try_reserve_input().unwrap();
    assert!(reservation.generation > 1);
    assert!(block_on(reservation.abort()).unwrap().is_clean());
    assert!(service.shutdown().clean);
}

#[test]
fn stale_generation_cannot_control_a_reused_slot() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(32)).unwrap();
    let bus = session.script_bus();
    let first = bus.try_reserve_input().unwrap();
    let stale_slot = Arc::clone(&first.slot);
    let stale_generation = first.generation;
    assert!(block_on(first.abort()).unwrap().is_clean());

    let second = bus.try_reserve_input().unwrap();
    assert_eq!(second.slot.address.index, stale_slot.address.index);
    assert_ne!(second.generation, stale_generation);
    assert!(!stale_slot.set_suspended(stale_generation, true));
    assert!(!stale_slot.close(stale_generation, false));
    assert!(block_on(second.abort()).unwrap().is_clean());
    assert!(service.shutdown().clean);
}

struct BlockingInput {
    entered: mpsc::SyncSender<thread::ThreadId>,
    release: Arc<Barrier>,
    drops: Arc<AtomicUsize>,
}

struct OrderedBlockingInput {
    entered: mpsc::SyncSender<thread::ThreadId>,
    release: Arc<Barrier>,
    dropped: mpsc::SyncSender<thread::ThreadId>,
    backend_drops: Arc<AtomicUsize>,
}

impl MixerInput for OrderedBlockingInput {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        self.entered.send(thread::current().id()).unwrap();
        self.release.wait();
        output.fill(MixerFrame::from_mono(0.25));
        MixerInputStatus::Active
    }
}

impl Drop for OrderedBlockingInput {
    fn drop(&mut self) {
        assert_eq!(self.backend_drops.load(Ordering::Acquire), 1);
        let _ = self.dropped.send(thread::current().id());
    }
}

impl MixerInput for BlockingInput {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        self.entered.send(thread::current().id()).unwrap();
        self.release.wait();
        output.fill(MixerFrame::from_mono(0.25));
        MixerInputStatus::Active
    }
}

impl Drop for BlockingInput {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

#[derive(Default)]
struct WakeProbe {
    wakes: AtomicUsize,
}

impl Wake for WakeProbe {
    fn wake(self: Arc<Self>) {
        self.wakes.fetch_add(1, Ordering::Release);
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.wakes.fetch_add(1, Ordering::Release);
    }
}

struct PanicWake;

impl Wake for PanicWake {
    fn wake(self: Arc<Self>) {
        panic!("hostile waker")
    }

    fn wake_by_ref(self: &Arc<Self>) {
        panic!("hostile waker")
    }
}

#[test]
fn active_render_keeps_shutdown_pending_then_owner_wakes_and_retires_off_render() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(3)).unwrap();
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let release = Arc::new(Barrier::new(2));
    let drops = Arc::new(AtomicUsize::new(0));
    let running = start_reserved(
        session.script_bus().try_reserve_input().unwrap(),
        Box::new(BlockingInput {
            entered: entered_sender,
            release: release.clone(),
            drops: drops.clone(),
        }),
    );

    let render_probe = probe.clone();
    let render = thread::spawn(move || {
        let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
        render_probe.render(&mut output);
        output
    });
    let render_thread = entered_receiver.recv().unwrap();
    let mut shutdown = Box::pin(running.shutdown());
    let wake_probe = Arc::new(WakeProbe::default());
    let waker = Waker::from(wake_probe.clone());
    let mut context = Context::from_waker(&waker);
    assert!(matches!(
        shutdown.as_mut().poll(&mut context),
        Poll::Pending
    ));
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    release.wait();
    assert_samples(&render.join().unwrap(), 0.25);
    eventually(|| wake_probe.wakes.load(Ordering::Acquire) > 0);
    let retirement = match shutdown.as_mut().poll(&mut context) {
        Poll::Ready(result) => result.unwrap(),
        Poll::Pending => panic!("owner wake did not publish retirement"),
    };
    assert!(retirement.is_clean());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert_ne!(render_thread, thread::current().id());
    assert!(service.shutdown().clean);
}

#[test]
fn hostile_completion_waker_cannot_kill_the_owner() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(35)).unwrap();
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let release = Arc::new(Barrier::new(2));
    let drops = Arc::new(AtomicUsize::new(0));
    let running = start_reserved(
        session.script_bus().try_reserve_input().unwrap(),
        Box::new(BlockingInput {
            entered: entered_sender,
            release: release.clone(),
            drops: drops.clone(),
        }),
    );
    let render_probe = probe.clone();
    let render = thread::spawn(move || {
        let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
        render_probe.render(&mut output);
    });
    entered_receiver.recv().unwrap();

    let mut shutdown = Box::pin(running.shutdown());
    let panic_waker = Waker::from(Arc::new(PanicWake));
    let mut panic_context = Context::from_waker(&panic_waker);
    assert!(matches!(
        shutdown.as_mut().poll(&mut panic_context),
        Poll::Pending
    ));
    release.wait();
    render.join().unwrap();
    eventually(|| drops.load(Ordering::Acquire) == 1);

    assert!(block_on(shutdown).unwrap().is_clean());
    assert!(service.shutdown().clean);
}

#[test]
fn service_shutdown_waits_for_active_render_then_forced_cleanup() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(33)).unwrap();
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let release = Arc::new(Barrier::new(2));
    let drops = Arc::new(AtomicUsize::new(0));
    let running = start_reserved(
        session.script_bus().try_reserve_input().unwrap(),
        Box::new(BlockingInput {
            entered: entered_sender,
            release: release.clone(),
            drops: drops.clone(),
        }),
    );
    let render_probe = probe.clone();
    let render = thread::spawn(move || {
        let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
        render_probe.render(&mut output);
    });
    entered_receiver.recv().unwrap();

    let release_for_thread = release.clone();
    let drops_for_thread = drops.clone();
    let releaser = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        assert_eq!(drops_for_thread.load(Ordering::Relaxed), 0);
        release_for_thread.wait();
    });
    let started = Instant::now();
    let result = service.shutdown();
    assert!(started.elapsed() >= Duration::from_millis(50));
    assert!(result.clean);
    releaser.join().unwrap();
    render.join().unwrap();
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(block_on(running.shutdown()).unwrap().is_clean());
}

#[test]
fn output_death_waits_for_active_callback_then_joins_before_source_cleanup() {
    let probe = RenderProbe::default();
    let settings = settings(probe.clone());
    let backend_closes = Arc::clone(&settings.backend_closes);
    let backend_drops = Arc::clone(&settings.backend_drops);
    let service = MixerService::start_with_driver::<TestBackend>(settings, TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(906)).unwrap();
    let (entered_sender, entered_receiver) = mpsc::sync_channel(1);
    let (dropped_sender, dropped_receiver) = mpsc::sync_channel(1);
    let release = Arc::new(Barrier::new(2));
    let running = start_reserved(
        session.script_bus().try_reserve_input().unwrap(),
        Box::new(OrderedBlockingInput {
            entered: entered_sender,
            release: Arc::clone(&release),
            dropped: dropped_sender,
            backend_drops: Arc::clone(&backend_drops),
        }),
    );

    let render_probe = probe.clone();
    let render = thread::spawn(move || {
        let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
        render_probe.render(&mut output);
    });
    let render_thread = entered_receiver.recv().unwrap();
    assert!(probe.fail_output());

    let release_for_thread = Arc::clone(&release);
    let drops_for_thread = Arc::clone(&backend_drops);
    let releaser = thread::spawn(move || {
        thread::sleep(Duration::from_millis(50));
        assert_eq!(drops_for_thread.load(Ordering::Acquire), 0);
        release_for_thread.wait();
    });
    let started = Instant::now();
    let shutdown = service.shutdown();
    assert!(started.elapsed() >= Duration::from_millis(50));
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
    releaser.join().unwrap();
    render.join().unwrap();
    assert_eq!(backend_closes.load(Ordering::Acquire), 1);
    assert_eq!(backend_drops.load(Ordering::Acquire), 1);
    let cleanup_thread = dropped_receiver
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    assert_ne!(cleanup_thread, render_thread);
    assert_ne!(cleanup_thread, thread::current().id());
    let retirement = block_on(running.shutdown()).unwrap();
    assert!(retirement.failed_before_retirement);
    assert_eq!(
        retirement.output_failure,
        Some(MixerOutputFailure::BackendFailure)
    );
}

#[test]
fn dropping_service_after_output_death_still_retires_backend_and_source() {
    let probe = RenderProbe::default();
    let settings = settings(probe.clone());
    let backend_closes = Arc::clone(&settings.backend_closes);
    let backend_drops = Arc::clone(&settings.backend_drops);
    let service = MixerService::start_with_driver::<TestBackend>(settings, TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(907)).unwrap();
    let (source, _, source_drops) = constant(0.25);
    let running = start_reserved(session.script_bus().try_reserve_input().unwrap(), source);

    assert!(probe.fail_output());
    drop(service);
    drop(running);
    eventually(|| {
        backend_closes.load(Ordering::Acquire) == 1
            && backend_drops.load(Ordering::Acquire) == 1
            && source_drops.load(Ordering::Acquire) == 1
    });
}

struct ReentrantDestructor {
    bus: MixerScriptBusHandle,
    result: mpsc::SyncSender<Result<(), MixerControlError>>,
}

impl MixerInput for ReentrantDestructor {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        output.fill(MixerFrame::ZERO);
        MixerInputStatus::Active
    }
}

impl Drop for ReentrantDestructor {
    fn drop(&mut self) {
        let _ = self.result.send(self.bus.set_gain(0.5));
    }
}

#[test]
fn callback_destructor_can_reenter_control_without_deadlocking_owner() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(34)).unwrap();
    let bus = session.script_bus();
    let (result_sender, result_receiver) = mpsc::sync_channel(1);
    let running = start_reserved(
        bus.try_reserve_input().unwrap(),
        Box::new(ReentrantDestructor {
            bus: bus.clone(),
            result: result_sender,
        }),
    );
    assert!(block_on(running.shutdown()).unwrap().is_clean());
    assert_eq!(
        result_receiver
            .recv_timeout(Duration::from_secs(2))
            .unwrap(),
        Ok(())
    );
    assert!(service.shutdown().clean);
}

struct PanickingInput {
    drops: Arc<AtomicUsize>,
}

impl MixerInput for PanickingInput {
    fn render(&mut self, _output: &mut [MixerFrame]) -> MixerInputStatus {
        panic!("hostile input")
    }
}

impl Drop for PanickingInput {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::Relaxed);
    }
}

#[test]
fn render_panic_fails_closed_and_is_destroyed_only_off_render() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(4)).unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    let running = start_reserved(
        session.script_bus().try_reserve_input().unwrap(),
        Box::new(PanickingInput {
            drops: drops.clone(),
        }),
    );

    let mut output = [1.0; INTERNAL_BUFFER_FRAMES * 2];
    probe.render(&mut output);
    assert_samples(&output, 0.0);
    assert!(running.is_failed());
    assert_eq!(drops.load(Ordering::Relaxed), 0);

    let retirement = block_on(running.shutdown()).unwrap();
    assert!(retirement.failed_before_retirement);
    assert!(!retirement.source_destructor_panicked);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(service.shutdown().clean);
}

struct PanickingDestructor;

impl MixerInput for PanickingDestructor {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        output.fill(MixerFrame::ZERO);
        MixerInputStatus::Active
    }
}

impl Drop for PanickingDestructor {
    fn drop(&mut self) {
        panic!("hostile destructor")
    }
}

struct BlockingDestructor {
    entered: mpsc::SyncSender<()>,
    release: Arc<(Mutex<bool>, Condvar)>,
    drops: Arc<AtomicUsize>,
}

impl MixerInput for BlockingDestructor {
    fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
        output.fill(MixerFrame::ZERO);
        MixerInputStatus::Active
    }
}

impl Drop for BlockingDestructor {
    fn drop(&mut self) {
        let _ = self.entered.send(());
        let (released, ready) = &*self.release;
        let mut released = lock_recover(released);
        while !*released {
            released = ready
                .wait(released)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
        }
        self.drops.fetch_add(1, Ordering::Release);
    }
}

#[test]
fn hostile_destructor_is_contained_and_quarantines_capacity() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(5)).unwrap();
    let bus = session.script_bus();
    let running = start_reserved(
        bus.try_reserve_input().unwrap(),
        Box::new(PanickingDestructor),
    );
    assert_eq!(
        block_on(running.shutdown()),
        Err(MixerRetirementError::SourceDestructorPanicked)
    );
    assert!(matches!(
        bus.try_reserve_input(),
        Err(MixerControlError::InputCapacity)
    ));
    assert!(
        !service.shutdown().clean,
        "a quarantined destructor cannot produce a clean service proof"
    );
}

#[test]
fn forced_hostile_destructor_makes_service_shutdown_unclean_and_wakes_owner() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(36)).unwrap();
    let running = start_reserved(
        session.script_bus().try_reserve_input().unwrap(),
        Box::new(PanickingDestructor),
    );

    assert!(!service.shutdown().clean);
    assert_eq!(
        block_on(running.shutdown()),
        Err(MixerRetirementError::SourceDestructorPanicked)
    );
}

#[test]
fn early_driver_death_seals_control_notifies_each_live_input_once_and_joins() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(901)).unwrap();
    let bus = session.script_bus();
    let (notifications, received) = mpsc::sync_channel(2);
    let first_observer = Arc::new(FailureObserverProbe {
        bus: bus.clone(),
        notifications: notifications.clone(),
        calls: AtomicUsize::new(0),
        panic_before_publish: false,
        panic_after_publish: false,
    });
    let second_observer = Arc::new(FailureObserverProbe {
        bus: bus.clone(),
        notifications,
        calls: AtomicUsize::new(0),
        panic_before_publish: false,
        panic_after_publish: true,
    });
    let first = bus
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(ObservedInput {
            observer: Arc::clone(&first_observer),
        }))
        .unwrap();
    let second = bus
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(ObservedInput {
            observer: Arc::clone(&second_observer),
        }))
        .unwrap();
    let mut output = [0.0; 16];
    probe.render(&mut output);
    assert!(probe.fail_output());
    assert!(matches!(
        bus.try_reserve_input(),
        Err(MixerControlError::OwnerStopped)
    ));
    assert_eq!(bus.set_gain(0.75), Err(MixerControlError::OwnerStopped));
    let first_shutdown = first.shutdown();
    assert!(!second.suspend());

    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        MixerOutputFailure::BackendFailure
    );
    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        MixerOutputFailure::BackendFailure
    );
    eventually(|| bus.format() == Err(MixerControlError::OwnerStopped));
    assert_eq!(first_observer.calls.load(Ordering::Acquire), 1);
    assert_eq!(second_observer.calls.load(Ordering::Acquire), 1);

    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
    assert_eq!(
        second.output_failure(),
        Some(MixerOutputFailure::BackendFailure),
        "terminal cleanup must retain the exact cause for a late endpoint close"
    );
    let first_retirement = block_on(first_shutdown).unwrap();
    let second_retirement = block_on(second.shutdown()).unwrap();
    assert!(first_retirement.failed_before_retirement);
    assert!(second_retirement.failed_before_retirement);
    assert_eq!(
        first_retirement.output_failure,
        Some(MixerOutputFailure::BackendFailure)
    );
    assert_eq!(
        second_retirement.output_failure,
        Some(MixerOutputFailure::BackendFailure)
    );
    assert_eq!(first_observer.calls.load(Ordering::Acquire), 1);
    assert_eq!(second_observer.calls.load(Ordering::Acquire), 1);
}

#[test]
fn retirement_extraction_delivers_a_racing_output_failure_before_source_drop() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(911)).unwrap();
    let bus = session.script_bus();
    let (notifications, received) = mpsc::sync_channel(1);
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let notified_before_drop = Arc::new(AtomicBool::new(false));
    let observer = Arc::new(RetirementRaceObserver {
        notifications,
        calls: Arc::clone(&calls),
    });
    let running = bus
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(RetirementRaceInput {
            observer,
            drops: Arc::clone(&drops),
            notified_before_drop: Arc::clone(&notified_before_drop),
        }))
        .unwrap();

    let (scan_paused, scan_paused_receiver) = mpsc::sync_channel(1);
    let release_scan = Arc::new(Barrier::new(2));
    *lock_recover(&service.driver_status.retirement_scan_hook) =
        Some(Arc::new(RetirementScanHook {
            entered: scan_paused,
            release: Arc::clone(&release_scan),
            armed: AtomicBool::new(true),
        }));
    scan_paused_receiver
        .recv_timeout(Duration::from_secs(2))
        .expect("owner did not pause between its failure check and second retirement scan");

    assert!(probe.fail_output());
    let retirement = running.shutdown();
    release_scan.wait();

    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        MixerOutputFailure::BackendFailure
    );
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
    let retirement = block_on(retirement).unwrap();
    assert!(retirement.failed_before_retirement);
    assert_eq!(
        retirement.output_failure,
        Some(MixerOutputFailure::BackendFailure)
    );
    assert_eq!(calls.load(Ordering::Acquire), 1);
    assert_eq!(drops.load(Ordering::Acquire), 1);
    assert!(notified_before_drop.load(Ordering::Acquire));
}

#[test]
fn output_death_between_install_and_open_preserves_exact_cause_and_observer() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(908)).unwrap();
    let bus = session.script_bus();
    let (notifications, received) = mpsc::sync_channel(1);
    let observer = Arc::new(FailureObserverProbe {
        bus: bus.clone(),
        notifications,
        calls: AtomicUsize::new(0),
        panic_before_publish: false,
        panic_after_publish: false,
    });
    let reservation = bus.try_reserve_input().unwrap();
    let Ok(installed) = reservation.install_preboxed(Box::new(ObservedInput {
        observer: Arc::clone(&observer),
    })) else {
        panic!("live reservation must install");
    };

    assert!(probe.fail_output());
    let open_failure = installed.open().expect_err("dead output cannot open input");
    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        MixerOutputFailure::BackendFailure
    );
    assert_eq!(observer.calls.load(Ordering::Acquire), 1);
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
    let retirement = block_on(open_failure.shutdown()).unwrap();
    assert!(retirement.failed_before_retirement);
    assert_eq!(
        retirement.output_failure,
        Some(MixerOutputFailure::BackendFailure)
    );
}

#[test]
fn output_death_after_live_open_snapshot_rejects_before_running_publication() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(910)).unwrap();
    let bus = session.script_bus();
    let (notifications, received) = mpsc::sync_channel(1);
    let observer = Arc::new(FailureObserverProbe {
        bus: bus.clone(),
        notifications,
        calls: AtomicUsize::new(0),
        panic_before_publish: false,
        panic_after_publish: false,
    });
    let reservation = bus.try_reserve_input().unwrap();
    let Ok(installed) = reservation.install_preboxed(Box::new(ObservedInput {
        observer: Arc::clone(&observer),
    })) else {
        panic!("live reservation must install");
    };
    let (entered, entered_receiver) = mpsc::sync_channel(1);
    let release = Arc::new(Barrier::new(2));
    *lock_recover(&service.driver_status.open_snapshot_hook) = Some(Arc::new(OpenSnapshotHook {
        entered,
        release: Arc::clone(&release),
        armed: AtomicBool::new(true),
    }));

    let opening = thread::spawn(move || installed.open());
    entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    assert!(probe.fail_output());
    eventually(|| service.driver_status.failure().is_some());
    release.wait();
    let open_failure = opening
        .join()
        .unwrap()
        .expect_err("the final driver snapshot rejects the doomed endpoint");
    assert_eq!(open_failure.error, MixerControlError::OwnerStopped);

    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        MixerOutputFailure::BackendFailure
    );
    assert_eq!(observer.calls.load(Ordering::Acquire), 1);
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
    let retirement = block_on(open_failure.shutdown()).unwrap();
    assert!(retirement.failed_before_retirement);
    assert_eq!(
        retirement.output_failure,
        Some(MixerOutputFailure::BackendFailure)
    );
}

#[test]
fn service_shutdown_drains_an_admitted_open_before_marking_driver_closing() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(911)).unwrap();
    let reservation = session.script_bus().try_reserve_input().unwrap();
    let (source, _, _) = constant(0.125);
    let Ok(installed) = reservation.install_preboxed(source) else {
        panic!("live reservation must install");
    };
    let (entered, entered_receiver) = mpsc::sync_channel(1);
    let release = Arc::new(Barrier::new(2));
    *lock_recover(&service.driver_status.open_snapshot_hook) = Some(Arc::new(OpenSnapshotHook {
        entered,
        release: Arc::clone(&release),
        armed: AtomicBool::new(true),
    }));
    let control = Arc::clone(service.control.as_ref().unwrap());
    let status = Arc::clone(&service.driver_status);

    let opening = thread::spawn(move || installed.open());
    entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    let release_after_seal = thread::spawn(move || {
        eventually(|| lock_recover(&control.gate).production_sealed);
        let stayed_live = status.is_live();
        release.wait();
        stayed_live
    });
    let shutdown = service.shutdown();
    assert!(
        release_after_seal.join().unwrap(),
        "shutdown must drain admitted opens while live"
    );
    let running = opening
        .join()
        .unwrap()
        .expect("the admitted open completes before shutdown closes the driver");
    drop(running);
    drop(session);
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, None);
}

#[test]
fn output_death_after_final_snapshot_is_classified_as_owner_stopped() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(911)).unwrap();
    let reservation = session.script_bus().try_reserve_input().unwrap();
    let (input, _, _) = constant(0.25);
    let Ok(installed) = reservation.install_preboxed(input) else {
        panic!("live reservation must install");
    };
    let (entered, entered_receiver) = mpsc::sync_channel(1);
    let release = Arc::new(Barrier::new(2));
    *lock_recover(&service.driver_status.open_publish_hook) = Some(Arc::new(OpenSnapshotHook {
        entered,
        release: Arc::clone(&release),
        armed: AtomicBool::new(true),
    }));

    let opening = thread::spawn(move || installed.open());
    entered_receiver
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    assert!(probe.fail_output());
    eventually(|| service.driver_status.failure().is_some());
    release.wait();
    let open_failure = opening
        .join()
        .unwrap()
        .expect_err("device death must prevent Running publication");
    assert_eq!(open_failure.error, MixerControlError::OwnerStopped);

    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
    let retirement = block_on(open_failure.shutdown()).unwrap();
    assert!(retirement.failed_before_retirement);
    assert_eq!(
        retirement.output_failure,
        Some(MixerOutputFailure::BackendFailure)
    );
}

#[test]
fn reservations_acquired_before_output_death_abort_or_drop_without_leaking() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(909)).unwrap();
    let bus = session.script_bus();
    let aborted = bus.try_reserve_input().unwrap();
    let dropped = bus.try_reserve_input().unwrap();
    let aborted_slot = Arc::downgrade(&aborted.slot);
    let dropped_slot = Arc::downgrade(&dropped.slot);

    assert!(probe.fail_output());
    let retirement = block_on(aborted.abort()).unwrap();
    assert!(retirement.is_clean());
    assert_eq!(retirement.output_failure, None);
    drop(dropped);

    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
    eventually(|| aborted_slot.upgrade().is_none());
    eventually(|| dropped_slot.upgrade().is_none());
}

#[test]
fn observer_panic_before_publication_cannot_erase_failure_or_cleanup_proof() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(904)).unwrap();
    let bus = session.script_bus();
    let (notifications, received) = mpsc::sync_channel(1);
    let observer = Arc::new(FailureObserverProbe {
        bus: bus.clone(),
        notifications,
        calls: AtomicUsize::new(0),
        panic_before_publish: true,
        panic_after_publish: false,
    });
    let running = bus
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(ObservedInput {
            observer: Arc::clone(&observer),
        }))
        .unwrap();

    assert!(probe.fail_output());
    eventually(|| observer.calls.load(Ordering::Acquire) == 1);
    assert!(matches!(
        received.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
    let retirement = block_on(running.shutdown()).unwrap();
    assert!(retirement.failed_before_retirement);
    assert_eq!(
        retirement.output_failure,
        Some(MixerOutputFailure::BackendFailure)
    );
}

#[test]
fn owner_panic_is_first_writer_failure_and_cleanup_remains_autonomous() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(902)).unwrap();
    let bus = session.script_bus();
    let (notifications, received) = mpsc::sync_channel(1);
    let observer = Arc::new(FailureObserverProbe {
        bus: bus.clone(),
        notifications,
        calls: AtomicUsize::new(0),
        panic_before_publish: false,
        panic_after_publish: false,
    });
    let running = bus
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(ObservedInput {
            observer: Arc::clone(&observer),
        }))
        .unwrap();
    service
        .driver_status
        .panic_owner
        .store(true, Ordering::Release);

    assert_eq!(
        received.recv_timeout(Duration::from_secs(2)).unwrap(),
        MixerOutputFailure::OwnerPanicked
    );
    assert_eq!(observer.calls.load(Ordering::Acquire), 1);
    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::OwnerPanicked));
    let retirement = block_on(running.shutdown()).unwrap();
    assert!(retirement.failed_before_retirement);
}

#[test]
fn logical_close_that_wins_before_late_output_death_stays_normal() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(903)).unwrap();
    let bus = session.script_bus();
    let (notifications, received) = mpsc::sync_channel(1);
    let observer = Arc::new(FailureObserverProbe {
        bus: bus.clone(),
        notifications,
        calls: AtomicUsize::new(0),
        panic_before_publish: false,
        panic_after_publish: false,
    });
    let running = bus
        .try_reserve_input()
        .unwrap()
        .start_preboxed(Box::new(ObservedInput {
            observer: Arc::clone(&observer),
        }))
        .unwrap();

    let retirement = running.shutdown();
    assert!(probe.fail_output());
    let retirement = block_on(retirement).unwrap();
    assert!(retirement.is_clean());
    assert_eq!(retirement.output_failure, None);
    assert_eq!(observer.calls.load(Ordering::Acquire), 0);
    assert!(matches!(
        received.try_recv(),
        Err(mpsc::TryRecvError::Empty)
    ));

    let shutdown = service.shutdown();
    assert!(shutdown.clean);
    assert_eq!(shutdown.failure, Some(MixerOutputFailure::BackendFailure));
}

#[test]
fn missing_required_payload_is_structural_and_never_recycled_cleanly() {
    let probe = RenderProbe::default();
    let service = MixerService::start_with_driver_and_limits::<TestBackend>(
        settings(probe),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let session = service.add_session(AudioSessionId(37)).unwrap();
    let bus = session.script_bus();
    let (source, _, drops) = constant(0.25);
    let running = start_reserved(bus.try_reserve_input().unwrap(), source);
    // SAFETY: this test has no renderer entry and deliberately simulates a
    // corrupt Running state whose HAS_PAYLOAD bit no longer matches storage.
    let stolen = unsafe { (**running.slot.payload.get()).take() }.unwrap();
    assert_eq!(
        block_on(running.shutdown()),
        Err(MixerRetirementError::Structural)
    );
    assert!(matches!(
        bus.try_reserve_input(),
        Err(MixerControlError::InputCapacity)
    ));
    drop(stolen);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(!service.shutdown().clean);
}

fn standalone_running(
    control: &Arc<ControlInner>,
    drops: Arc<AtomicUsize>,
) -> (RunningMixerInput, Arc<InputSlot>, u64) {
    let slot = Arc::new(InputSlot::new(SlotAddress {
        session: SessionKey {
            id: AudioSessionId(38),
            generation: 1,
        },
        bus: SessionBus::Script,
        index: 0,
    }));
    let generation = slot.reserve().unwrap();
    assert!(
        slot.install(
            generation,
            Box::new(ConstantInput {
                frame: MixerFrame::ZERO,
                calls: Arc::new(AtomicUsize::new(0)),
                drops,
            }),
        )
        .is_ok()
    );
    assert!(slot.open(generation));
    (
        RunningMixerInput {
            slot: ManuallyDrop::new(Arc::clone(&slot)),
            generation,
            control: Arc::downgrade(control),
            driver_status: Arc::clone(&control.driver_status),
            session: Arc::new(SessionControl::new(slot.address.session)),
        },
        slot,
        generation,
    )
}

fn reclaim_retained_slot(shutdown: &mut MixerInputShutdown) -> Arc<InputSlot> {
    let slot = match &mut shutdown.state {
        ShutdownState::RetainedError {
            _slot: retained_slot,
            ..
        }
        | ShutdownState::Forced {
            slot: retained_slot,
            ..
        } => {
            // SAFETY: the test immediately marks the future Finished and takes
            // the sole retained authority exactly once for explicit cleanup.
            unsafe { ManuallyDrop::take(retained_slot) }
        }
        _ => panic!("shutdown did not retain strong slot authority"),
    };
    shutdown.state = ShutdownState::Finished;
    slot
}

#[test]
fn output_retirement_between_close_and_submission_keeps_the_exact_receipt() {
    for finish_before_submission in [false, true] {
        let (commands, _commands) = mpsc::sync_channel(1);
        let (retirements, requests) = mpsc::sync_channel(1);
        let control = test_control(commands, retirements);
        let drops = Arc::new(AtomicUsize::new(0));
        let (running, slot, generation) = standalone_running(&control, Arc::clone(&drops));
        let (entered, waiting) = mpsc::sync_channel(1);
        let release = Arc::new(Barrier::new(2));
        *lock_recover(&running.session.input_closed_hook) = Some(Arc::new(RetirementScanHook {
            entered,
            release: Arc::clone(&release),
            armed: AtomicBool::new(true),
        }));
        let close = thread::spawn(move || running.shutdown());
        waiting.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(slot.close_after_output_failure(generation, MixerOutputFailure::BackendFailure));
        let (retired_generation, mut prepared) = slot.prepare_forced_terminal(false).unwrap();
        assert_eq!(retired_generation, generation);
        if finish_before_submission {
            drop(prepared.source.take());
            slot.finish_terminal(generation, prepared.result).unwrap();
        }
        release.wait();
        let mut shutdown = close.join().unwrap();
        let waker = Waker::from(Arc::new(WakeProbe::default()));
        let mut cx = Context::from_waker(&waker);
        if !finish_before_submission {
            assert!(Pin::new(&mut shutdown).poll(&mut cx).is_pending());
            assert_eq!(drops.load(Ordering::Relaxed), 0);
            drop(prepared.source.take());
            slot.finish_terminal(generation, prepared.result).unwrap();
        }
        let receipt = block_on(shutdown).expect("concurrent retirement retains its receipt");
        assert_eq!(
            receipt.output_failure,
            Some(MixerOutputFailure::BackendFailure)
        );
        assert_eq!(drops.load(Ordering::Relaxed), 1);
        assert!(matches!(
            requests.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
    }
}

#[test]
fn retirement_queue_full_and_disconnect_retain_exact_strong_authority() {
    for disconnected in [false, true] {
        let (commands, _command_receiver) = mpsc::sync_channel(1);
        let (retirements, retirement_receiver) = mpsc::sync_channel(0);
        if disconnected {
            drop(retirement_receiver);
        }
        let control = test_control(commands, retirements);
        let drops = Arc::new(AtomicUsize::new(0));
        let (running, slot, generation) = standalone_running(&control, drops.clone());
        let mut shutdown = running.shutdown();
        let safe_waker = Waker::from(Arc::new(WakeProbe::default()));
        let mut context = Context::from_waker(&safe_waker);
        if disconnected {
            assert!(matches!(
                Pin::new(&mut shutdown).poll(&mut context),
                Poll::Pending
            ));
        } else {
            assert!(matches!(
                Pin::new(&mut shutdown).poll(&mut context),
                Poll::Ready(Err(MixerRetirementError::QueueInvariant))
            ));
        }
        assert!(Arc::strong_count(&slot) >= 2);
        assert_eq!(drops.load(Ordering::Relaxed), 0);

        let retained = reclaim_retained_slot(&mut shutdown);
        let mut prepared = retained.prepare_retirement(generation).unwrap();
        drop(prepared.source.take());
        let _ = retained.finish_terminal(generation, prepared.result);
        drop(retained);
        drop(slot);
        assert_eq!(drops.load(Ordering::Relaxed), 1);
    }
}

#[test]
fn cleanup_queue_full_retains_callback_without_owner_thread_drop() {
    let slot = Arc::new(InputSlot::new(SlotAddress {
        session: SessionKey {
            id: AudioSessionId(39),
            generation: 1,
        },
        bus: SessionBus::Script,
        index: 0,
    }));
    let generation = slot.reserve().unwrap();
    let drops = Arc::new(AtomicUsize::new(0));
    assert!(
        slot.install(
            generation,
            Box::new(ConstantInput {
                frame: MixerFrame::ZERO,
                calls: Arc::new(AtomicUsize::new(0)),
                drops: drops.clone(),
            }),
        )
        .is_ok()
    );
    assert!(slot.open(generation));
    assert!(slot.close(generation, false));
    let prepared = slot.prepare_retirement(generation).unwrap();
    let (completion, completed) = oneshot::channel();
    let job = CleanupJob {
        record: ReservedRecord {
            slot: Arc::clone(&slot),
            generation,
        },
        prepared,
        completion: Some(completion),
        terminal: false,
    };
    let (cleanup, _receiver) = mpsc::sync_channel(0);
    let Err(TrySendError::Full(job)) = cleanup.try_send(job) else {
        panic!("zero-capacity cleanup queue must reject without dropping the job");
    };
    retain_failed_cleanup_job(job);

    assert_eq!(
        block_on(completed).unwrap(),
        Err(MixerRetirementError::OwnerUncertain)
    );
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    assert_eq!(
        slot_phase(slot.word.load(Ordering::Acquire)),
        SlotPhase::Quarantined
    );
}

#[test]
fn failure_notification_full_or_disconnect_retains_all_authority() {
    for disconnected in [false, true] {
        let status = Arc::new(DriverStatus::new());
        let probe = RenderProbe::default();
        let mut backend_settings = settings(probe);
        backend_settings.enforce_owner_thread = false;
        let (mut mixer, _) = MixerCore::<TestBackend>::with_limits(
            backend_settings,
            Arc::clone(&status),
            MixerFormat {
                sample_rate: TEST_RATE,
            },
            1,
            1,
        )
        .unwrap();
        let session = mixer.add_session(AudioSessionId(905)).unwrap();
        let record = mixer
            .reserve(session.control.key, SessionBus::Script)
            .unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observer_drops = Arc::new(AtomicUsize::new(0));
        let source_drops = Arc::new(AtomicUsize::new(0));
        let observer = Arc::new(RetainedFailureObserver {
            calls: Arc::clone(&calls),
            drops: Arc::clone(&observer_drops),
        });
        assert!(
            record
                .slot
                .install(
                    record.generation,
                    Box::new(RetainedObservedInput {
                        observer: Arc::clone(&observer),
                        drops: Arc::clone(&source_drops),
                    }),
                )
                .is_ok()
        );
        assert!(record.slot.open(record.generation));
        drop(observer);

        let (cleanup, receiver) = mpsc::sync_channel(0);
        if disconnected {
            drop(receiver);
        }
        notify_failed_inputs(&mut mixer, MixerOutputFailure::BackendFailure, &cleanup);
        assert!(!mixer.cleanup_clean);
        assert_eq!(calls.load(Ordering::Acquire), 0);
        assert_eq!(observer_drops.load(Ordering::Acquire), 0);
        assert_eq!(source_drops.load(Ordering::Acquire), 0);

        assert!(retire_backend(&mut mixer));
        mixer.force_quarantine_all();
        drop(record);
        drop(mixer);
        assert_eq!(observer_drops.load(Ordering::Acquire), 0);
        assert_eq!(source_drops.load(Ordering::Acquire), 0);
    }
}

#[test]
fn impossible_kira_track_undercount_terminalizes_with_structural_session_error() {
    let status = Arc::new(DriverStatus::new());
    let probe = RenderProbe::default();
    let mut backend_settings = settings(probe);
    backend_settings.enforce_owner_thread = false;
    let (mut mixer, _) = MixerCore::<TestBackend>::with_limits(
        backend_settings,
        Arc::clone(&status),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        2,
        1,
    )
    .unwrap();
    let retiring = mixer.add_session(AudioSessionId(950)).unwrap();
    let _sibling = mixer.add_session(AudioSessionId(951)).unwrap();
    retiring.control.begin_close().unwrap();
    let completed = retiring.retirement;
    assert!(
        mixer
            .begin_session_retirement(SessionRetirementRequest {
                key: retiring.control.key,
            })
            .is_ok()
    );
    mixer.reported_sub_track_count = Some(0);
    let (cleanup_sender, _cleanup_receiver) = mpsc::sync_channel(1);

    assert!(!progress_session_retirements(
        &mut mixer,
        &cleanup_sender,
        &[],
        &status
    ));
    assert!(!mixer.cleanup_clean);
    assert!(retire_backend(&mut mixer));
    mixer.finish_session_retirements_after_backend(true);
    assert_eq!(
        block_on(completed).unwrap(),
        Err(MixerSessionRetirementError::Structural)
    );
}

#[test]
fn stalled_render_callbacks_get_post_resume_grace_then_terminalize() {
    let status = Arc::new(DriverStatus::new());
    let probe = RenderProbe::default();
    let mut backend_settings = settings(probe);
    backend_settings.enforce_owner_thread = false;
    let (mut mixer, _) = MixerCore::<TestBackend>::with_limits(
        backend_settings,
        Arc::clone(&status),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let retiring = mixer.add_session(AudioSessionId(952)).unwrap();
    retiring.control.begin_close().unwrap();
    let completed = retiring.retirement;
    assert!(
        mixer
            .begin_session_retirement(SessionRetirementRequest {
                key: retiring.control.key,
            })
            .is_ok()
    );
    mixer.close_drained_session_inputs();
    mixer.drop_ready_session_tracks();
    mixer
        .sessions
        .get_mut(&AudioSessionId(952))
        .unwrap()
        .retirement
        .as_mut()
        .unwrap()
        .tracks_dropped_at = Some(
        Instant::now()
            .checked_sub(SESSION_TRACK_RETIREMENT_TIMEOUT)
            .expect("the monotonic clock has advanced past the retirement timeout"),
    );
    // A callback would reduce this to zero after observing the dropped track.
    mixer.reported_sub_track_count = Some(1);
    mixer.session_track_retirement_timeout = Some(SESSION_TRACK_RETIREMENT_TIMEOUT);

    assert!(mixer.complete_rendered_session_retirements(&status));
    assert_eq!(status.failure(), None);
    assert!(
        mixer.sessions[&AudioSessionId(952)]
            .retirement
            .as_ref()
            .unwrap()
            .callback_stall
            .is_some()
    );

    // A callback arriving after the owner wakes proves the device made
    // progress and restarts the full grace period.
    status.record_render_callback();
    assert!(mixer.complete_rendered_session_retirements(&status));
    assert_eq!(status.failure(), None);
    mixer
        .sessions
        .get_mut(&AudioSessionId(952))
        .unwrap()
        .retirement
        .as_mut()
        .unwrap()
        .callback_stall
        .as_mut()
        .unwrap()
        .observed_at = Instant::now()
        .checked_sub(SESSION_TRACK_RETIREMENT_TIMEOUT)
        .expect("the monotonic clock has advanced past the retirement timeout");

    assert!(!mixer.complete_rendered_session_retirements(&status));
    assert_eq!(status.failure(), Some(MixerOutputFailure::CallbackStalled));
    assert!(!mixer.cleanup_clean);
    assert!(retire_backend(&mut mixer));
    mixer.finish_session_retirements_after_backend(true);
    assert_eq!(block_on(completed).unwrap(), Ok(()));
}

#[test]
fn mixer_managed_retirement_stall_publishes_terminal_event_before_owner_exit() {
    let status = Arc::new(DriverStatus::new());
    let probe = RenderProbe::default();
    let mut backend_settings = settings(probe);
    backend_settings.enforce_owner_thread = false;
    let backend_closes = Arc::clone(&backend_settings.backend_closes);
    let backend_drops = Arc::clone(&backend_settings.backend_drops);
    let (mut mixer, _) = MixerCore::<MixerManagedTestBackend>::with_limits(
        backend_settings,
        Arc::clone(&status),
        MixerFormat {
            sample_rate: TEST_RATE,
        },
        1,
        1,
    )
    .unwrap();
    let retiring = mixer.add_session(AudioSessionId(953)).unwrap();
    retiring.control.begin_close().unwrap();
    let completed = retiring.retirement;
    assert!(
        mixer
            .begin_session_retirement(SessionRetirementRequest {
                key: retiring.control.key,
            })
            .is_ok()
    );
    mixer.close_drained_session_inputs();
    mixer.drop_ready_session_tracks();
    let stale = Instant::now()
        .checked_sub(SESSION_TRACK_RETIREMENT_TIMEOUT)
        .unwrap();
    let retirement = mixer
        .sessions
        .get_mut(&AudioSessionId(953))
        .unwrap()
        .retirement
        .as_mut()
        .unwrap();
    retirement.tracks_dropped_at = Some(stale);
    retirement.callback_stall = Some(CallbackStallObservation {
        epoch: status.callback_epoch(),
        observed_at: stale,
    });
    // Kira still reports the dropped track because this test driver never
    // advances callbacks autonomously.
    mixer.reported_sub_track_count = Some(1);

    let (commands_sender, commands) = mpsc::sync_channel(1);
    let (retirements_sender, retirements) = mpsc::sync_channel(1);
    let (session_retirements_sender, session_retirements) = mpsc::sync_channel(1);
    let control = test_control_with_status(
        commands_sender,
        retirements_sender,
        session_retirements_sender,
        Arc::clone(&status),
    );
    let control_weak = Arc::downgrade(&control);
    let (cleanup_sender, cleanup_receiver) = mpsc::sync_channel(1);
    let (cleanup_results_sender, cleanup_results) = mpsc::sync_channel(1);
    let cleanup_owner = thread::spawn(move || {
        run_cleanup_worker(&cleanup_receiver, &cleanup_results_sender);
    });
    let (failure_sender, mut failures) = futures_mpsc::channel(1);
    let mut output_failure_sender = Some(failure_sender);
    let mut pending = Vec::new();

    run_active_owner(
        &mut mixer,
        &commands,
        &retirements,
        &session_retirements,
        &cleanup_sender,
        &cleanup_results,
        &control_weak,
        &mut pending,
        &status,
        &mut output_failure_sender,
    );

    assert_eq!(status.failure(), Some(MixerOutputFailure::CallbackStalled));
    assert!(lock_recover(&control.gate).production_sealed);
    assert_eq!(
        block_on(failures.next()),
        Some(MixerOutputFailure::CallbackStalled)
    );
    assert_eq!(block_on(failures.next()), None);
    assert!(!terminal_owner_cleanup(
        &mut mixer,
        &retirements,
        &session_retirements,
        &control_weak,
        &mut pending,
        cleanup_sender,
        &cleanup_results,
        cleanup_owner,
        &status,
    ));
    assert_eq!(block_on(completed).unwrap(), Ok(()));
    assert_eq!(backend_closes.load(Ordering::Relaxed), 1);
    assert_eq!(backend_drops.load(Ordering::Relaxed), 1);
}

#[test]
fn manual_driver_disables_wall_clock_stall_detection_except_when_injected() {
    assert_eq!(
        TestBackend::CALLBACK_STALL_POLICY,
        CallbackStallPolicy::Manual
    );
    assert_eq!(
        MixerCore::<TestBackend>::rendered_session_retirement_timeout(None),
        None
    );
    let injected = Duration::from_millis(7);
    assert_eq!(
        MixerCore::<TestBackend>::rendered_session_retirement_timeout(Some(injected)),
        Some(injected)
    );
}

#[test]
fn observer_authority_is_fail_closed_on_arbitrary_stack_unwind() {
    let calls = Arc::new(AtomicUsize::new(0));
    let drops = Arc::new(AtomicUsize::new(0));
    let observer = Arc::new(RetainedFailureObserver {
        calls,
        drops: Arc::clone(&drops),
    });
    {
        let mut authority = ObserverAuthority::new();
        authority.replace(Some(observer));
    }
    assert_eq!(drops.load(Ordering::Acquire), 0);
}

#[test]
fn service_shutdown_force_cleans_live_endpoints_and_future_is_ready() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(6)).unwrap();
    let (source, _, drops) = constant(0.25);
    let running = start_reserved(session.script_bus().try_reserve_input().unwrap(), source);

    assert!(service.shutdown().clean);
    assert!(!probe.is_live());
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(block_on(running.shutdown()).unwrap().is_clean());
}

#[test]
fn sealed_owner_returns_source_and_reservation_for_honest_cleanup() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(8)).unwrap();
    let reservation = session.script_bus().try_reserve_input().unwrap();
    assert!(service.shutdown().clean);

    let (source, _, drops) = constant(0.5);
    let MixerInputStartFailure::Rejected(failure) = reservation.start_preboxed(source).unwrap_err()
    else {
        panic!("sealed owner must reject before installation");
    };
    assert_eq!(failure.error(), MixerControlError::OwnerStopped);
    let (error, reservation, source) = failure.into_parts();
    assert_eq!(error, MixerControlError::OwnerStopped);
    assert_eq!(drops.load(Ordering::Relaxed), 0);
    drop(source);
    assert_eq!(drops.load(Ordering::Relaxed), 1);
    assert!(block_on(reservation.abort()).unwrap().is_clean());
}

#[test]
fn backend_failures_and_rate_mismatch_are_typed_and_joined() {
    let setup_probe = RenderProbe::default();
    let setup_error = MixerService::start_with_driver::<TestBackend>(
        TestBackendSettings {
            fail_setup: true,
            ..settings(setup_probe.clone())
        },
        TEST_RATE,
    )
    .unwrap_err();
    assert!(matches!(
        setup_error,
        MixerStartError::Backend(TestBackendError::Setup)
    ));
    assert!(!setup_probe.is_live());

    let start_probe = RenderProbe::default();
    let start_closes = Arc::new(AtomicUsize::new(0));
    let start_drops = Arc::new(AtomicUsize::new(0));
    let start_error = MixerService::start_with_driver::<TestBackend>(
        TestBackendSettings {
            fail_start: true,
            backend_closes: start_closes.clone(),
            backend_drops: start_drops.clone(),
            ..settings(start_probe.clone())
        },
        TEST_RATE,
    )
    .unwrap_err();
    assert!(matches!(
        start_error,
        MixerStartError::Backend(TestBackendError::Start)
    ));
    assert!(!start_probe.is_live());
    assert_eq!(start_closes.load(Ordering::Relaxed), 1);
    assert_eq!(start_drops.load(Ordering::Relaxed), 1);

    let rate_probe = RenderProbe::default();
    let rate_closes = Arc::new(AtomicUsize::new(0));
    let rate_drops = Arc::new(AtomicUsize::new(0));
    let rate_error = MixerService::start_with_driver::<TestBackend>(
        TestBackendSettings {
            actual_rate: 44_100,
            backend_closes: rate_closes.clone(),
            backend_drops: rate_drops.clone(),
            ..settings(rate_probe.clone())
        },
        TEST_RATE,
    )
    .unwrap_err();
    assert!(matches!(
        rate_error,
        MixerStartError::SampleRateMismatch {
            expected: TEST_RATE,
            actual: 44_100
        }
    ));
    assert!(!rate_probe.is_live());
    assert_eq!(rate_closes.load(Ordering::Relaxed), 1);
    assert_eq!(rate_drops.load(Ordering::Relaxed), 1);
}

#[test]
fn invalid_gain_is_rejected_before_owner_mutation() {
    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(11)).unwrap();
    let bus = session.script_bus();
    assert_eq!(bus.set_gain(-0.1), Err(MixerControlError::InvalidGain));
    assert_eq!(bus.set_gain(f32::NAN), Err(MixerControlError::InvalidGain));
    assert_eq!(
        bus.set_gain(f32::INFINITY),
        Err(MixerControlError::InvalidGain)
    );
    assert!(service.shutdown().clean);
}

#[test]
fn close_is_absorbing_even_after_finished_callback() {
    struct OneShot(Arc<AtomicBool>);

    impl MixerInput for OneShot {
        fn render(&mut self, output: &mut [MixerFrame]) -> MixerInputStatus {
            output.fill(MixerFrame::from_mono(0.5));
            self.0.store(true, Ordering::Release);
            MixerInputStatus::Finished
        }
    }

    let probe = RenderProbe::default();
    let service =
        MixerService::start_with_driver::<TestBackend>(settings(probe.clone()), TEST_RATE).unwrap();
    let session = service.add_session(AudioSessionId(12)).unwrap();
    let rendered = Arc::new(AtomicBool::new(false));
    let running = start_reserved(
        session.script_bus().try_reserve_input().unwrap(),
        Box::new(OneShot(rendered.clone())),
    );
    let mut output = [0.0; INTERNAL_BUFFER_FRAMES * 2];
    probe.render(&mut output);
    assert_samples(&output, 0.5);
    assert!(rendered.load(Ordering::Acquire));
    assert!(!running.resume());
    probe.render(&mut output);
    assert_samples(&output, 0.0);
    assert!(block_on(running.shutdown()).unwrap().is_clean());
    assert!(service.shutdown().clean);
}
