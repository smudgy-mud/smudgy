#![cfg(feature = "web-audio")]

//! File-source script graph, lifecycle, and quota integration.
use deno_audio::{
    AudioEventPumpLiveness, AudioExtensionOptions, AudioFileOpener, AudioFileResolver, AudioHost,
    AudioHostLimits, AudioLimits, SilentAudioOutput, install_audio_file_resolver,
};
use deno_core::{JsRuntime, OpState, PollEventLoopOptions, RuntimeOptions};
use deno_error::JsErrorBox;
use std::fs::File;
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

struct Fixtures(PathBuf);
impl Fixtures {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "audio-file-binding-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        for (name, frames) in [("short", 256_u32), ("medium", 9_600), ("long", 480_000)] {
            let mut wav = Vec::new();
            wav.extend_from_slice(b"RIFF");
            wav.extend_from_slice(&(36 + frames * 2).to_le_bytes());
            wav.extend_from_slice(b"WAVEfmt ");
            wav.extend_from_slice(&16_u32.to_le_bytes());
            wav.extend_from_slice(&1_u16.to_le_bytes());
            wav.extend_from_slice(&1_u16.to_le_bytes());
            wav.extend_from_slice(&48_000_u32.to_le_bytes());
            wav.extend_from_slice(&96_000_u32.to_le_bytes());
            wav.extend_from_slice(&2_u16.to_le_bytes());
            wav.extend_from_slice(&16_u16.to_le_bytes());
            wav.extend_from_slice(b"data");
            wav.extend_from_slice(&(frames * 2).to_le_bytes());
            for _ in 0..frames {
                wav.extend_from_slice(&8192_i16.to_le_bytes());
            }
            std::fs::write(root.join(name), wav).unwrap();
        }
        std::fs::write(root.join("bad"), b"not audio").unwrap();
        Self(root)
    }
}
impl Drop for Fixtures {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).unwrap();
    }
}
struct Resolver(PathBuf);
impl AudioFileResolver for Resolver {
    fn resolve(&self, _state: &OpState, source: &str) -> Result<AudioFileOpener, JsErrorBox> {
        if !["short", "medium", "long", "bad"].contains(&source) {
            return Err(JsErrorBox::type_error("denied source"));
        }
        let path = self.0.join(source);
        Ok(Box::new(move || File::open(path)))
    }
}
fn runtime(host: Arc<AudioHost>, limits: AudioLimits, fixtures: Option<&Fixtures>) -> JsRuntime {
    let runtime = JsRuntime::new(RuntimeOptions {
        extensions: vec![
            deno_webidl::deno_webidl::init(),
            deno_web::deno_web::init(
                deno_web::BlobStore::default_arc(),
                None,
                Default::default(),
                deno_web::InMemoryBroadcastChannel::default(),
            ),
            deno_audio::deno_audio::init(
                AudioExtensionOptions::new(host)
                    .limits(limits)
                    .event_pump_liveness(AudioEventPumpLiveness::Ref)
                    .output_factory(Arc::new(SilentAudioOutput::new())),
            ),
            deno_core::Extension {
                name: "smudgy_media_test",
                esm_files: std::borrow::Cow::Owned(vec![
                    deno_core::ExtensionFileSource::new(
                        "ext:smudgy_media_test/media.js",
                        deno_core::ascii_str_include!("../src/media.js"),
                    ),
                    deno_core::ExtensionFileSource::new(
                        "ext:smudgy_media_test/entry.js",
                        deno_core::ascii_str!(
                            "import * as media from 'ext:smudgy_media_test/media.js'; globalThis.__testMedia = media;"
                        ),
                    ),
                ]),
                esm_entry_point: Some("ext:smudgy_media_test/entry.js"),
                ..Default::default()
            },
        ],
        ..RuntimeOptions::default()
    });
    if let Some(fixtures) = fixtures {
        install_audio_file_resolver(
            &mut runtime.op_state().borrow_mut(),
            Arc::new(Resolver(fixtures.0.clone())),
        );
    }
    runtime
}
async fn run(runtime: &mut JsRuntime, script: &'static str) {
    runtime.execute_script("file_source.js", format!("globalThis.done = false; (async () => {{ const {{ createFileSource, AudioFileSourceNode, Audio }} = globalThis.__testMedia; {script}\n globalThis.done = true; }})();")).unwrap();
    tokio::time::timeout(
        Duration::from_secs(15),
        runtime.run_event_loop(PollEventLoopOptions::default()),
    )
    .await
    .unwrap()
    .unwrap();
    runtime
        .execute_script(
            "assert_done.js",
            "if (!done) throw new Error('file source test did not complete');",
        )
        .unwrap();
}

#[tokio::test]
async fn media_player_preloads_replays_and_releases_contexts_before_ended() {
    let fixtures = Fixtures::new();
    let host = Arc::new(AudioHost::new(
        AudioHostLimits::unlimited()
            .max_online_contexts(Some(1))
            .max_streaming_jobs(Some(1)),
    ));
    let mut runtime = runtime(Arc::clone(&host), AudioLimits::default(), Some(&fixtures));
    run(&mut runtime, r"
        if ('Audio' in globalThis) throw new Error('Audio leaked into globals');
        const audio = new Audio();
        if (audio.src !== '' || !audio.paused || audio.ended || audio.error !== null || audio.volume !== 1 || audio.muted) throw new Error('bad defaults');
        let refused = false;
        try { await audio.play(); } catch (e) { refused = e.name === 'NotSupportedError'; }
        if (!refused) throw new Error('empty playback accepted');
        audio.volume = .3; audio.muted = true; audio.muted = false;
        for (const volume of [-1, 1.1, NaN, Infinity]) {
            refused = false; try { audio.volume = volume; } catch { refused = true; }
            if (!refused || audio.volume !== .3) throw new Error('invalid volume mutated player');
        }
        const loaded = new Promise(resolve => { audio.oncanplay = resolve; });
        audio.src = 'short'; await loaded;
        if (!audio.paused || audio.ended || audio.error !== null) throw new Error('preload started playback');
        for (let i = 0; i < 3; ++i) {
            const ended = new Promise(resolve => { audio.onended = resolve; });
            await audio.play(); await ended;
            if (!audio.ended || !audio.paused || audio.error !== null) throw new Error('bad natural completion');
        }
        await audio.close(); await audio.close();
        if (audio.src !== 'short' || !audio.paused || audio.ended) throw new Error('close did not reset reusable player');
    ").await;
    assert_eq!(host.usage().online_contexts(), 0);
    assert_eq!(host.usage().streaming_jobs(), 0);
    assert_eq!(host.usage().scheduled_sources(), 0);
}

#[tokio::test]
async fn media_player_pause_interrupts_pending_play_and_resumes_without_ending() {
    let fixtures = Fixtures::new();
    let host = Arc::new(AudioHost::default());
    let mut runtime = runtime(Arc::clone(&host), AudioLimits::default(), Some(&fixtures));
    run(&mut runtime, r"
        const audio = new Audio('medium');
        let endedCount = 0, playingCount = 0;
        audio.onended = () => endedCount++;
        audio.onplaying = () => playingCount++;
        const interrupted = audio.play().then(() => { throw new Error('interrupted play resolved'); }, e => {
            if (e.name !== 'AbortError') throw e;
        });
        audio.pause(); await interrupted;
        if (!audio.paused || audio.ended) throw new Error('bad paused state');
        await Promise.all([audio.play(), audio.play()]);
        audio.pause();
        // A separate context keeps advancing while the player's private context
        // is suspended. Longer than the complete file, so mute-only pause fails.
        const clock = new AudioContext();
        const timer = clock.createOscillator();
        const waited = new Promise(resolve => { timer.onended = resolve; });
        timer.start(); timer.stop(clock.currentTime + .3); await waited; await clock.close();
        if (endedCount !== 0 || !audio.paused) throw new Error('paused file kept consuming PCM');
        const ended = new Promise(resolve => { audio.onended = () => { endedCount++; resolve(); }; });
        await audio.play(); await ended;
        if (endedCount !== 1 || playingCount !== 2) throw new Error('duplicate lifecycle events');
        await audio.close();
    ").await;
    assert_eq!(host.usage().online_contexts(), 0);
    assert_eq!(host.usage().streaming_jobs(), 0);
}

#[tokio::test]
async fn media_player_replacement_and_close_cancel_loads_without_stale_events() {
    let fixtures = Fixtures::new();
    let host = Arc::new(AudioHost::new(
        AudioHostLimits::unlimited()
            .max_online_contexts(Some(1))
            .max_streaming_jobs(Some(1)),
    ));
    let mut runtime = runtime(Arc::clone(&host), AudioLimits::default(), Some(&fixtures));
    run(&mut runtime, r"
        const audio = new Audio('long');
        let errors = 0, endedCount = 0;
        audio.onerror = () => errors++;
        audio.onended = () => endedCount++;
        const cancelled = audio.play().then(() => { throw new Error('replaced play resolved'); }, e => {
            if (e.name !== 'AbortError') throw e;
        });
        audio.src = 'denied'; audio.src = 'bad'; audio.src = 'long';
        await cancelled;
        await audio.play();
        // Replace a running decoder with a full backpressure queue under a
        // single context/worker allowance. Replacement must join before reopen.
        audio.src = 'short';
        const ended = new Promise(resolve => { audio.onended = () => { endedCount++; resolve(); }; });
        await audio.play(); await ended;
        if (errors !== 0 || endedCount !== 1) throw new Error('stale load or ended event');
        audio.src = 'long';
        const pending = audio.play().catch(e => { if (e.name !== 'AbortError') throw e; });
        await audio.close(); await pending;
        audio.src = '';
        await audio.close();
    ").await;
    assert_eq!(host.usage().online_contexts(), 0);
    assert_eq!(host.usage().streaming_jobs(), 0);
}

#[tokio::test]
async fn media_player_load_errors_are_observable_and_retry_requires_load() {
    let fixtures = Fixtures::new();
    let host = Arc::new(AudioHost::new(
        AudioHostLimits::unlimited()
            .max_online_contexts(Some(1))
            .max_streaming_jobs(Some(1)),
    ));
    let mut runtime = runtime(Arc::clone(&host), AudioLimits::default(), Some(&fixtures));
    run(&mut runtime, r"
        const audio = new Audio();
        let endedCount = 0;
        audio.onended = () => endedCount++;
        for (const source of ['denied', 'bad']) {
            const failed = new Promise(resolve => { audio.onerror = resolve; });
            audio.src = source;
            await failed;
            if (!audio.error || !audio.paused || audio.ended) throw new Error('bad error state');
            let rejected = false;
            try { await audio.play(); } catch (e) { rejected = e === audio.error; }
            if (!rejected) throw new Error('failed play did not reject with load error');
        }
        audio.src = 'short';
        audio.load();
        const ended = new Promise(resolve => { audio.onended = resolve; });
        await audio.play(); await ended;
        if (audio.error !== null || endedCount !== 0) throw new Error('failed source emitted ended or error persisted');
        await audio.close();
    ").await;
    assert_eq!(host.usage().online_contexts(), 0);
    assert_eq!(host.usage().streaming_jobs(), 0);
}

#[tokio::test]
async fn media_player_paused_resources_are_bounded_and_close_allows_another_player() {
    let fixtures = Fixtures::new();
    let host = Arc::new(AudioHost::new(
        AudioHostLimits::unlimited()
            .max_online_contexts(Some(1))
            .max_streaming_jobs(Some(1)),
    ));
    let mut runtime = runtime(Arc::clone(&host), AudioLimits::default(), Some(&fixtures));
    run(&mut runtime, r"
        const first = new Audio('long');
        await first.play(); first.pause();
        const second = new Audio('short');
        let refused = false;
        try { await second.play(); } catch (e) { refused = e.name === 'QuotaExceededError'; }
        if (!refused) throw new Error('paused context bypassed shared quota');
        await first.close();
        // Ready handlers can synchronously replace their source. The old
        // observer and pending state transitions must not start the new file.
        let replaced = false;
        second.oncanplay = () => {
            if (!replaced) { replaced = true; second.src = 'short'; }
        };
        second.load();
        const interrupted = second.play().then(() => { throw new Error('reentrant load resolved old play'); }, e => {
            if (e.name !== 'AbortError') throw e;
        });
        await interrupted;
        const ended = new Promise(resolve => { second.onended = resolve; });
        await second.play(); await ended;
        await second.close();
    ").await;
    assert_eq!(host.usage().online_contexts(), 0);
    assert_eq!(host.usage().streaming_jobs(), 0);
    assert_eq!(host.usage().scheduled_sources(), 0);
}

#[tokio::test]
async fn file_node_connects_ends_once_and_rejects_cross_context_and_restarts() {
    let fixtures = Fixtures::new();
    let host = Arc::new(AudioHost::default());
    let mut runtime = runtime(Arc::clone(&host), AudioLimits::default(), Some(&fixtures));
    run(&mut runtime, r"
        if ('createFileSource' in AudioContext.prototype || 'AudioFileSourceNode' in globalThis) throw new Error('Web Audio globals were extended');
        let invalid = false;
        try { new AudioFileSourceNode(); } catch { invalid = true; }
        if (!invalid) throw new Error('custom node constructor exposed');
        invalid = false;
        try { await createFileSource(new OfflineAudioContext(1, 128, 48000), 'short'); } catch { invalid = true; }
        if (!invalid) throw new Error('offline context accepted');
        const context = new AudioContext(), other = new AudioContext();
        const source = await createFileSource(context, 'short');
        if (!(source instanceof AudioFileSourceNode) || !(source instanceof AudioNode)) throw new Error('wrong prototype');
        if (source.numberOfInputs !== 0 || source.numberOfOutputs !== 1 || source.error !== null) throw new Error('wrong source shape');
        let refused = false;
        try { source.connect(other.destination); } catch (e) { refused = e.name === 'InvalidAccessError'; }
        if (!refused) throw new Error('cross-context connect accepted');
        const gain = context.createGain(); gain.gain.value = .5;
        source.connect(gain); gain.connect(context.destination);
        let count = 0;
        const ended = new Promise(resolve => { source.onended = () => { count++; resolve(); }; });
        source.start();
        try { source.start(); throw new Error('restart accepted'); } catch(e) { if (e.name !== 'InvalidStateError') throw e; }
        await ended;
        source.stop(); source.stop();
        await context.close(); await other.close();
        if (count !== 1 || source.error !== null) throw new Error('incorrect completion');
    ").await;
    assert_eq!(host.usage().streaming_jobs(), 0);
    assert_eq!(host.usage().scheduled_sources(), 0);
}

#[tokio::test]
async fn file_refusals_and_cancelled_full_queue_leave_slots_reusable() {
    let fixtures = Fixtures::new();
    let host = Arc::new(AudioHost::new(
        AudioHostLimits::unlimited().max_streaming_jobs(Some(1)),
    ));
    let mut runtime = runtime(Arc::clone(&host), AudioLimits::default(), Some(&fixtures));
    run(
        &mut runtime,
        r"
        const c = new AudioContext();
        for (const name of ['bad', 'denied']) {
            let refused = false; try { await createFileSource(c, name); } catch { refused = true; }
            if (!refused) throw new Error('invalid source accepted');
        }
        const long = await createFileSource(c, 'long');
        let refused = false; try { await createFileSource(c, 'long'); } catch { refused = true; }
        if (!refused) throw new Error('stream quota ignored');
        long.connect(c.destination);
        const ended = new Promise(resolve => { long.onended = resolve; });
        long.start(); long.stop(); await ended;
        const replacement = await createFileSource(c, 'short'); replacement.stop();
        await c.close();
    ",
    )
    .await;
    assert_eq!(host.usage().streaming_jobs(), 0);
    assert_eq!(host.usage().scheduled_sources(), 0);
}

#[tokio::test]
async fn close_drains_running_file_and_paused_load_cancels() {
    let fixtures = Fixtures::new();
    let host = Arc::new(AudioHost::default());
    let mut runtime = runtime(Arc::clone(&host), AudioLimits::default(), Some(&fixtures));
    run(&mut runtime, r"
        const c = new AudioContext(); const source = await createFileSource(c, 'long');
        source.connect(c.destination); let ended = 0; source.onended = () => ended++;
        source.start(); await c.close();
        if (ended !== 1) throw new Error('close lost file completion');
        const d = new AudioContext();
        const pending = createFileSource(d, 'long').then(() => { throw new Error('closed load succeeded'); }, () => {});
        await d.close(); await pending;
        const e = new AudioContext(); let closing, rejected = false;
        try { await createFileSource(e, { toString() { closing = e.close(); return 'long'; } }); }
        catch { rejected = true; }
        await closing;
        if (!rejected) throw new Error('source conversion admitted a worker after close');
    ").await;
    assert_eq!(host.usage().streaming_jobs(), 0);
}

#[tokio::test]
async fn absent_resolver_and_file_size_limit_fail_closed() {
    let fixtures = Fixtures::new();
    for enabled in [false, true] {
        let host = Arc::new(AudioHost::default());
        let mut runtime = runtime(
            Arc::clone(&host),
            AudioLimits::default().max_streaming_file_bytes(Some(8)),
            enabled.then_some(&fixtures),
        );
        run(
            &mut runtime,
            r"
            const c = new AudioContext(); let refused = false;
            try { await createFileSource(c, 'short'); } catch { refused = true; }
            if (!refused) throw new Error('source should be refused'); await c.close();
        ",
        )
        .await;
        assert_eq!(host.usage().streaming_jobs(), 0);
    }
}

#[tokio::test]
async fn close_joins_unstarted_sources_and_failed_start_keeps_source_reusable() {
    let fixtures = Fixtures::new();
    let host = Arc::new(AudioHost::new(
        AudioHostLimits::unlimited().max_scheduled_sources(Some(1)),
    ));
    let mut runtime = runtime(Arc::clone(&host), AudioLimits::default(), Some(&fixtures));
    run(
        &mut runtime,
        r"
        const c = new AudioContext();
        const first = await createFileSource(c, 'long');
        const second = await createFileSource(c, 'long');
        first.start();
        let refused = false; try { second.start(); } catch { refused = true; }
        if (!refused) throw new Error('scheduled source limit ignored');
        const ended = new Promise(resolve => { first.onended = resolve; });
        first.stop(); await ended;
        second.start(); second.stop(); await c.close();
        const d = new AudioContext();
        await createFileSource(d, 'long');
        await d.close();
    ",
    )
    .await;
    assert_eq!(host.usage().streaming_jobs(), 0);
    assert_eq!(host.usage().scheduled_sources(), 0);
}
