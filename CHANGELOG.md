# Changelog

All notable changes to smudgy are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.1] - 2026-07-14

### Changed

- **Massive server output ingests in a fraction of a second.** Server lines
  now reach the display in coalesced batches instead of one display update
  per line, so replaying a big log or catching up after a long disconnect no
  longer paints line by line while the client falls behind. A 16MB,
  150,000-line dump went from ~15 seconds of visible scrolling to under a
  second, every line intact.
- **Less work per server line.** The raw wire form of each line (what
  ANSI-aware `rawPatterns` triggers match against) is only captured while such
  a trigger actually exists, and display styling is now baked per line only
  when a line first becomes visible — output that scrolls straight through
  scrollback during a flood skips that work entirely. Nothing changes
  functionally; profiles without raw-pattern triggers just stop paying for
  them on every line.
- **Styled `echo` is ~28x faster on heavily-styled lines.** Styled fragments
  now cross the scripting boundary packed (one string + one record table)
  instead of as a per-run object graph. A line with 90 color changes went
  from ~106µs to ~3.8µs per echo, and from ~914 to ~10 allocations; scripts
  don't change — `echo`, `style`, and `link` behave exactly as before.
- **Echo storms no longer flood the display.** Script echoes now reach the
  terminal in coalesced batches (as server output does) instead of
  one display event per `echo()` call, so a script echoing tens of
  thousands of lines renders in a handful of updates and the UI stays
  responsive while it happens.

### Added

- **Cloud atlases now scope to your servers.** Each atlas shows only on the
  server entries it's associated with: a session's map tree lists that
  server's maps plus a collapsed Unassigned group, and room identification
  ignores maps that belong to your other games — look-alike stock zones from
  another game can no longer capture your location. Existing atlases start
  unassigned and home themselves as you play (sustained locates or a
  speedwalk associate the atlas, with an undoable notice); anything you
  create is scoped to the server you created it on. The map editor gains a
  This server / All atlases view and a per-atlas "Servers…" checklist for
  adjusting scope directly.
- **Shared maps arrive organized and homed.** Areas shared to you now carry
  their owner's folder name, so a share appears as named folders instead of
  a flat pile. When creating a share you can disclose which game hosts the
  maps belong to (pre-checked, removable, per share); the recipient's client
  files the share under their matching server automatically, and with no
  match it simply lands in Unassigned. And if rooms you're walking match a
  map filed under another server, smudgy offers to show it here too rather
  than re-mapping them.
- **MSDP support.** For MUDs that publish structured data over MSDP rather
  than GMCP: negotiation is automatic, and every variable the server reports
  lands in the same live state tree scripts already use for GMCP — read it
  from `smudgy:state/msdp`, with `msdp:ready` and `msdp:closed` events
  marking availability. Reported variables also appear in the Store tab of
  the automations window, so you can see exactly what your MUD publishes.
- **Session maps and server room identities.** Scripts can create session
  maps — `mapper.createArea(name, { ephemeral: true })` — that live only for
  the current session and are never synced: the place to build maps
  automatically from server data. Rooms can now also carry the server's own
  room identifier (`externalId`, as reported over GMCP or MSDP), and
  `mapper.findRoomByExternalId` turns one into a room — reliable
  you-are-here resolution on MUDs that announce room ids. A session map
  worth keeping can be exported with `mapper.exportArea`.
- **Clickable server links (OSC 8 hyperlinks).** MUDs that send OSC 8
  hyperlinks now render as clickable links in the terminal. `http`/`https`
  links open in your browser; a MUD-specific `send:` link sends a command as
  you. Because these come from the server, the first click to a given site
  (or the first command link) opens a confirmation showing the exact
  destination — nothing the server sends can disguise it — where you can also
  choose to always allow that site or always trust every link from that
  server. Other URI schemes are ignored.

### Fixed

- **Map preferences stop retrying maps that can't sync.** A map disabled
  locally that the cloud can't store a preference for (a local-only map, or
  one you no longer have access to) was re-pushed on every 90-second sync
  cycle for as long as the app ran. The preference still works and stays on
  your machine; the client now stops asking after the first refusal, and
  tries again when you toggle the map or sign in.
- **ANSI background colors display.** Server text styled with SGR background
  codes (`ESC[41m`, `ESC[48;5;n`, `ESC[48;2;r;g;b`, and the bright
  `100`–`107` range) now shows its backgrounds; they were previously ignored
  at both ends — the codes were dropped during ingest, and the terminal
  never painted span backgrounds (which also kept link chips and underlines
  from rendering).
- **One unsupported ANSI code no longer discards a whole style sequence.**
  Codes smudgy doesn't render (underline, italic, blink, …) used to poison
  every color that shared their sequence — `ESC[1;4;31m` displayed as plain
  text instead of bright red. Each code now applies independently, the
  bare-reset form `ESC[m` resets, colon-form extended colors
  (`38:2::r:g:b`) parse, and out-of-range color components clamp instead of
  wrapping around.
- **Progress bars that redraw with a bare carriage return display as one
  updating line.** Text following a `\r` now overwrites the line it returned
  to — previously every frame concatenated into one endlessly growing line.
  The session log keeps the final frame.
- **A malformed telnet subnegotiation can no longer exhaust memory.** A
  server that opens a subnegotiation and never closes it had its payload
  buffered without bound; the buffer is now capped (256 KiB), with the
  oversized subnegotiation discarded and the stream resynchronized at its
  end.
- **Echoed text is scrubbed of control characters.** Stray ESC bytes, `\r`
  tails from split CRLF text, and other control characters in `echo`ed or
  spliced text (tabs excepted) are stripped instead of landing in the
  display buffer.

## [0.4.0] - 2026-07-12

### Added

- **GMCP support.** smudgy now speaks GMCP, the protocol most modern MUDs use
  to send structured data (vitals, room info, inventories, chat) alongside
  the text. Negotiation and the opening handshake are automatic — the
  widely-implemented baseline modules (`Char`, `Char.Skills`, `Char.Items`,
  `Room`) are requested on connect — and everything the server sends lands
  in a live tree scripts read like any shared state, from `smudgy:state/gmcp`:
  `gmcp.value.Char.Vitals.hp` is the latest reading,
  `gmcp.watch("Char.Vitals.hp", ...)` runs on each vitals message, and
  `gmcp.bind(path)` wires a widget. Message names match case-insensitively,
  the common messages (`Char.Vitals`, `Char.Status`, `Room.Info`,
  `Comm.Channel`, …) arrive fully typed, and a script can type its own game's
  messages by extending `GmcpTree`. Delta-shaped messages fold into the state
  they describe — `Char.Items.Add`/`Remove`/`Update` maintain the retained
  item lists, `Room.AddPlayer`/`RemovePlayer` maintain `Room.Players` — and
  partial updates merge instead of replacing (`Char.Status` by default;
  `gmcp.mergeKeys(...)` adds more). Scripts talk back with
  `gmcp.send("Char.Skills.Get", { group: "combat" })` and turn optional
  modules on with `gmcp.enableModule("IRE.Rift")` — module use is shared
  across scripts, and a module enabled before connecting joins the handshake.
  `gmcp.onReady(...)` runs code once GMCP is up whether the script loaded
  before or after the connection, `ready`/`closed` events in
  `smudgy:events/gmcp` mark the transitions, and the Store tab shows the
  whole GMCP tree live, next to everything else shared in the session. For a
  sandboxed package, sending GMCP to the game is a permission of its own,
  consented at install like the rest.
- **Styled echoes.** `echo` now takes styled text built with the new `style`
  tagged template: `` echo`A ${style.red`red`} word` `` — or
  `` echo(style.blue.bgYellow`loud`) ``, `` style.fg({ r: 255, g: 128, b: 0 })`exact` ``,
  `` style({ fg: "cyan", bg: "black" })`both` ``. One `echo` for everything:
  it accepts a plain string, a styled fragment, or direct use as a template
  tag, and the same styled text works with a session's and a pane's `echo`.
  Fragments nest with sensible inheritance — an inner fragment keeps its own
  colors and picks up the rest from the fragment around it — and anything a
  fragment leaves unstyled looks exactly like a plain echo. ANSI names, theme
  roles (`default`/`echo`/`output`/`warn`), exact RGB, and dim variants all
  use the same `Color` forms the line-styling APIs already take.
- **Clickable links in session output.** The new `link` tag makes any run of
  echoed text clickable: `` echo`Exits: ${link("north")`north`}` `` sends the
  command when clicked (as if typed), and `link(fn)` runs a script handler
  with the click's modifier keys instead. Links render underlined over a
  faint wash of the text's own color — the same affordance as Markdown-widget
  links, whatever colors the text uses — with a pointer cursor on hover, and
  dragging a selection across a link never triggers it. Command links keep
  working for as long as the line is on screen; handler links work while the
  script that made them stays loaded, with only the most recent kept.
- **Styled text in line edits.** A trigger can splice styled, linked text
  into incoming lines: `line.insert`, `line.replaceAt`, and `line.replace`
  all accept styled fragments —
  `` line.replace("north", link("north")`${style.cyan`north`}`) `` turns an
  exit name into a clickable command chip. Unstyled parts of the fragment
  blend into the surrounding line (or take `insert`'s color options), and the
  line's existing colors and links stay intact around the edit.
- **Shared state & typed events between scripts and packages.** A package (or
  your own scripts) can publish live values and broadcast happenings through
  named handles: `export const vitals = createState<VitalData>()` /
  `export const prompt = createEvent<PromptData>()` from `smudgy:core` — the
  exported const names the handle — then publish with `.set()` / `.emit()`.
  Anyone else consumes them by importing from `smudgy:state/<owner>/<package>`
  and `smudgy:events/<owner>/<package>` — reading another package's state or
  subscribing to its events never runs its code, and only the package itself
  can publish under its name (importing a package's *code* yields a copy with
  the publishing handles removed, and a notice names the right import). The
  built-in happenings (connect, disconnect, outgoing commands, incoming
  lines, map movement) are consumed the same way, from `smudgy:events/sys`
  and `smudgy:events/map`, with full payload types. Consumer imports are
  typed from the producer's own source, and each handle's name doubles as its
  payload type — `function onPrompt(p: prompt)` just works — so renaming a
  field in the producer immediately re-types every consumer in the editor.
  (The old string-based `on`/`once`/`emit` functions and the `SmudgyEventMap`
  augmentation pattern are gone — they had no published users.)
- **Per-write state subscriptions, assignment-style publishing, derived
  values, and procedures.** Consumers of shared state can now hear *every
  write* with `.onWrite(...)` — occurrence-shaped data (a chat line, a
  command response) arrives once per write, where `.watch(...)` folds a burst
  into its final value — and both can watch just one entry
  (`vitals.watch('hp', ...)`). In either handler, `.previousValue` holds the
  value from before the update began, so working out what changed is a
  comparison away. Producers can publish with plain assignment
  (`vitals.value.hp = 42` publishes exactly that entry), and
  `export const hpPct = createDerived(vitals, v => v.hp / v.maxhp)` publishes
  a value computed from state you don't own — bindable to widgets like any
  state of your own. Packages can also declare a procedure — a directed ask
  that only they answer: `export const refresh = createProcedure((args,
  sender) => { ... })`, where callers import from
  `smudgy:procedures/<owner>/<package>` and `.post(...)`, and the
  implementation sees who asked, guaranteed by smudgy rather than claimed by
  the sender. An ask posted moments before its package finishes loading (or
  during a script reload) is held briefly and delivered, not lost. And
  `await prompt.once()` waits for an event's next occurrence.
- **A live Store view in the Automations window.** The new Store tab shows
  everything shared in the current session: each publisher's live state tree
  (collapsible, with its storage footprint), and every state key, event, and
  procedure seen — who declared it, its payload type as declared and as
  actually observed, and the most recent payloads with timestamps and
  senders. Browse what a package shares before writing a consumer, or watch
  your own script's published state update as you play.
- **Event handlers can act on an incoming line the way a trigger does.** The
  `receive` event in `smudgy:events/sys` fires for each complete line from
  the game, after triggers run and before the line displays. The payload
  carries the text as originally received, and inside the handler the
  ambient `line` refers to that same line — `line.gag()`,
  `line.redirect()`, and `line.replace()` work exactly as they do in a
  trigger, so a package can filter or reroute output without owning a
  trigger pattern.
- **`extractMarkdownLinks()` in `smudgy:widgets`.** Scripts can now read the links
  out of a Markdown document — exactly the set a `Markdown` widget renders,
  bare `<command>` links included, with backslash escapes honored and
  inline/fenced code left literal — instead of approximating them with a
  pattern. Each link arrives as `{ label, url }` in document order, so "run
  the first link in this room's notes" is a one-liner.
- **Flexible session panes.** Session output now lives in a real pane grid:
  drag panes to rearrange (their headers are the drag handles), resize with
  dividers, maximize/restore, tear a pane out into its own window, and drag
  panes between smudgy windows. Scripts can create additional output panes
  (`pane.split()`), route lines into them (`line.redirect()`/`line.copy()`),
  and mount widgets in them. Each new session divides the window evenly
  against the existing sessions, and script-created panes always build out
  their own session's area — the layout comes out the same regardless of the
  order sessions connect and scripts run.
- **Distraction-free pane headers.** Session and pane headers show only while
  a window's toolbar is expanded, restoring the pre-pane quiet display; a new
  Preferences toggle ("Hide panel headers unless the main menu is active",
  on by default) turns this off to keep headers always visible. Scripts can
  pin an individual pane's header with `split(dir, { titleBar: "always-show" })`
  — which, aimed at an existing pane (including `main`), also re-policies it.
- **Axis-checked pane sizing for scripts.** `pane.split()`'s spec now ties the
  initial size to the split axis in the TypeScript typings — `width` on
  `left`/`right` splits, `height` on `top`/`bottom` — so passing the wrong
  dimension is a compile-time error in the editor instead of a silent no-op.
- **Reloading scripts cleans up abandoned panes.** Panes still survive script
  reloads with their placement untouched when the reloaded scripts recreate
  them (the normal `split()` get-or-create idiom) — but a pane nothing
  re-claims is now closed when the reload finishes, so disabling the package
  that created a panel actually frees the screen space.
- **Linux builds ship as a Flatpak.** smudgy now packages for Linux as a
  self-distributed Flatpak bundle (`bin/release-linux.sh` →
  `dist/smudgy-<version>-x86_64.flatpak`), alongside the Windows installer and
  macOS `.dmg`. Data lives in the host's `~/Documents/smudgy` (shared with a
  non-Flatpak install), the app runs on the lean `org.freedesktop.Platform`
  runtime, and the manifest and packaging assets live in `packaging/linux/`.
- **Scripts can walk to an area by name.**
  `mapper.findNearestRoomInArea(from, area)` returns the closest reachable
  room of the given area, by the same weighted route search as
  `getPathBetweenRooms` — the start room counts if it is already in the area,
  and naming an area reaches it even when it is marked inactive. Pairs with
  `getPathBetweenRooms` for area-targeted speedwalks, complementing the
  tag-based `findNearestRoomWithTag(s)`.

### Changed

- **Reading shared state costs microseconds, not milliseconds.** The session
  store keeps published values as a persistent tree behind cheap snapshots,
  with each handle's identity resolved once at construction: a consumer
  reading a value four levels deep in a large state tree dropped from
  ~750µs to ~2.4µs, and the per-publish flush cost fell by an order of
  magnitude. The Store tab's live view is budgeted and paced (~30 updates a
  second), so keeping it open on a busy session no longer taxes the session
  itself. GMCP data rides this same store, so a chatty game's constant
  updates stay cheap.
- **Script automations no longer require a name.** `createAlias`, `createTrigger`,
  `createTimer`, and `createHotkey` drop the leading `name` argument: pass the
  pattern (or key/interval) first, and the automation names itself after it —
  which is what the name almost always was. The automations window shows the
  pattern for unnamed automations, re-creating the same pattern replaces rather
  than stacks, and `singleton` keys on the same derived identity. To tell apart
  two automations sharing a pattern, or to keep a stable label for registry
  lookups, pass `{ name: "..." }` in the options (explicit names still follow
  the automations-editor naming rules; derived ones are exempt). `createTriggers`
  is unchanged: its keys are the names, which is the point of the batch form.
  Old name-first calls keep working through the 0.4 line behind a deprecation
  shim — the positional name lands in `options.name`, identical in every
  observable way — with a `[deprecated]` notice echoed once per script and
  function. The shim is removed in 0.5 (a build-time tripwire enforces it),
  after which the old form throws a `TypeError` at creation.
- **Session logs are the union of all panes.** The plaintext session log now
  records every line shown in any of the session's panes, in completion order
  (a line redirected away from the main output still lands in the log,
  unattributed). Fully-gagged lines stay unlogged.
- **Gag no longer short-circuits line edits.** `line.gag()` now only removes
  the line from the main display; transforms (`replace`/`highlight`/…) issued
  before or after it still apply to copies routed to other panes. A script
  that relied on gag skipping later edits sees those edits take effect now.
- Internal: the `smudgy_bench` crate now covers the client's hot paths
  end to end — socket ingest through telnet and VT parsing, trigger matching
  and pattern-set rebuilds, command dispatch, the session store's op layer,
  terminal shaping, and mapper routing — with an allocation-counting
  harness, so performance regressions anywhere on the ingest-to-display
  path are measurable before release.

### Fixed

- **Credentials persist on Linux.** With no Linux keyring backend enabled,
  `keyring` silently fell back to an in-memory mock store, so the cloud session
  token and profile passwords were lost on every launch (and the obfuscated-file
  fallback never engaged, because the mock "succeeded"). Linux builds now use the
  Secret Service (GNOME Keyring / KWallet) backend, falling back to the
  obfuscated file when no secret service is running. (macOS has the same latent
  gap — it needs keyring's `apple-native` backend — and is not addressed here.)
- **The Linux window shows the app icon.** The main and tool windows now set
  their `application_id` to `org.smudgy.Smudgy` on Linux, so the running window
  associates with the desktop entry (Wayland `app_id` / X11 `WM_CLASS`) instead
  of showing a generic icon.
- **Pressing a script widget after a reload no longer crashes smudgy.** A
  widget mounted before a script reload (typically one a handler created with
  `createWidget`) kept callbacks tied to the torn-down script engine; pressing
  one of its buttons crashed the whole client. Widgets are now cleared when
  scripts reload — reloading scripts re-mount theirs as usual — and a press
  that races the reload is safely ignored instead of crashing.
- **One session's script failure no longer takes down the others.** If a
  session's script runtime dies, interacting with that session's widgets now
  logs the problem and disarms them; every other session keeps running,
  instead of the entire client aborting.
- **Clicking the terminal focuses the input again.** Clicking a session's main
  terminal (without selecting text) once more puts keyboard focus in that
  session's command input, as it did before the pane grid — without stealing
  focus from widgets layered over the terminal.
- **Self-loop exits look like loops.** An exit that leads back to its own
  room now draws as a small loop arc on the room's wall — with the exit's
  usual style, color, and secret dashing — instead of a bare stub
  indistinguishable from a dangling exit.
- **Your private cloud packages show up in the package browser.** The "my
  cloud packages" pane hid any package that also exists as a local authored
  copy, so an author browsing for a package they published as Private saw
  nothing at all. It now lists the package with a "Local" badge instead of
  an Install button.
- **`line.replace` no longer garbles a line or crashes on copy.** A script that
  replaces text in the middle of a line (rather than the whole line) no longer
  duplicates fragments on screen, and copying the edited line to the clipboard
  no longer crashes. Replacements on lines containing non-ASCII characters
  (accents, emoji) now land in the right place too.
- **`npm:` packages with dependencies now load.** Importing an npm package
  that depends on other packages — most real ones, like `npm:discord.js` —
  failed with "Cannot find module …" when the package required its
  dependencies; only dependency-free packages worked. Named imports from
  CommonJS packages (`import { Client } from "npm:discord.js"`) work now
  too; previously only the default import carried the module's exports.

## [0.3.4] - 2026-07-01

### Added

- **Packages can require a newer smudgy.** A package manifest may declare a
  minimum smudgy version, and smudgy honors it everywhere it resolves a package
  — install consent, sandboxed and trusted loads, and the offline cache —
  holding an update back with a "needs a newer smudgy" notice rather than trying
  to run code your client is too old for. The manifest editor gains a "Requires
  smudgy" field.
- **New "Stub" exit style.** A fifth exit style draws a minimal directional
  marker: a bare stub for a same-level exit, and a re-anchored level triangle
  with a fading gradient stub for a cross-level one. Normal cross-level exits
  now draw their gradient directional stub too.
- **Map scripting for shippable map packages.** Scripts can import and export
  whole areas, create and edit map labels and shapes (and read them back), and
  reach a durable per-package data directory via `getDataDir()` that survives
  the package's own version updates. Packages can also ship data as JSON modules
  (`import … with { type: "json" }`). Together these let a package seed, export,
  and reset starter maps in place.

### Fixed

- **Windows upgrades install over a running smudgy.** Installing a new version
  while smudgy is open no longer fails and rolls back; the installer closes the
  running instance and relaunches it from the Finished page.
- **The script inspector works in release builds.** With advanced scripting
  features enabled, "Inspect" now actually starts the inspector in release and
  release-candidate builds instead of doing nothing; enabling it mid-session
  shows a toast telling you to reconnect (the inspector is created at connect
  time).
- **Local package fork, delete, and reload cleanup.** "Edit a copy" now mirrors
  the source's enabled state even while signed out — an enabled original hands
  its install off to the copy, a disabled one yields a disabled, inspect-only
  copy. Deleting a local package removes the phantom "installed" entry it left
  behind, a local package enabled before you chose a nickname keeps running after
  you choose one, and reloading a session no longer reverts a just-made install,
  uninstall, or enable change.
- **Honest "not loaded" notice.** When no version of an installed package can be
  found at all (deleted, unpublished, or a removed local folder), the session now
  says so and suggests removing or reinstalling it, instead of claiming it "needs
  more permissions than you've granted".

## [0.3.3] - 2026-06-30

### Added

- **limitations on scripts' usage of npm and jsr.io** A package now declares how far
  outside the smudgy ecosystem it may download and run code, as one of three
  levels: 
  - nothing beyond smudgy packages (the default)
  - public registries (npm and jsr)
  - or anywhere on the web
- **Scripts can now read settings** `getSettings()` in `smudgy:core` lets a
  script read your settings from the preferences window, e.g., command separator,
  raw-line prefix, fonts, theme, command-input behavior,
- **Modules and trusted packages** can now create aliases, hotkeys, and triggers.
  This isn't available to packages running in a sandbox.
- A few areas in the automations window received some ui/ux polish

### Changed

- **Faster trigger matching on busy sessions.** The trigger engine no longer
  slows down as you add triggers: per-line fire-limit bookkeeping now touches
  only the triggers that actually set a limit, and per-line timing
  instrumentation is compiled out of release builds. Heavy-trigger setups keep
  up with fast-scrolling MUD output instead of falling behind.

### Fixed

- **Crash on launch from a terminal.** Fixed a startup crash on macOS and
  Linux when smudgy was launched from a terminal
- **Dependency fix in "Make a copy."** After you fork your own package with
  "Make a copy" and republish it, a package that depends on it no longer silently
  bundles the older version you forked
- Publishing a package that ships its own `.d.ts` files no longer fails with a
  duplicate-subpath error
- **Clearer package-dependency rows.** A package pulled in as a dependency now
  reads "active/inactive" rather than "enabled/disabled", which was misleading
- **Access your own published but deleted packages** The Shared pane is now "Private &
  Shared", and includes your own packages, not only ones shared with you.

## [0.3.2] - 2026-06-28

### Added

- **Shared script packages.** smudgy now has a package ecosystem. Browse and
  install community packages from the cloud (`smudgy://owner/name`), rate the
  ones you've installed, and publish your own. Packages are versioned with
  semver dependency ranges and locked, reproducible resolution; installs are
  per-server, and an auto-updating package prints a one-line session notice when
  it moves to a new version. Packages can expose configurable **params** —
  including secret values, which are kept in your OS keyring rather than on disk.
- **Sandboxed packages, with consent.** An installed third-party package runs in
  its own isolate with only the permissions its manifest declares — network,
  file, and environment access plus smudgy's own capabilities — and you approve
  that set in a consent dialog at install time. You can **trust** a package to
  grant it full access; your own scripts and packages you trust run unrestricted.
  A package can only ever read its own configured params, never another
  package's.
- **Local package authoring.** "Edit a copy" forks any package into a local,
  editable copy you can rename and open on disk; the sidebar splits Installed
  from Local, and local packages run even while you're signed out.
- **Use smudgy without an account.** Connecting, playing, and local mapping all
  work signed out, and you can install and run **public packages anonymously**.
  A cloud account is now needed only to publish, share maps, or use social
  features. The "update available" check works without signing in, too.
- **A new scripting runtime.** The embedded JS/TS engine was rebuilt on a
  Deno-based, in-tree runtime: real `jsr:` and `npm:` imports, working TLS and
  `fetch`, and a bundled **DevTools** sidecar — an "Inspect" button on the
  toolbar opens a Chrome-DevTools inspector bound to the active session.
- **Reworked scripting API.** A small `globalThis` plus a `smudgy:core` module:
  bash-style capture templates with numeric *and* named matches (a collision-safe
  matches bag), handle-based create/remove for aliases, triggers, and hotkeys
  (each with optional fire- and line-count limits), managed timers, persistent
  `vars`, and a single unified line/buffer model. Automations a script creates
  carry their origin and now appear live in the Automations window.
- **VS Code support for scripts.** smudgy generates a `tsconfig` and ships type
  declarations — for `smudgy:core`, `smudgy:params`, `smudgy:widgets` and your
  installed packages, and the Deno + Node runtime — so editing `modules/` and
  packages in VS Code gives full TypeScript IntelliSense. Publishing generates
  a package's `.d.ts` with an embedded `tsc`.
- **Secret-aware sending.** `$PASSWORD` in your auto-login text will prompt you
  for a password, which will then be backed by the OS keyring for storage, and 
  `SendWithRedactions`, which sends secret text to the MUD while masking it in 
  your terminal and logs.
- **Open a session offline, and Disconnect.** You can open a session without
  connecting — to work on its scripts or map — and a new Disconnect control drops
  the connection without closing the session.
- **Better prompt handling.** A new telnet preprocessor recognizes IAC GA/EOR
  prompt markers, so prompts are detected reliably and raw telnet control bytes
  no longer leak into the terminal.
- **Connect & onboarding pass.** Friendlier session-start output, a connect
  dialog that opens fully populated (no loading flash), a taller on-connect
  editor, and clearer copy that points you at `$PASSWORD`.
- **Configurable command input.** Command-separator and raw-prefix behavior are
  configurable, with their persistence fixed.

### Changed

- **Your nickname is now your unique handle.** The old username discriminator is
  gone — your nickname alone identifies you, which simplifies package ownership
  and sharing.
- **One sign-in flow.** Signing in and creating an account are now a single
  email-first flow.
- The application binary is now `smudgy`, shipped alongside a bundled
  `smudgy_inspector` DevTools helper.
- Faster outgoing-command handling: the alias regex set is rebuilt lazily on the
  first outgoing line instead of eagerly, and per-line script timing was dropped
  to a trace-only path.

## [0.3.1] - 2026-06-18

### Added

- **Map folders.** Organize your maps into named folders (atlases): create,
  rename, and delete folders, and move maps between them. The area list
  keeps folders you own separate from folders shared with you, and shared
  maps now show the handle of the friend who shared them.
- **Local maps, no account required.** Maps can now live entirely on your
  disk and work while you're signed out, appearing in the same list
  alongside your cloud maps. Signing in later simply adds your cloud maps
  to the local ones rather than replacing them.
- **Transfer map ownership.** Hand a map — or a whole folder — to a friend.
  You send an offer and they accept; ownership moves to them, and you keep
  admin rights they can later revoke. Only an owner can offer a transfer,
  and only the new owner can transfer it again or appoint admins. Pending
  offers appear in the social panel to accept, decline, or cancel, and an
  offer is withdrawn automatically if either side blocks or unfriends the
  other.
- **Co-owner (admin) sharing.** Share a map with a friend as an *admin* and
  they gain every owner power — edit, re-share, copy, manage secrets,
  rename, delete — except transferring ownership or naming further admins.
  Maps you administer are flagged with an "admin" badge. Folder-wide
  (atlas-scoped) shares can now include secrets, too.
- **"Update available" notice.** When a newer release has shipped, smudgy
  shows a dismissable popup linking to the download page, with "Remind me
  later" (just this session) and "Skip this version" (quiet until a newer
  release appears).
- **Graceful "out of date" handling.** If your client is too old for the
  cloud service, you now get a clear banner — a newer version is required
  for some features — with a click-to-open download link, instead of
  cryptic failures as the API moves on. Core MUD play and local mapping
  keep working; only the cloud features are gated behind the upgrade.
- **Run two copies side by side.** New `--data-dir` and `--keyring-user`
  launch flags point a second instance at a separate data directory and
  cloud login, so you can (for example) view a shared map as both the owner
  and the recipient. Both accept `--flag value` and `--flag=value`; the
  default launch is unchanged.
- **Open source licenses.** Settings has a new Licenses tab listing the
  third-party notices for the fonts, icons, and libraries smudgy bundles.

### Changed

- **Active/inactive map choices now sync across your devices.** Toggling a
  map active or inactive follows your account to your other machines
  instead of staying on the one where you set it.
- **Sessions stay signed in while you use smudgy.** The client refreshes its
  cloud session on launch and roughly once a day, so an actively-used login
  never lapses for inactivity; a session left untouched still expires after
  a year.
- Internal: every shipped crate now carries the same version, sent to the
  cloud as `X-Smudgy-Client-Version` so the server can recognize out-of-date
  clients.

## [0.3.0] - 2026-06-12

### Added

- **Cloud accounts (passwordless).** Create an account and sign in from the
  settings window with just your email — there is no password: smudgy emails
  you a short one-time code, and pasting it both verifies the address and
  signs you in (the same code-paste flow covers returning devices; "Resend
  code" mails a fresh one). Sessions persist in the OS credential store and
  re-authenticate silently, so a returning user rarely needs a new code.
  Mapper API keys are sunset as a client credential — the mapper
  authenticates with your logged-in session (the Security tab still manages
  server-side keys and sessions).
- **Map sharing.** Friends and blocks (enumeration-resistant), a share
  dialog with per-recipient capabilities (edit / re-share / copy / include
  secrets), a secret-count warning with review and an exact recipient
  preview, secret marking for rooms/exits/labels/shapes/properties (bulk
  marking and an owner audit panel included), shared atlases in the area
  list with owner attribution, "Unknown map" rendering for links into maps
  not shared with you, clone-to-modify with provenance, and a `/sync`
  poller that keeps shared maps current (revoked access purges the local
  cache, including secrets).
- **Map copies and merging.** A map you copy no longer has to compete with
  the original. Any map can be toggled active/inactive from the area list
  (and the inspector): an inactive map stays visible and editable but is
  excluded from room identification and avoided by auto-routing, so a copy
  with some secrets unmarked won't shadow your real map. Owned maps gain a
  **Duplicate** action, and the duplicate starts inactive. When you have
  several copies of the same map, the inspector shows the family and an
  "active copy" picker so exactly one is used for identification. You can
  also mass-select rooms and **copy them between maps** (with their exits
  and properties — exits inside the selection are re-linked, links to other
  maps stay live, and the rest paste unconnected), gated on the same `copy`
  permission as whole-map cloning, to merge a friend's changes into your
  own map. The "shared with me" list now groups maps by the **friend who
  shared them** (not just the original owner), and flags a re-shared map
  with who owns it. Active/inactive choices persist locally and apply to
  every session.
- **Preferences.** Terminal font (Geist Mono, five Monaspace variants, or
  any monospaced system font), font size, optional max line length,
  scrollback length (previously configured but silently ignored), command
  separator, a raw line prefix that sends a line verbatim (no splitting, no
  alias matching), and logging controls — the plaintext session log is now
  optional and an additional raw log can capture exact server bytes
  including ANSI codes. Changes apply to running sessions immediately.
- **Themes.** 27 color schemes (Rosé Pine, Catppuccin, Tokyo Night,
  Tomorrow, Modus, Nord, Monokai, Matcha, Apprentice, Gruvbox and Solarized
  in dark and light, and more), each styling the terminal palette, app
  chrome, and the input strip's deliberate contrast. Truecolor and
  256-color text is interpreted *archetypally* — interpolated between the
  theme's background and its bright primaries instead of black and pure
  RGB — so server colors stay coherent in any scheme, including light
  ones. Every theme is tweakable non-destructively: background darkness,
  text brightness, contrast (anchored on the background), saturation, and
  per-color overrides, stored per theme.
- Panics are now written to `smudgy.log` with a full backtrace before the
  process dies. Previously a release-build crash left no trace, since
  windowed builds have no visible stderr.
- Dragging a text selection past the top or bottom of the terminal now
  auto-scrolls the view toward the cursor — faster the further past the
  edge — and keeps extending the selection while the mouse is held still,
  so multi-screen selections no longer require the scroll wheel mid-drag.

### Fixed

- Fixed a crash when opening a session with the software renderer
  (`ICED_BACKEND=tiny-skia`): the terminal scrollbar computed NaN geometry
  for an empty scrollback (0/0), which the software rasterizer rejects.
  The GPU renderer silently discarded the bad quad, hiding the bug.
- The Windows installer now ships an app-local copy of the VC++ runtime
  (`vcruntime140.dll` / `vcruntime140_1.dll`), so smudgy starts on clean
  Windows 10/11 machines that don't have the VC++ Redistributable
  installed.
- Deleting a room now clears the destination of every exit that pointed at
  it instead of leaving them dangling at the gone room until the next
  sync. The cache mirrors the server's cascade across all loaded maps
  (cross-area links included), and undoing a delete re-links those inbound
  exits.

### Fixed (software renderer)

Running smudgy with `ICED_BACKEND=tiny-skia` (the automatic fallback on
machines without a usable GPU) is now actually usable. Five upstream
iced bugs are fixed in a vendored copy of `iced_tiny_skia` (see
`vendor/`), all invisible under the GPU renderer:

- The session minimap rendered its rooms outside the widget (or not at
  all) and scattered its labels across the window: canvas clip
  rectangles were translated twice, the DPI scale was composed inside
  the canvas offset instead of outside it (displacing content in
  proportion to its distance from the window origin), and canvas text
  ignored its clip bounds entirely.
- Hovering UI elements progressively darkened the window until it was
  unreadable: quad shadows were blended without a clip mask, so every
  partial repaint stacked another translucent coat outside the damaged
  region. Shadow extents are now also included in culling and damage
  calculations, so glows repaint correctly.
- Diagnostics for future regressions: `SMUDGY_TINY_SKIA_FULL_DAMAGE=1`
  forces full-frame repaints (bypasses damage tracking),
  `SMUDGY_TINY_SKIA_DEBUG=1` traces presents and per-layer damage
  decisions, `SMUDGY_TINY_SKIA_PAINT_DAMAGE=1` outlines repainted
  regions, and `SMUDGY_MAP_DEBUG=1` traces map widget draw state.
- The map's pan animation oscillated divergently when frames ran slower
  than the animation tick clamp (33ms). Map panning now uses a 250ms
  ease-out, which is stable at any frame rate.
- The map canvas is clipped to its bounds, so map geometry near the
  viewport edge no longer paints over neighboring UI.

## [0.2.8] - 2026-06-10

### Added

- **The map editor is now an actual editor.** Previously it only displayed
  the map; it is now a full editing environment laid out as resizable panes
  (area list, canvas, inspector) under a toolbar:
  - **Areas** can be created, renamed inline, and deleted (behind a
    confirmation showing the room count).
  - **Rooms**: click, shift-click, or rubber-band to select; drag to move
    (snapped to the grid, hold Alt for free placement); arrow keys nudge;
    Delete removes. The inspector edits title, description, level,
    position, color, and key-value properties; multi-selections support
    bulk color/level edits. An Add Room tool places rooms with a snapped
    ghost preview.
  - **Exits**: drag from a room's edge to another room to create a two-way
    exit (direction inferred from the drag; hold Ctrl for one-way), or
    drag into empty space to create a connected room. Every exit field —
    destination (including other areas), return direction,
    hidden/closed/locked, style, weight, command, path, color — is
    editable in the inspector.
  - **Labels and shapes** now render on maps (they previously didn't draw
    at all) and can be created by dragging out a rectangle, moved, resized
    via handles, and styled in the inspector. Selected labels/shapes can be
    copied, cut, and pasted (Ctrl+C/X/V) — pastes land on the current
    level, offset a step per paste, with full styling and undo.
  - **Levels**: a toolbar stepper (or PgUp/PgDn) switches the visible
    level, adjacent levels show as faint ghosts for aligning stairwells,
    and up/down exits draw as corner markers. Ctrl+PgUp/PgDn moves the
    selection itself between levels.
  - **Undo/redo** (Ctrl+Z / Ctrl+Y) covers every edit: a multi-room drag
    is one step, a typing burst in a field is one step, and undoing a
    delete restores the rooms with their properties and exits. History is
    per-area.
  - Every color field has a **color picker**: click the swatch to open an
    inline hue/saturation/value picker. Dragging previews live and writes
    once on release; typed CSS colors (hex, `rgb()`, names) still work.
    Unset colors now show a slashed empty swatch and "(default)"/"(none)"
    placeholders instead of a misleading gray `#888888`, and the bulk
    color/level fields prefill with the selection's shared value or show
    "(mixed)".
  - Edits apply **live** — there is no save button; changes hit the shared
    map immediately (visible to sessions and other windows) and sync to
    the cloud in the background, with a toolbar indicator showing
    sync status.
- The session minimap now respects the player's current level instead of
  drawing every level at once, and shows map labels and shapes.
- **Borderless main window.** The main window no longer has a native title
  bar; the toolbar now hosts minimize/maximize/close buttons and acts as the
  titlebar — drag its empty space to move the window, double-click it to
  toggle maximize. The window edges and corners remain resizable via
  invisible grips, with the OS handling the actual move/resize so edge
  snapping and minimum sizes behave natively.
- **Main window restyle.** Toolbar actions are now quiet menu-bar items
  instead of large purple buttons, and the expand/collapse toggle is the
  same hamburger icon in both states. Session headers look like actual
  tabs on a tab strip — the active session's tab is highlighted, the close
  button is a small icon, and Reconnect is a compact low-emphasis button.
  Buttons created by scripts via JSX default to the same low-emphasis style
  instead of the loud primary purple.
- **Minimap widget.** Scripts can overlay a live map on a session via the
  JSX `<Map />` element, with the current area and player location kept in
  sync with the mapper.
- Areas now fade smoothly when the player crosses between them on the map.
- Trackpad-friendly map navigation: two-finger scroll pans the map, and
  Command/Ctrl + scroll zooms. Mouse-wheel zoom and right-drag panning are
  unchanged. Previously panning required a right-button drag, which
  trackpads cannot express.
- Modal dialogs can be dismissed with Escape.
- Tab-completion is smarter about MUD-style punctuation: tokens like
  `guard:Awful,` or `Rr'Kar` complete sensibly, possessive endings are
  stripped, and typing a delimiter matches the full compound token.

### Fixed

- Typing certain colors (for example `hsl(360, 50%, 50%)`) into a map
  element's color field crashed the app: the color-parsing library panics
  on boundary values its own validator accepts. All color parsing — editor
  fields, map data synced from other clients, and colors passed by scripts
  — now treats such input as simply "not a color".
- Labels with transparent backgrounds turned white when pasted (or restored
  by undo): the cloud API fills in defaults for colors omitted at creation.
  The editor now always states styling explicitly when creating labels and
  shapes, and new labels default to a transparent background. (The API's
  creation defaults are also fixed alongside, so absent shape fills/strokes
  round-trip as "none".)
- Closing a map editor window previously leaked it; it kept processing
  player-location updates forever.
- The map editor no longer jumps to a different area when the player moves
  there mid-edit; only the player marker follows.
- **Command execution order is now deterministic and depth-first.** A script
  calling `send()` multiple times executed its commands in *reverse* order,
  and script-generated commands could preempt commands already queued behind
  the alias or trigger that produced them. Everything a command produces —
  plaintext alias expansion, script `send()` calls, trigger output — now
  executes immediately after it, in emission order, before queued siblings.
  Commands sent from asynchronous script contexts (timers, resolved
  promises) join the back of the queue like new input.

  *Note for script authors:* if a script worked around the old behavior
  (for example, by pre-reversing a sequence of `send()` calls), remove the
  workaround.
- The minimap no longer swallows mouse clicks meant for widgets beneath it;
  the terminal scrollbar and text selection work under an overlaid map.
- Scrolling the terminal with a trackpad could move the scrollback up but
  never back down. Both directions work now.
- The scrollback scrollbar no longer logs its drag position to the console.
- An empty "send on connect" no longer saves a stray newline.

### Changed

- **Trigger and alias matching is dramatically faster with large pattern
  sets.** Patterns are now classified at load time and routed to the
  cheapest engine that can match them: plain-text patterns (such as
  item-name substitutions, even when regex-escaped) are matched in a single
  Aho-Corasick pass regardless of how many there are, and remaining regexes
  are prefiltered by their required literals so the full regex engine only
  runs on lines that could match. On a real 16MB session log with ~6,300
  item-name triggers, matching dropped from ~1.2ms per line to ~0.23µs —
  roughly 5,000× faster — and rebuilding after a trigger edit got faster
  too. Profiles with thousands of substitution/highlight triggers no longer
  lag behind incoming text.
- **Upgraded to iced 0.14 from crates.io.** smudgy previously tracked a
  patched fork of an unreleased iced; its only addition (`select_range` for
  text inputs, used by tab-completion highlighting) landed upstream in
  0.14.0. The `iced` and `iced_anim` git forks are gone.
- Hotkey key-name parsing no longer relies on `unsafe` enum transmutes;
  unknown or renamed key names now fail at compile time rather than
  misbehaving at runtime.
- Internal: the core crate (session engine, scripting, telnet/VT parsing,
  models) no longer depends on any UI framework and can run headless; the
  command-ordering guarantees above are enforced by integration tests that
  exercise a full scripted session without a UI.
- Internal: large modules were reorganized for maintainability (runtime,
  connect dialog, script editor), and UI components were consolidated under
  a consistent widget/component/window hierarchy.
- Internal: a new `smudgy_bench` workspace crate benchmarks trigger-matching
  engines against a real session log and item-name corpus
  (`cargo bench -p smudgy_bench`), including the engine smudgy ships, so
  matching-performance regressions are visible.

## [0.2.7] - 2025-11-19

### Added

- Cached cloud map backend: map areas are cached locally with revision
  tracking and loaded via spatial queries, dramatically reducing cloud
  round-trips when loading and rendering maps.
