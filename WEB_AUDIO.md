# Web Audio support

Smudgy has a bounded, standards-shaped Web Audio integration for short
scripted synthesis and local audio-file playback, including earcons and recorded
speech. It is available to trusted
modules and sandboxed packages through the same per-session output boundary.

Official Windows, macOS, and Flatpak build paths select the physical Web Audio
feature. The API remains available through hosted silent emulation when a
compatible physical output is absent, full, or unavailable at startup. Platform
runtime evidence is listed below; configured packaging is not itself a runtime
or device-support claim.

Audio is optional: an unavailable physical output or lifecycle worker never
prevents the terminal client or Web Audio globals from starting. A no-argument
default `AudioContext` and explicit `sinkId: "none"` both remain normal,
joinable graphs backed by hosted silent output. The visible controls can save a
next-physical-start preference in that emulated mode and label it "not
applied."

After physical output starts, recoverable CPAL runtime events such as device
loss, stream invalidation, backend failure, or stalled render callbacks rebuild
the endpoint in process against the then-current compatible default device.
The process mixer is retained, so its logical contexts, sessions, inputs, and
volume/mute controls remain live. While no compatible default can be opened,
the mixer advances silently and retries with bounded exponential backoff.
Permanent or protocol-level backend errors, or any failure that leaves native
stream teardown uncertain, still terminalize physical output for the launch
and require a process restart. A stale or closing live session, inactive
package scope, or terminal physical-output failure is a visible control failure
and is not persisted as a successful change.

## Volume and mute controls

The Settings window's Audio pane exposes one master row, one row for every
active session, and one row for each enabled sandbox package root. Trusted packages run on
Main and are explicitly labeled as session-controlled; Smudgy does not claim
truthful per-package control inside that shared isolate.

Volume is stored as a whole percentage from 0 through 100 and mapped linearly
to mixer gain 0.0 through 1.0. Mute is independent, so unmuting restores the
remembered volume. Master policy is global. Session policy uses the durable
server/profile identity, and sandbox policy adds its folded, versionless
owner/name root. Non-default package policies remain available across
uninstall/reinstall and version changes; default rows are compacted away.

Open the pane from the toolbar's Audio button, from Settings itself, or with
`Ctrl+Shift+A` (`Command+Shift+A` on macOS); every entry lands on the pane with
the master row focused. `Tab` and `Shift+Tab` cycle its focusable rows without
a pointer. On a focused
row, the arrow keys adjust volume by five points, `Home` and `End` select 0 and
100, and `Space` toggles mute. Focus is visibly outlined and off-screen rows
are scrolled into view.

These are visible, keyboard-focusable controls, not a full screen-reader
semantics claim. Smudgy carries narrow patch-crate changes to iced runtime and
winit that expose one AccessKit live region per native window. The Audio pane
uses it only for its localized saved-preference and failure feedback — a
routine applied-and-saved change is silent. It does not describe the controls,
their values, focus, or the rest of the widget tree, so full screen-reader
navigation remains out of scope.

Each window holds at most one pending announcement of at most 4 KiB. The newest
valid request in an event-loop batch replaces the earlier one; empty and
oversize requests are ignored. Inactive accessibility drops requests and never
replays them after activation. Repeated identical feedback is reset and sent
again through the same stable live-region node. Closing a window makes its id
stale before native teardown, so pending or later feedback cannot cross into a
different window.

Announcements contain only localized control outcomes. Detailed errors may
remain visible in the panel and in existing diagnostic logs, but raw error text
is never copied into the AccessKit update. Announcement text is not logged or
persisted. The effect is best effort: completion means only that iced accepted
the request, not that assistive technology was active or that speech occurred.
This bridge is not TTS, does not use a Web Audio bus, and exposes no script API.

### Screen-reader evidence

The bridge pins `accesskit` 0.24.1 and `accesskit_winit` 0.33.2. Automated tests
cover the fixed two-node tree, bounds and coalescing, repeated text, inactive
lifecycle, multi-window stale routing, event ordering, and native-window
retirement. Those tests do not establish end-user screen-reader support.

Run the dependency-patch unit tests on Windows, macOS, or Linux with:

```sh
python bin/test-iced-accessibility-patches.py
```

The helper rematerializes the tracked patches, copies the patched crates to a
temporary workspace, restricts resolution to package versions already present
in Smudgy's lock file, and runs both dependency test suites with `--locked`.
CI runs this command once on each supported desktop OS.

Manual packaged tests with NVDA on Windows, VoiceOver on macOS, and Orca on both
X11 and Wayland remain pending. Until those literal-text checks are recorded,
this document makes no platform speech or exact-once delivery claim.

Each evidence record must include the exact Smudgy commit, OS and packaged
build, AccessKit versions and enabled features, and screen-reader version and
settings. Linux records must additionally name the desktop, AT-SPI version, and
X11 or Wayland session. Use the same literal Unicode feedback and capture it in
NVDA Speech Viewer, VoiceOver Caption Panel, or Orca speech/debug output; a tree
inspector alone is insufficient. Run with the reader active before launch and
activated after launch. Check exact-once polite success and assertive failure,
identical-text repetition, rapid latest-wins replacement, independent windows,
unchanged focus, queued-close and post-close stale rejection, close/reopen, and
deactivate/reactivate without replay. The Windows record must also exercise the
existing Restart Manager and rounded-frame native subclass hooks.

## Build features

| Configuration | Web Audio availability | Output |
| --- | --- | --- |
| No feature (crate default) | Not installed in ordinary sessions | No audio device dependency |
| `web-audio` | Available only to callers that construct an explicit audio-scoped session | An injected logical output, including `sinkId: "none"`; no CPAL device backend |
| `web-audio-cpal` (official desktop packages) | Enables the desktop audio coordinator and includes `web-audio` | One shared current-default-device stream when available; startup unavailability uses hosted silent emulation, while recoverable runtime loss retains and silently advances the process mixer as the physical endpoint retries |

For a physical source build:

```sh
cargo run -p smudgy_ui --features web-audio-cpal
```

`web-audio-cpal` reaches CPAL only through
`smudgy_audio/physical-output`. It uses WASAPI on Windows, ALSA on Linux, and
CoreAudio on macOS. Linux source builds need the ALSA development package
(`libasound2-dev` on Ubuntu/Debian). The current process mixer runs at 48,000 Hz
stereo and rejects a default device that cannot provide that exact format.

The hardware-free feature is primarily for deterministic tests and embedders.
Selecting it on `smudgy_ui` alone does not silently create audio authority for
ordinary desktop sessions.

Audible default contexts use the shared fixed 48,000 Hz mixer. An explicitly
requested non-48 kHz audible default context is currently rejected as
unsupported; assets at other rates are resampled by Web Audio within a 48 kHz
context. Hosted silent/emulated contexts have no hardware-rate contract and may
retain another requested logical context rate. This leaves a clear future seam
for context-to-output resampling without reconfiguring the process device.

## Supported hosted surface

The generated script declarations intentionally describe only this supported
online surface:

| Surface | Supported behavior |
| --- | --- |
| `AudioContext` | Default session output or `sinkId: "none"`; if physical output is unavailable, either form remains a silent, time-advancing context; `suspend()`, `resume()`, and absorbing `close()` |
| `destination` | The context's private destination on its session Script bus |
| `GainNode` | Construction, connection, and scalar `gain.value` mutation |
| `OscillatorNode` | Sine, square, sawtooth, and triangle waves; scalar frequency/detune; `start()`, `stop()`, and `ended` |
| Connections | Exact node/parameter `connect()` and `disconnect()` forms for the hosted graph |

Other node families, custom/periodic waves, `AudioParam` automation methods,
offline rendering, whole-buffer decoding, worklets, media capture, and device selection are
outside this hosted compatibility promise. The pinned alpha extension may
expose additional working symbols, but packages cannot rely on those as Smudgy
product surface. Operations that the hosted layer rejects are validated before
they mutate its mixer.

### Local audio files

For earcons and recorded speech, use the local streaming player in
`smudgy:media`. It manages its own session output and cleanup:

```ts
import { Audio } from "smudgy:media";

const audio = new Audio(new URL("./cue.wav", import.meta.url));
audio.volume = 0.3;
audio.onerror = () => console.error(audio.error);
audio.onended = () => console.log("Finished");
await audio.play(); // Resolves when playback starts, not when it finishes.
```

`new Audio()` creates an empty player; assign `audio.src` later. Construction
with a source, source assignment, and `load()` preload a bounded PCM queue
without autoplay. `pause()` preserves the decoder position; `play()` resumes,
or reopens the file from the beginning after natural completion. `volume`
ranges from 0 to 1; `muted` preserves the chosen volume. Each player has its own
logical context, so pausing it does not pause other audio.

Source replacement, `load()`, `pause()`, and `close()` reject interrupted
`play()` promises with `AbortError`. Load and decoder failures set `error`,
dispatch `error`, and reject pending play promises; failures do not dispatch
`ended`. Set `src` or call `load()` to retry after an error. Natural completion
releases the context and joins its decoder before dispatching `ended`.
`await audio.close()` releases a preloaded or paused player and waits for its
decoder to exit. It retains `src`, volume and mute; subsequent `play()` starts
from the beginning. Setting `src = ""` also initiates resource release.

This module exports a documented subset of HTML Audio behavior, not an
`HTMLAudioElement`. It supports `loadstart`, `canplay`, `play`, `playing`,
`pause`, `ended`, `error`, and `volumechange` events and their `on…` handlers.
Events are delivered asynchronously; browser media-task ordering is not promised.
There is no global `Audio`, DOM element, `MediaError`, readiness enum, duration,
seek/currentTime API, looping, autoplay attribute, or network streaming. A
preloaded or paused player retains a context and its bounded resources until
closed, replaced, completed, or its isolate is retired. Keep such players for
reuse, or close them explicitly rather than depending on garbage collection.

For explicit Web Audio graph routing, the module also offers a lower-level
factory. It adds no methods to `AudioContext` and no Web Audio globals:

```ts
import { createFileSource } from "smudgy:media";

const context = new AudioContext();
const source = await createFileSource(context, new URL("./cue.wav", import.meta.url));
source.connect(context.destination);
source.onended = () => context.close();
source.start();
```

The module also exports `AudioFileSourceNode` for type annotations and
`instanceof` checks. Create sources with the factory, connect them to a gain or
destination, and call `start()` once. `stop()` is immediate and permanent.
[The file playback example](examples/web_audio_file.ts) includes gain and cleanup.
The API accepts local paths and `file:` URLs. Relative paths use the process
working directory; resolve module-relative assets with `new URL` as above.
Sandboxed packages need their existing file-read grant for the resolved asset.
Code-import authority does not grant asset access. HTTP/data URLs, network shares,
devices and pipes are not file sources.

Supported formats are WAV, MP3, FLAC, Ogg/Vorbis, AAC-LC or ALAC in M4A, and AIFF,
with mono/stereo audio at 8–192 kHz. The decoder reads incrementally on a worker;
neither the complete encoded file nor the complete decoded sound is loaded into
memory. Scheduling, seeking, looping and playback-rate changes are not supported
by file sources in this release.

Load failures reject the factory promise. A later decoder failure sets
`source.error` and dispatches `error` before `ended`. `stop()` cancels and silences
the source; `context.close()` additionally waits for admitted decoder workers to
exit, including sources never started. Session reload and isolate teardown cancel
their workers through the existing audio lifecycle.

The initial policy permits encoded files up to 256 MiB and eight active decoder
workers per isolate and application. Each worker reserves a 4 MiB working
allowance plus a 16 KiB PCM queue from the shared 64 MiB audio budget. Worker
charges remain until the thread is joined; graph/queue charges follow their
physical lifetime. Codec-private and container-index allocations are not measured
by that allowance, so it is not a hard process-memory ceiling. Packet, channel,
rate and metadata-entry limits provide additional bounds.

See [the trusted module example](examples/web_audio_earcon.ts) and [the complete
sandboxed package example](examples/web_audio_a11y_package/). The package asks
only for alias and echo capabilities; it has no audio grant.

## Sandbox boundary

Web Audio is a bounded baseline API inside an audio-scoped session, including
inside sandboxed accessibility packages. It does not add filesystem, network,
module-loading, subprocess, FFI, system-information, or audio-capture authority.
Audio sources remain subject to the package's existing source-access rules.
Graphs and mixer inputs are private to their context, isolate, and session.

The Flatpak package statically grants `--socket=pulseaudio`. That is a broad
application-level sandbox opening because the PulseAudio protocol can expose
both playback and capture capabilities to native code. Smudgy's CPAL integration
opens output only, and Web Audio provides no capture API or operation. Sandboxed
packages receive no new module-loading, filesystem, network, subprocess, FFI,
system-information, or capture authority; all existing checks remain in force.
The trusted Main isolate is already allow-all, including FFI. Native code it is
already authorized to load can inherit the Flatpak's new audio-server
reachability, including protocol-level capture capability. That residual is why
the static socket grant is broader than the Web Audio output API.

## Manual physical checks

### Current evidence

On 2026-08-20 `./bin/release.ps1 -BuildOnly` completed the unsigned
`release-full` physical-feature build. A validation-only unsigned Inno compile
then assembled the installer, and its compiler log confirmed that the app,
inspector, runtime DLLs, and `THIRD-PARTY-NOTICES.md` were included. That
installer was neither signed nor launched, so this is build/package-assembly
evidence only.

The ignored Windows/WASAPI lifecycle smoke also passed with:

```powershell
cargo test -p smudgy_audio --features physical-output --lib `
  system::tests::manual_default_device_silent_open_suspend_resume_close_and_repeat `
  --locked -- --ignored --exact --nocapture
```

The test opened the default device, exercised suspend/resume and logical input
shutdown, and reported `shutdown.clean == true` with `failure == None`. An
immediate second service open/shutdown also reported clean/none.

Windows live device removal, audible playback, and default-device change remain
pending. Physical runtime exercises on Linux and macOS also remain pending.

CI is configured to compile the physical feature and run injected-output tests
on Windows, Linux, and macOS, but it never requires a default audio device.
Configuration and compile results are not runtime evidence. The current support
table is intentionally evidence-bounded:

| Platform | Backend | Current runtime evidence | Still pending |
| --- | --- | --- | --- |
| Windows | WASAPI | Default-device stream open, suspend/resume, clean close, and immediate reopen; injected runtime invalidation, missing-device retry, callback-stall rebuild, and terminal-failure classification | Packaged-app launch, audible playback and continuity, ordered application shutdown, manual no-device boot with time-advancing default and `"none"` contexts, truthful not-applied controls, live removal/recovery, and default-route recovery |
| Linux Flatpak | ALSA via PulseAudio/PipeWire | Injected runtime recovery and terminal-failure classification only; release manifest and CI compilation are not physical-backend runtime evidence | Packaged-app runtime, audible playback and continuity, ordered application shutdown, manual no-device boot with time-advancing default and `"none"` contexts, truthful not-applied controls, close/reopen, live removal/recovery, and default-route recovery |
| macOS | CoreAudio | Injected runtime recovery and terminal-failure classification only; release script and CI compilation are not physical-backend runtime evidence | Packaged-app runtime, audible playback and continuity, ordered application shutdown, manual no-device boot with time-advancing default and `"none"` contexts, truthful not-applied controls, close/reopen, live removal/recovery, and default-route recovery |

Deterministic injected tests separately cover unavailable startup, a
time-advancing emulated no-argument default context, `sinkId: "none"`, truthful
not-applied controls, recoverable runtime invalidation, temporary absence of a
compatible default device with silent mixer advancement and later reopen,
callback-stall rebuild, preservation of installed inputs, and terminalization
for permanent errors or unproven stream teardown. These are control-flow and
ownership proofs against injected hosts. They do not establish that a real
platform backend emits a particular event for removal or route change, nor do
they replace any packaged-app/manual audible-continuity gate.

For removal, keep at least two default-output contexts and one `sinkId: "none"`
context alive, with a stateful source or scheduled event that can demonstrate
logical-time progress. Remove or disable the active output device and record
the exact CPAL/backend classification. For a recoverable event, confirm that
the existing contexts and sessions remain registered, new sessions and
volume/mute controls remain usable, `sinkId: "none"` continues independently,
and the physical output stays silent while no compatible default exists.
Confirm the stateful source or scheduled event continues to advance through
that silent interval. Restore a compatible device or select a compatible new
default, then confirm automatic retry resumes audible output from the retained
contexts without restarting and without resetting their saved or live control
state. Finally close the sessions and confirm logical endpoint,
callback/source, mixer-session, and physical-driver cleanup separately. If the
backend instead reports a permanent/protocol error, or stream teardown cannot
be proven, confirm that output terminalizes visibly and remains restart-only;
record that path separately from recoverable device loss.

For a default-device change, record whether the operating system keeps the
existing stream alive, emits an advisory route-change notification, or
invalidates the stream. Smudgy does not proactively replace a still-usable
stream on an advisory notification. If the old stream is invalidated or its
device is removed, confirm that the in-process recovery loop opens the
then-current compatible default and retained contexts become audible there
without a process restart. Also exercise a period with no compatible default
and confirm silent advancement plus backoff before adding or selecting one.
Until these checks are recorded on packaged Windows, Linux, and macOS builds,
the injected tests are not evidence of backend-specific live-route behavior or
audible continuity.

Smudgy's shutdown proof covers its own joined physical-driver loop and exact
logical callback/source retirement. CPAL and an operating-system backend may
use internal helper threads whose joins are not exposed. Dropping their RAII
objects is therefore not documented as proof that every opaque helper joined.
