# Smudgy Web proof of concept

This crate is the first browser vertical slice. It deliberately does not link
`smudgy_core` or Deno. It proves the proposed role-based architecture against a
live MUD:

```text
smudgy_protocol       smudgy_session_model
       |                       |
 smudgy_engine ------- smudgy_ui_shared
       |                       |
       +-------- smudgy_web ---+
```

- `smudgy_engine` owns event/effect ordering, Telnet policy, terminal state,
  ANSI styling, the compile-time automation capability, and plaintext
  alias/trigger implementation. It owns no socket or browser API.
- `smudgy_session_model` contains the portable styled-line, system-row, input,
  and line-operation types used by both native core and shared UI. The native
  paths remain compatibility re-exports, so this does not duplicate models.
- `smudgy_ui_shared` owns target-neutral themed iced presentation. It contains
  Smudgy's production pane-group model, keyed tab host,
  `TerminalBuffer`, split terminal pane, `SessionInput`, CRT cat, and other
  target-neutral widgets used by desktop. The Connect frame, navigation view
  model, typed intents, and server rail are shared. Native and browser adapt
  those intents to their own record and credential stores; native still has
  richer server records and uses an OS credential
  service where the browser uses app-origin IndexedDB. Both use the portable
  profile-activation policy. Browser last-session restore is scoped to one
  window; native retains its per-server workspace template.
- `smudgy_web` owns the browser's single-window shell, catalog/manager reducer,
  IndexedDB adapter, and bundled-font composition. Each connected session
  gets a dedicated Web Worker on a separate script origin. The worker owns its
  Rust `ClientEngine`, plaintext automation, synchronous JavaScript handlers,
  Telnet/ANSI state, and physical WebSocket. A small cross-origin frame creates
  workers and relays transferable terminal frames. The window-side WASM
  owns iced and applies ordered, incremental presentation messages from those
  workers to one independent terminal/input state per pane-grid cluster.

## Run

```text
rustup target add wasm32-unknown-unknown
cd web
npm ci
trunk build index.html --release
npm run serve:isolated
```

Open `http://127.0.0.1:8096/`, click Connect, and add a WSS server such as
Last Outpost's public `wss://last-outpost.com/ws/telnet/` service. The window
and runtime use separate loopback origins (ports 8096 and 8097). A production
build points at `https://runtime.smudgy.org/runtime-frame.html`; the local
server substitutes its own origin. Deploy only `runtime-frame.html`,
`runtime-broker.js`, `session-worker.js`, and the two `smudgy-web-worker`
assets on the script origin. Its CSP should permit only
`https://web.smudgy.org` as a frame ancestor; the web origin must allow the
script origin as a frame source. Keep web auth cookies host-only and require
exact-origin/CSRF checks on authenticated APIs. WSS gateways may inspect the
script origin in their handshake.

The Connect modal uses the shared desktop/browser dialog frame, server rail,
and profile rows. It saves WSS servers and profiles. The full URL (including
path/query), send-on-connect text, ordered regex aliases/triggers, and
JavaScript source persist in IndexedDB. The catalog has a versioned record
envelope (schema 3); old unwrapped and schema-2 records migrate on their next
successful save. Legacy profile package lists become explicit selected-profile
activation scopes on load. A
single readwrite transaction compares catalog generations before each write;
stale tabs reload rather than overwrite newer edits. Browser profile passwords
are kept in a separate app-origin IndexedDB object store, never in the catalog
or package origin. A credential edit and its profile record commit together;
deleting a profile or server deletes its credentials in that transaction.
`$PASSWORD` in send-on-connect text is resolved just before connection and on
every reconnect. Unlike native's OS keyring, IndexedDB alone does not encrypt
these passwords against someone with access to this browser profile. Server/
profile deletion requires a second confirmation.

An ordinary, editable `Default` profile is created on first quick-connect in
both hosts. Its aliases, triggers, scripts, packages, and login commands are
specific to Default, not inherited by named profiles. The browser catalog
also carries server-wide automation and package activation. `All profiles`
includes profiles created later; an explicit selection of every *current*
profile does not. A package can therefore be Default-only or All-profiles.
Profile aliases and input scripts get first refusal over server-wide rules;
server-wide output rules run first.

Profiles may also activate local packages. From the profile editor, import one
directory containing `smudgy.package.json` and TypeScript/JavaScript source.
The manifest must explicitly target `"web"` or `"both"`; absent targets remain
native-only. A separate worker bundles the local module graph with esbuild-wasm
and saves the result in the web origin's IndexedDB. The window loads only the
selected compiled packages and sends them to the isolated worker before
opening the socket, keeping compilation out of the receive path. The current
browser package API is a deliberately limited synchronous subset of
`smudgy:core`: `send`, `echo`, `createAlias`, and `createTrigger`. Unsupported
imports, non-relative dependencies, native permissions, required packages,
parameters, and unverified minimum-version floors fail import.
There is no published/cloud package installation yet. Local packages cannot
read the web origin's stored profiles or secrets, but retain the script
origin's network authority and access to their own session. Import only
source you trust with that session.
Re-importing identical contents is harmless; changing an installed package
with the same name is refused until an explicit update flow exists, so an
import cannot silently change other profiles that reference it.
The profile editor lets each active package apply only to that profile or to
all current and future profiles.

The browser saves one last nonempty window arrangement in app-origin IndexedDB:
server/profile names, explicit connection intent, and the pane-cluster tree.
Connect offers an explicit Restore action, including after the tab closes;
restore fills the current browser window rather than opening another one. The
current catalog is re-resolved before opening a worker, so deleted records are
skipped; no credential, script source, or terminal scrollback is copied into
the snapshot. This is one origin-wide latest-window record, not account sync
or a history of tabs. If multiple tabs are open, the last committed snapshot
wins. Connect refreshes that snapshot when opened in an empty window, and
Restore reads it again so an already-open tab does not apply an older copy.
Older tab-scoped snapshots migrate on first load.
Named layouts are separate one-window templates. Applying one rearranges this
window in place; if it omits a live session, the dialog asks whether to keep
or close that session before making any change.

Settings shares native terminal-display controls for font size, SGR
bold/bright behavior, blinking text, line wrapping, link tooltip delay,
extended-color adjustment, scrollback length, the toolbar-dependent
pane-header policy, and the same built-in terminal/app color schemes;
it also shares input behavior, Telnet ECHO password masking, history matching,
history size, command separator, and raw-line prefix. The browser
presents them in the single-window modal frame and stores one versioned
settings snapshot in app-origin IndexedDB; schema-1 appearance-only,
schema-2 appearance/input, and schema-3 command-syntax snapshots retain their
values and gain the native default theme and reconnect-on-send-error policy in
schema 4. Native continues to
propagate committed changes through its existing settings event. Other native
Settings features (font catalog, per-theme color tweaks, logging, account, and
security) have not yet moved into shared UI. A scrollback change is sent once
to each live session worker and trims both transcript buffers. Other browser
appearance/input/theme display changes stay in the window; command-syntax edits are
sent once to each live session worker and apply to the next outgoing command.
A large browser history limit reserves at most the default 100,000
rows initially; it grows as output arrives. Native buffer construction keeps
its existing capacity policy.

Profile scripts are user-authored JavaScript loaded as Blob modules before that
profile's session worker starts its connection. Optional
`function onLine(line, api)` and `function onInput(text, api)` handlers run
synchronously in network/input event order. `api.send(command)` enters the
native-style separator and alias pipeline; a self-referential alias stops at
the native depth limit. `api.print(line)` adds a local line. `onInput` returns `true` to
consume the original input. Like native's submission boundary, `onInput`
sees an unmasked typed line before aliases; profile startup sends without a
password enter aliases without calling `onInput`. A startup command with a
substituted password, and a masked input submission, bypass handlers and
aliases and go verbatim to the transport. Successfully sent commands echo into
the terminal after the socket accepts them, joining an open prompt as on
native. Secret substrings are replaced with a fixed-width mask in that echo.
Startup text retains blank commands and trailing newlines, matching native's
outgoing-command pipeline.
Profile scripts can also create terminal-only panes from
`api.session.mainPane` with `split(direction, spec)` and `addTab(spec)`.
Returned pane handles support `echo`, `clear`, `close`, `hide`, `show`, and
`isHidden`. Pane widgets, pane-owned inputs, package pane namespaces, and
cross-window pane operations remain native-only. Script panes are recreated
when the profile script starts; their placement is not captured in browser
window snapshots yet.
This is callback routing, not a security
boundary against a hostile script running in the same worker. Saved regex rules
use Rust capture replacements (`$1`, `$name`). The distinct browser origin
protects the web catalog and future secrets, not data already given to a
session. A blocking/infinite handler can hang its session worker; the other
sessions and window remain responsive. Do not paste untrusted scripts into a
profile. MUD output is passed as data and never evaluated as source. A handler
that returns a Promise reports an error, and `api.send`/`api.print` calls made
after the handler returns are ignored to protect event ordering.
The module load is asynchronous, but event handlers remain synchronous. A
production script-origin CSP needs to allow worker creation from that origin,
`blob:` module imports in the worker's `script-src`, and WSS to selected MUDs;
`unsafe-eval` is not
required for profile scripts. Static `import` declarations inside a profile
script are not supported by this function-body format.

Append `#scrollback-test` to the local URL to fill the shared terminal buffer
with 100,000 styled rows. This is a repeatable visual check of the actual
Smudgy split-terminal widget, including its historical/live-tail behavior,
large bounded scrollback, selection machinery, and bundled Geist Mono faces.

The `web` package builds two WASM binaries. `smudgy-web` starts iced in the
Window; `smudgy-web-worker` contains the session runtime and exposes the
message entry point used by `session-worker.js`. Trunk's worker assets have
stable names, so the worker no longer scrapes the generated page HTML. This
avoids loading iced into every session worker and avoids a JavaScript
reimplementation of the engine. Each engine drives the same
`smudgy_protocol::telnet::TelnetParser` and responder state as the desktop
connection. That provides ECHO, SGA, TTYPE/MTTS, NEW-ENVIRON, CHARSET, NAWS,
GMCP, and MSDP negotiation without a second protocol stack. The browser accepts
MCCP2 and uses the shared bounded inflater, but declines MCCP4. Configured
character encodings and CHARSET negotiation use the shared transcoder. Terminal bytes pass through a
persistent worker-owned VT parser into a bounded 100,000-row deque. Workers
send only new committed rows plus the mutable prompt. Those deltas borrow the
worker's retained rows while a compact, versioned binary frame is encoded, then
cross to the window as one transferable `ArrayBuffer`; the hot path does not
clone terminal rows or construct per-span JavaScript objects. Each worker
coalesces updates over an 8 ms presentation interval and permits one frame in
flight until the window applies and acknowledges it. Failed posts do not
advance the terminal cursor. The window decodes directly into the shared
`StyledLine` model and the same presentation buffers/widgets as desktop.
Telnet GA/EOR markers end the server's logical prompt source for synchronous
plaintext triggers while leaving that prompt visually open, as native does.
Subsequent server fragments do not replay the old prompt in scrollback. The
shared terminal widget reports its actual character grid for NAWS.

Opening Connect repeatedly creates additional independent workers and places
their session clusters side by side in Smudgy's pane grid. A slow session's
network, Telnet, or automation work therefore cannot block another session's
runtime, and the window thread never executes that work. Browser JavaScript
callbacks remain synchronous within their owning session worker.
One-window layouts may group main-session tabs: the selected tab's terminal
and input render through the same keyed tab host as native, and selection is
saved with the workspace. Profile scripts may create terminal-only split or
tab panes and control their visibility through the same shared pane registry
and layout model. Browser restore still persists only main-session panes, so
profile startup is responsible for recreating script panes.
The browser and native window now render the same tab-strip widget and feed
its gestures through the same drop classifier and single-window workspace
mutations. Dragging a tab can reorder or merge strips, center-swap rendered
tabs, split a tab beside a pane, or place it at a whole-grid edge. Native keeps
the platform-specific coordinator for cross-window tear-out and drops.

Disconnect keeps the transcript and worker alive for reconnect, but closes the
socket. Close terminates the worker and removes the session pane and its
scrollback; closing the last pane restores the main-window empty state. Divider
drags write through to the shared pane-group model, so a later connection does
not discard them.

## Automated browser tests

The Playwright suite builds the release page and worker WASM binaries, serves
the window and runtime from separate loopback origins with strict CSPs, and
connects them to a real local WSS fixture with an
in-memory test certificate. It does not need a public MUD or a proxy. The
worker tests exercise scripting and plaintext automation order, CSP/module
startup, session isolation, reconnect, Telnet negotiation, presentation
backpressure, and bounded scrollback. Fixed-viewport window tests click the
actual iced canvas and verify the shared Connect modal, theme, IndexedDB
persistence, session inputs, pane isolation, worker lifecycle, script-origin
WSS handshake, and lack of access to the window's IndexedDB.

```text
cd web
npm ci
npx playwright install chromium
npm run test:e2e
```

`npm run test:e2e:run` repeats the tests against an existing `web/e2e-dist`
build, leaving any developer-served `web/dist` untouched.
On a workstation with Chrome already installed, set `PLAYWRIGHT_CHANNEL=chrome`
to avoid downloading Playwright's Chromium. Failure screenshots and traces are
written to `web/test-results/`. The test fixture binds only to loopback ports
8094 (window HTTP), 8095 (runtime HTTP), and 9443 (WSS); no certificate or
profile data is checked in.

The browser target is checked in CI. For a live smoke test, use a trusted WSS
endpoint such as Last Outpost, confirm that its ANSI welcome screen and prompt
appear, then Close the session. The `#scrollback-test` diagnostic exercises the
100,000-row pane and the same close/empty-state path without network access.

## Cross-host behavior contract

`test_fixtures/connection_profile_send.json` is consumed by the native socket
integration test and the browser-worker WSS test. Both hosts must defer profile
text until displayable network output arrives, ignore an empty terminal line,
and send matching trigger effects before the deferred profile command. This
fixture is a narrow behavioral contract, not a claim that the two session
runtimes have full feature parity.

| Area | Native | Browser slice |
| --- | --- | --- |
| Transport | TCP, TLS, or WSS | WSS (binary Telnet frames) |
| Telnet parser and responders | `smudgy_protocol` | Same parser and responders, with browser policy |
| Compression | Configurable MCCP2 and MCCP4 | MCCP2; MCCP4 declined |
| Charset | Configured/negotiated encodings | Configured encoding and CHARSET negotiation |
| Scripts | Deno runtime and packages | Synchronous per-worker handlers, plaintext rules, terminal-only profile-script panes, and installed local TS/JS packages using a limited Smudgy API |
| Profile storage | Versioned native records and OS credentials | Versioned IndexedDB catalog, separate app-origin passwords, one-window restore |

New shared behavior should get a cross-host fixture or equivalent paired tests
before more runtime code is extracted. Intentional gaps should stay explicit.

The browser and its portable engine are workspace members but not default
members. Desktop consumes the shared session presentation types and widgets,
but normal desktop builds do not compile or link the browser client, portable
engine, or browser APIs.
