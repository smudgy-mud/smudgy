# Changelog

All notable changes to smudgy are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- **Servers offering to negotiate a character set now get an answer.** When a server
  asks smudgy to choose an encoding, smudgy names its preference — UTF-8, or the
  server's manual encoding setting ahead of UTF-8 when one is set — instead of
  declining the conversation. That preference now reads the same whichever side opens
  the negotiation — before, a manual encoding setting could be quietly passed over for
  UTF-8 depending on who asked first. Servers that hold back their UTF-8 output until a client asks now
  send it, so accented letters and line-drawing arrive as the server meant them
  instead of as mojibake. On the connections where the choice can actually change
  something — a server with a manual encoding set — what you type is held for the
  moment the negotiation takes, so it goes out in the encoding both sides agreed on
  rather than the one it started in.
- **256-color and truecolor come back on MNES servers.** Servers that ask about the
  client through NEW-ENVIRON (the MNES convention) rather than the classic
  terminal-type cycle were getting silence: smudgy accepted the option (for its OSC 8
  link capabilities, new in 0.5.3) but answered only those, so such servers concluded
  "no terminal type, no color support" and dropped the whole session to the base ANSI
  palette. Smudgy now answers the MNES identity questions — client name and version,
  terminal type, the charset in use, and the MTTS capability list (256-color,
  truecolor, UTF-8, TLS) — and claims the MNES bit in its terminal-type cycle too.

## [0.5.4] - 2026-08-18

### Added

- **Styling from scripts got simpler and sharper.** `line.highlight()` and the `style`
  chain now take any subset of options: what you set changes, and everything you leave
  out is left alone — recolor a foreground without touching backgrounds or bold.
  Text attributes accept any subset the same way, the chain gains one shorthand per
  attribute (`style.bold.red`, `style.italic.underline`, ...), a chain works directly
  as a highlight's options (`line.highlight("goblin", style.red.bgWhite)`), and a
  `link(...)` tag does too — making every match clickable in place while keeping its
  styling (`link: null` strips links from matches instead). Mistakes are loud: unknown
  color names and misspelled attribute keys now throw where they are written instead
  of silently styling nothing.
- **Servers introduce themselves on the Connect screen.** Smudgy now reads MSSP — the
  status block many MUD servers volunteer on connect — and remembers it per server:
  the game's own name when it differs from yours, the player count from your last
  visit shown with its age (a point-in-time number is honest only with one), how long
  the server has been up, a TLS-available badge, its status when not live, and
  Discord / website / contact links, which go through the same confirmation as links
  a server prints in-session. Every server also shows when you last connected,
  whether or not it speaks MSSP. All of it is optional display data — a server that
  sends nothing looks exactly as before — and none of it ever changes how smudgy
  connects. Scripts get a read-only live view: `smudgy:state/mssp` holds the
  variables as sent (strings, arrays where the server sent several values) and
  `smudgy:events/mssp` fires `updated` as data arrives.
- **A guarded offer to encrypt.** When a plain connection advertises a TLS port, an
  in-session banner offers to switch: accepting flips the server to TLS on that port
  and reconnects; "Not for this server" is remembered and the offer never returns.
  The banner appears at most once per connection, and only when the offer holds up —
  a real port different from the one in use, a server dialed by hostname (a
  certificate cannot validate against an IP address), and no conflicting `HOSTNAME`
  claim from the server itself.
- **Game icons, fetched carefully.** A server advertising an icon URL gets it shown
  beside its name on the Connect screen. The fetch is https-only and automatic only
  when the icon lives on the game's own host (or a parent domain of it) or on a host
  you have already trusted for links — any other host waits for that same per-host
  approval. Downloads are size-capped, refused for private and internal addresses,
  and the image is strictly decoded and re-encoded before being cached beside the
  server's files, refetched only when the advertised value changes.
- **The welcome screen has a cat.** The no-session screen's lightning-bolt icon is
  now a small pixel-art cat, drawn in the style of a CRT. It blinks; occasionally it
  licks. It animates only while that screen is visible and costs nothing once a
  session is open.

### Fixed

- **Windows: moving and resizing the window can no longer wedge.** On PCs with a
  touchscreen, pen, or drawing tablet, a single touch or pen press on the titlebar
  or a window edge could leave the window impossible to move or resize (maximize
  still worked) until relaunch. Moves and resizes are now handled by Windows
  itself — which also makes them work by touch and pen, and brings the native
  comforts along: Aero Snap, double-click to maximize, dragging a maximized window
  to restore it, and the titlebar's right-click system menu.
- Double-clicking the titlebar to maximize no longer also begins a window drag from
  the same click.

### Changed

- A highlight no longer repaints what its options leave unset. Previously
  `line.highlight("orc", { fg: "red" })` also reset the match's background and text
  attributes to defaults; now they are preserved. To repaint a range wholesale, pass
  explicit values for every channel — a read-back `line.styles` span works verbatim.
- `line.insert()` follows the same rule: text inserted without options (or with
  partial options) now blends into the style at the insertion point, where it
  previously always rendered in the terminal defaults.
- In the `{ color }` foreground/background form, omitting `bold` now means what the
  bare name means — the bright variant for ANSI names — where it previously selected
  the dim slot. Scripts that spell `bold` out (anything type-checked against the old
  contract, which required it) are unaffected.
- The embedded JavaScript/TypeScript runtime now follows Deno 2.9.5 (V8 150.4), including its
  updated npm/CommonJS compatibility and matching editor declarations.
- Package manifests gain an `ipc` permission axis for local IPC endpoints. Each row declares a
  Unix socket path and/or a Windows named-pipe name; the entry matching the user's platform is
  granted as an exact target (together with the read/write access those socket operations
  require), while the other entry is shown as not applying to that computer. The axis carries
  the effectively-full-access consent warning, and `net` remains internet hosts only —
  `unix:`/`vsock:` strings there are now manifest validation errors.
- Sandboxed packages can no longer terminate the application through Deno's unstable-feature
  gate (for example `Deno.connect({ transport: "vsock" })`); such calls now fail with an
  ordinary catchable error.

## [0.5.3] - 2026-08-14

### Added

- **Cross-area map labels can stay out of the way.** `MapView` styles can
  show destination names always, only while their connected room is hovered,
  or never, with an optional padded background painted above room glyphs.
  Because visibility is a connection style, a loaded route can keep its own
  destination visible over a hover-only `defaultStyle`.
- **Panes stack into tabs.** Drop a pane onto another pane's header and
  they share that spot as a tabbed group — one region, a tab per pane.
  Tabs carry their controls (connect/reconnect on a session's main tab,
  close, the hide eye), scroll when the strip fills, and
  Ctrl+Tab / Ctrl+Shift+Tab cycles a group's visible tabs from the
  keyboard. Hidden tabs stay reachable in the strip — dimmed, eye and
  all — so a hidden pane can always be brought back.
- **One drag for everything.** Rearranging panes is a single gesture
  wherever it ends: reorder tabs within a strip, merge into another
  group at an exact insertion point, split against any pane's edge, dock
  at a window edge, swap with the pane under the cursor, drop into
  another smudgy window, or release over empty desktop to tear out a new
  window. A live highlight — with an insertion caret in tab strips —
  shows exactly what the release will do (the preview and the drop share
  one geometry, so they can never disagree), and Escape cancels any drag
  with nothing moved.
- **Pick up where you left off.** Every server remembers the last
  arrangement you played it in — windows and panes, positions, sizes,
  splits, tab groups and selections, hidden panes, and which sessions
  were open. Smudgy still starts clean; the offer waits in the Connect
  dialog, where each server shows "Restore last session" with the
  profiles it will bring back. Restoring fills the window you are in
  rather than opening another. Sessions that were connected reconnect
  exactly as if you had clicked Connect (your connect commands
  included); sessions you opened offline stay offline. Script panes hold
  their exact places as empty frames while scripts load, then fill in
  without reshuffling. The record is continuous and crash-tolerant: even
  a forced shutdown (an installer closing smudgy, Windows restarting)
  keeps a snapshot at most a minute old, and an unreadable file means
  the offer simply does not appear — never a deleted one. Credentials,
  terminal contents, and script state are never written.
- **Named layouts.** The new Layouts button saves the current
  arrangement of the active session's server under a name and brings it
  back on demand. Applying runs the full flow: missing sessions spawn
  per their saved online/offline state, and extra live sessions get an
  explicit keep-or-close choice (keeping is the default; closing is
  never silent). Overwrite, rename, and delete sit behind
  confirmations, and Reset lets a session's script layout win again
  over remembered geometry. Layouts are stored per server as plain JSON
  files beside your profiles.
- **Scripts can switch layouts.** `session.layout` saves, applies, and
  lists the server's named layouts — `layout.apply("combat")` from a
  trigger, alias, or keybinding. A script apply only rearranges panes:
  it never opens or closes sessions, never prompts, and never touches
  other servers' windows, and it is safe to call at gameplay rates.
  Requires the `panes` and `session: reach-others` permissions.
 - **The client speaks your language.** Smudgy's interface is now
   localizable, and ships its first translation: 繁體中文（臺灣）
  (Traditional Chinese, Taiwan). Preferences gains a Language picker —
  System (matching your OS locale), English (United States), or
  Traditional Chinese — and switching takes effect immediately, no
  restart. Missing translations fall back to English, so future
  languages can land incrementally. Server output, player input, and
  package content are never translated. (Thanks @GTanger — #6, #8.)
- **A per-server encoding picker, including Big5.** The server form can
  now pin a legacy transport encoding (Big5, GBK, Shift_JIS, KOI8-R, and
  other MUD-relevant charsets) independent of the interface language;
  CHARSET negotiation still overrides it mid-connection. Outbound
   commands a legacy encoding cannot represent are rejected atomically
   with a clear message — nothing garbled goes to the wire.
   (Thanks @GTanger — #7, #8.)
 - **Discord Rich Presence.** Discord now shows "Playing smudgy" with
   elapsed time while the client is open, and adds the server once you
  connect: "on mud.arctic.org", falling back to the server's name from
  your list when its address is an IP or localhost. That one label is
  all Discord gets, delivered to the Discord app on your own computer
  over its local IPC pipe; who sees it from there is up to your Discord
   privacy settings. On by default; opt out in Preferences under
   Integrations.
- **Scripts can drive pane display state.** A pane handle now covers what
  the eyeball and dividers already let you do by hand: `hide()`/`show()`
  and `isHidden` (the same toggle as the title-bar eyeball, in both
  directions — scripts hear user clicks too, via the new
  `smudgy:events/pane` `visibility` event), `resize({ width, height })`
  and a live `size` read (with a `resize` event that fires once per
  settled layout change), and a per-pane terminal font override
  (`setFontSize(px | null)`, also available in the split spec as
  `fontSize`). A split spec can start a pane `hidden: true`, so
  reveal-on-event panes never flash at load; a reload keeps your eyeball
  toggles. The main pane accepts only the font override — a per-session
  override of the Preferences font size.
- **Scripts can rearrange panes.** `pane.relocate(direction, reference?)`
  moves a pane next to another one — across windows too, so relocating
  onto a pane in a torn-out window re-docks there — and
  `pane.tearOut({ width?, height? })` moves it into a fresh window of its
  own, exactly like dragging it out. Windows stay anonymous: when a
  torn-out pane leaves or closes, its window closes with it.
- **Scripts can work with the command input.** A new `input` object reads
  what's in the box, puts text there (`propose()` suggests a command
  fully selected, so typing anything discards it), moves the cursor,
  controls focus, and submits — the missing piece for clickable "put this
  command in my input" links and widgets. Reachable as `input` from
  `smudgy:core`, or `session.input`.
- **Password masking.** Scripts can switch any input into password mode:
  the box shows dots (with an eye button to peek), and the secret is kept
  from everything else — it never enters history or tab completion,
  scripts can't read it back, and the submission is sent with its echo
  redacted. Anything you were typing when masking engages is set aside
  and restored afterward.
- **Automatic password masking at login prompts.** When a MUD hides echo
  for a password prompt (telnet ECHO), the input masks itself with all of
  the protections above, and unmasks when the server restores echo. Can
  be turned off in Preferences under Input.
- **Intercept what you type.** The new `submit` event
  (`smudgy:events/sys`) fires for each line submitted from the input,
  before aliases and command splitting: a handler can observe it, rewrite
  it (`submission.replace()`), or swallow it (`submission.cancel()`) —
  shorthand expanders, confirm-before-send guards, and chat modes without
  an alias for every case. Lines sent by scripts don't fire it, and
  masked submissions never reach it.
- **Game-aware tab completion.** Scripts contribute words the input
  offers before the scrollback scan — spell names, group members from
  GMCP, speedwalk targets — via `input.completion.add(...)`, with a
  blacklist to keep noise words out of completion entirely. Each script's
  contributions stay its own; the user sees them merged.
- **Input history for scripts.** `input.history` lists what the Up arrow
  recalls (newest first), adds entries without sending, or clears it.
- **Panes can host their own input line.** A pane created with
  `input: { onSubmit }` gets a full command input under its body — its
  own history, tab completion, hotkeys, even masking — and its
  submissions go to your handler instead of the game: chat panes that
  prefix a channel, search boxes, note takers. Click a pane to focus its
  input; Escape returns to the main input.
- **Watch the input as the user types.** Observe-only `change` and
  `focus` events (`smudgy:events/input`) report edits (with what caused
  them: typing, a script, a link) and focus changes — enough for inline
  hints and validation, while masked typing reports nothing.
- **Duplicate-proof map seeding for packages.** A new
  `mapper.importAreasIfAbsent(...)` imports bundled maps only where no
  map of the same name exists anywhere on the profile — maps assigned
  to other servers and deactivated maps count — and waits for maps to
  finish loading first, so a package can safely offer its starter maps
  on every start without ever creating duplicates.
- **Widgets can show images.** A new `<Image src=... />` widget
  (`smudgy:widgets`) displays PNG, JPEG, WebP, and GIF (first frame)
  images from an `https://` URL, an inline `data:` URI, or a local file,
  with the usual sizing props plus `content_fit`, `filter_method`,
  `opacity`, and `rotation`. Remote images are cached on disk honoring
  the server's HTTP cache headers (and keep showing through network
  hiccups); a sandboxed package's image loads obey the same consented
  `net`/`read` permissions as its `fetch()` and file access. Packages
  can ship their own images and show them with `@/assets/...` or
  module-relative paths — published assets download lazily on first
  display (never at load time) and cache forever; a package you're
  authoring locally reads them straight from its folder, and edits
  show up in about a second.
- **Canvas scenes can draw images.** A new `{ kind: "image", src, x, y,
  width, height }` shape record puts rasters in script-drawn canvases —
  map backgrounds, item icons, portraits — with `fit`, `filter`
  (`"nearest"` for pixel art), `rotate`, and animatable position/size/
  rotation/opacity. Sources use the `<Image>` grammar and permissions.
  Two renderer facts to know: images always paint above lines/fills and
  below text regardless of scene order, and a scene fed through a
  binding can't name local files (same rule as bound `<Image>` srcs).
- **Per-server image cache management.** Each server's edit form now
  shows how much disk its cached images use, with a one-click clear;
  deleting a server removes its cache with it. A new
  `image_cache_max_mb` setting (default 256) bounds the whole image
  cache — the oldest-fetched entries are trimmed at startup.
- **Pick a connection's look by eye.** The connection inspector's color
  field gains the same swatch and picker rooms and labels have, and
  stroke width and dash style are chosen from panels that draw each
  choice as it will look on the map — width, dash, and color together.
  A width outside the offered list stays visible and reselectable.
- **One-way links can grow their return direction.** An "Add return
  direction" button creates the reciprocal traversal on the destination
  room and attaches it to the same link, in one undoable step. It stands
  aside when an existing reciprocal is available to Pair with, or when
  the return direction is already taken.
- **Connections light up under the cursor.** Hovering a link in the
  editor's Select mode glows it, so the click target (and click-cycling
  through crossings) is discoverable before you commit to a click.
- **Destination room numbers assume the current map.** Typing a room
  number into an exit's destination with no map picked selects the map
  you're editing automatically — the dropdown shows its name dimmed as
  the placeholder, so the default is visible before you type.
- **Maps choose where they live.** Every map and map folder (atlas) now
  has an explicit storage tier: session (this session only, discarded
  when it closes), on this device, or cloud. The New area and New folder
  dialogs gain a "Save in" choice — folders can now live on this device,
  not only in the cloud — and signed out, new maps and folders save on
  this device. Scripts choose the same way:
  `mapper.createArea(name, { storage: "session" | "local" | "cloud" })`,
  optionally with an `atlas` to create into, and `listAtlases` /
  `createAtlas` cover folders.
- **Maps and atlases move and copy between tiers.** A map's Move… dialog
  now lists every destination in one place — each of your folders with
  its tier, plus loose maps on this device or in the cloud — so filing a
  map into a folder and moving it to another tier are the same gesture,
  and a folder's own Move… relocates it with every map inside. Maps
  relocated together keep their links to each other, across tiers
  included. Scripts get the same operations — `copyArea`/`moveArea`,
  the multi-area `copyAreas`/`moveAreas`, and `copyAtlas`/`moveAtlas` —
  each taking a destination of tier plus optional folder. A cross-tier
  move copies everything to the destination before deleting the source,
  so a failure partway can leave the complete copy alongside the
  original — a duplicate to clean up, never lost work — and the editor
  then points at the existing copy instead of inviting a retry. A map
  with an unresolved sync conflict refuses to move until the conflict is
  settled.
- **Batched map edits for scripts.** `mapper.mutateArea(area, callback)`
  collects a run of related writes — creating rooms and exits, updating
  fields, linking — and submits them together when the callback
  finishes, in as few operations as the batch allows. Room numbers
  drafted inside the batch are reserved, so a room created meanwhile by
  a trigger or the map editor can't collide with them. If the callback
  throws, nothing is submitted; if a submission fails partway, the
  error reports the operations that had already committed.
- **Scripts style the map view.** `MapView` gains view-local
  presentation: a `styles` palette names each look once (room fill,
  stroke, and corner radius; connection color and width; door color),
  `apply` associates palette entries with rooms and exits — later
  entries win field-by-field over earlier ones and over `defaultStyle` —
  and `doors` overrides an exit's closed or locked state on screen.
  Top-level knobs set room spacing, the player marker's color, and
  whether doors draw at all. Exits are named by room and direction, and
  one reference styles both halves of a cross-level connection. All of
  it stays in the view: the shared map is never modified, nothing syncs,
  and a style update keeps the view's zoom and pan.

### Changed

- **The window wears its platform's frame.** On macOS the main window
  now keeps its native frame — the system's rounded corners, hairline
  border, and traffic-light buttons — with smudgy's toolbar drawn up in
  the titlebar area. On Linux under Wayland, the window draws the same
  finish itself, the way GNOME apps do: rounded top corners and a
  hairline border while floating, squared off while maximized. X11
  sessions keep the sharp rectangle, and `SMUDGY_SQUARE_CORNERS=1`
  turns the rounding off anywhere it fights a tiling layout. (Windows
  already had this look from the OS.)
- **Map links meet rooms the way the exit reads.** Compass exits keep
  their short wall stubs, and diagonal exits' stubs now leave the corner
  diagonally — but up/down, in/out, and portal links drop the stub
  entirely, running straight from the room's edge. Maps that draw up or
  down as a diagonal neighbor get a clean diagonal line instead of a
  nub-and-bend.
- **Exits sit where their direction says.** A connection endpoint now
  pins at its exit direction's home spot — east at the middle of the east
  wall, up in the top-right corner, down in the bottom-left, in and out
  at the other two corners — and is never nudged aside to make room for
  neighbors on the same wall. (A room with both an up and an east exit
  previously showed the east line pushed toward a corner.) The editor's
  Redistribute command remains for deliberately fanning out a crowded
  wall.
- **Terminal text stays on the grid.** The terminal font no longer merges
  character pairs like `=>` or `fi` into single glyphs — every character
  keeps its own column, which most MUD output assumes. Fonts with heavy
  contextual shaping (Monaspace, Lilex, Fira) now line up box-drawn maps
  and ASCII tables correctly. A new **Font ligatures** checkbox in
  Preferences → Appearance turns the merging back on, live. Ligatures in
  the rest of the app are unaffected.
- **Missing characters borrow from a monospaced font.** When the terminal
  font lacks a character, the replacement glyph is now drawn from another
  monospaced font (preferring the best coverage for the text at hand)
  instead of whatever proportional font the system suggested — so
  box-drawing, arrows, and other exotic characters no longer break column
  alignment or line height.
- Script automation factories now accept only the pattern/key/options-first
  signatures introduced in 0.4; the deprecated positional-name forms have been
  removed for 0.5.
- Scripts must import the map API from `smudgy:core`; the deprecated ambient
  `mapper` value has been removed. The ambient map types remain available, and
  the `Area` constructor is now an explicit import for `instanceof` checks.
- Package manifests and consent records now use only the canonical
  `interop: ["read", "write"]` capabilities; the deprecated
  `events: ["subscribe", "emit"]` alias has been removed for 0.5.
- Creating rooms while auto-mapping no longer slows down as more maps
  are loaded: the cost of a map write now scales with the touched map,
  not with everything loaded. With 100,000 rooms loaded, an auto-mapped
  step dropped from ~120 ms to ~2 ms, and stays single-digit
  milliseconds even at procedural-MUD scale.
- **The link inspector reads from your room's perspective.** Select a
  room, then one of its connections: the inspector lists that room as
  the From end — endpoint editors and traversals reorder to match, with
  From/To labels and room titles alongside the numbers.
- **Up/down stub exits draw as their triangles.** An unlinked up or down
  exit shows a hollow ▲/▼ at its fixed room corner — stroke only, so a
  real cross-level link's filled triangle still reads as linked — and
  the triangle is what you click, select, and see highlighted. Endpoints
  whose representation *is* the triangle (cross-level links, up/down
  stubs) no longer expose a draggable wall port; up/down links drawn as
  lines on the same level, and up/down exits to other maps, keep their
  placeable ports.
- **Changing an exit's direction moves its port along.** Switching an
  exit from north to east (or to up/down, or any direction) re-anchors
  the connection's endpoint at the new direction's home slot in the same
  undo step, instead of leaving the line attached to the old wall.
- **Selected connections are visibly selected.** The selection highlight
  is a real accent halo around the stroke (it was a fraction of a pixel
  wide), with the line's own color and dash redrawn inside it; cross-
  level links highlight their drawn triangle or fading stub — which are
  now also clickable exactly as drawn.
- Release builds now default the log level to `info` when `SMUDGY_LOG` is
  unset, so a production `smudgy.log` carries operational events rather
  than the debug stream; debug builds keep their `debug` default, and an
  explicit `SMUDGY_LOG` still overrides either.
- **On-connect text waits for the server to speak.** A profile's
  auto-send text now goes out when the first real server output arrives
  instead of the moment the connection opens, so triggers matching the
  greeting run before it and login commands no longer race the banner. A
  server that never prints anything does not receive the text.

### Deprecated

- Creating a map with the `ephemeral` flag is deprecated in favor of an
  explicit `storage: "session"`, as is the `isEphemeral` read (use
  `storage === "session"`). Both keep working through 0.5.x and are
  removed in 0.6.0. Creating a map with no storage choice at all remains
  fully supported: it saves to the cloud when signed in and to this
  device when not.

### Fixed

- Emoji variation selectors, joiners, and tag characters in OSC link labels now
  reach the text shaper instead of appearing as literal `\u{...}` escapes;
  invisible characters in disclosed link destinations remain escaped.
- Concealed OSC spoilers now render one space per Unicode grapheme instead of
  relying on foreground color, which could not hide color emoji glyphs. Click
  and selection offsets remain aligned with the original text after concealment.
- Clicking a terminal command link no longer panics when it tries to update
  scrollback during event dispatch; script send links and OSC `send:` links now
  defer their session work through the ordinary UI update path.
- Dragging a pane divider immediately after the layout changed under it
  (a pane opened, closed, or moved in the same moment) could apply the
  resize to the wrong edge; stale divider targets are now rejected and
  re-derived from the current layout.
- Connections attached near a room's corner no longer float in the gap
  left by the rounded outline: ports follow the drawn edge around the
  corner, meeting the adjacent wall at the corner's diagonal. Dragging an
  endpoint now also snaps to the wall midpoint and corner slots — hold
  Alt to place it freely.
- Plain-string `line.replace(...)` now preserves the style of the matched text,
  including when the match starts exactly at a color boundary; it previously
  restyled the replacement with the line's first color.
- Every map overlay (`MapView`) a script ever mounted quietly kept its
  pan/zoom state and player-location tracking for the rest of the
  session, even after the widget was removed. Unmounted maps are now
  fully released.
- The map editor could paint map and drag-preview geometry past the
  canvas edge, over its own area list and inspector panes (visible under
  the software renderer, whose partial repaints don't paint over the
  spill). The editor canvas now clips to its bounds, as the session
  minimap has since 0.3.0.
- The software renderer announces itself in the log at startup, so
  rendering reports can be triaged without guessing which renderer was
  active.
- Clicking a connection's port or waypoint handle no longer records a
  do-nothing undo step — and a bare click on an Automatic route's
  waypoint no longer silently converts the route to Manual. Dragging
  still does what it always did, starting once the pointer actually
  moves.
- Deleting or cutting a mixed selection now includes its explicitly
  selected links (they were silently left behind); undo restores the
  link and both traversals exactly once. Copying a link by itself now
  works too: pasting attaches it to the same-numbered rooms in the
  target map when they exist, and reports what couldn't attach instead
  of creating ambiguous duplicate exits.
- The editor's status-bar hints no longer claim dragging an Automatic
  route's line converts it (only handle drags and Ctrl+click do), Stub
  routing explains why there's nothing to edit, and a selected port
  advertises its arrow-key wall slide.
- **One command input holds focus at a time.** Keyboard focus over
  command inputs is now reconciled across every smudgy window: exactly
  one input is focused, and switching to another application and back
  counts as a real focus loss and return. Previously the loss went
  unnoticed — an input in a background window still counted as focused —
  so focus-driven behavior, from hotkey routing to scripts' input focus
  events, could follow an input in a window you had left.

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
  speedwalk associate the atlas); anything you create is scoped to the
  server you created it on. The map editor gains a This server / All
  atlases view and a per-atlas "Servers…" checklist for adjusting scope
  directly.
- **Shared maps arrive organized and homed.** Areas shared to you now carry
  their owner's folder name, so a share appears as named folders instead of
  a flat pile. When creating a share you can disclose which game hosts the
  maps belong to (pre-checked, removable, per share); the recipient's client
  files the share under their matching server automatically, and with no
  match it simply lands in Unassigned.
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
  hyperlinks now render as clickable links in the terminal. `http`/`https`/`ftp`
  links open in your browser; a MUD-specific `send:` link sends a command as
  you. Because these come from the server, the first click to a given site
  (or the first command link) opens a confirmation showing the exact
  destination — nothing the server sends can disguise it — where you can also
  choose to always allow that site or always trust every link from that
  server. Mudlet-compatible OSC styling now controls link colors, bold,
  italic, underline/overline/strikethrough forms, and decoration color;
  unstyled links retain Smudgy's underline and subtle wash, while an authored
  style (including `underline: false`) takes precedence. Stateful hover,
  press, focus, visited, selected, disabled, link, and any-link styles follow
  Mudlet's priority order. Tooltips, styled menu titles, context menus,
  disabled links, spoilers, timed/input/prompt/output visibility, radio and
  checkbox selection groups, compact keys, and session presets are supported;
  styled tooltip text is announced through Smudgy's
  `OSC_HYPERLINKS_TOOLTIP_SGR` capability extension; links can be traversed and
  activated from the keyboard. Selection and visited state agree across split
  views and routed panes. Incoming OSC 8 URIs are capped at 4096 bytes, while
  the parser bounds every OSC and APC string at 8192 bytes; deceptive invisible
  controls are rendered visibly, and unapproved URI schemes are ignored.
  Script-authored links retain their existing unrestricted payload size.
- **Configurable SGR bold presentation.** Terminal preferences now offer
  weight-only, bright-color-only, and combined bold modes. Weight-only bold
  preserves the selected regular font family and changes only its weight;
  existing boolean settings migrate without changing their appearance. The
  three choices use the selected interface language.
- **Scripts can round-trip terminal styling.** `line.styles` now reports every
  text attribute plus the exact ANSI palette-bright bit, and styled echo and
  line edits accept those attributes without losing double underline, fast
  blink, reverse video, or the distinction between bold weight and bright
  color. The legacy color `bold` readback remains available for compatibility.

### Fixed

- **Bold variable terminal fonts stay in their selected family.** Requesting
  bold from a variable font such as Geist Mono no longer falls through to a
  proportional face just because the font file registers one default weight;
  Smudgy now recognizes every weight covered by the font's `wght` axis.
- **Async link tooltips show progress.** A script tooltip whose callback is
  still resolving now opens with an animated loading indicator (and the honest
  target disclosure, when available) instead of appearing empty or finished.
  Tooltips also stay open when a link is reached from the keyboard.
- **Link clicks no longer hijack later command submissions.** Enter and Space
  activate a terminal link only while its keyboard focus ring is visible, and
  returning focus to a command editor clears the terminal's link-navigation
  focus instead of replaying the last mouse-clicked OSC or script link.
- **OSC 8 raw JSON and reserved query fields parse without URI ambiguity.**
  Literal JSON may contain `#` colors and `&` text, encoded `config%3D` and
  `preset%3D` fields are stripped like their literal forms, and real URL
  fragments and ordinary query parameters remain intact. Presets are capped at
  1024 names per connection; existing names remain replaceable at the cap.
- **OSC control strings cannot desynchronize terminal output.** Bare ESC bytes
  and UTF-8 continuation bytes no longer confuse a shadow OSC scanner and
  swallow all later output. Unterminated OSC and APC strings are also bounded,
  including non-hyperlink selectors.
- **OSC link state follows the session and the rendered buffer.** Selection and
  visited styles stay synchronized across scrollback splits and routed panes;
  spoilers survive widget rebuilds; input and prompt expiry reach links in
  routed panes; evicted selection values are retired.
- **Empty OSC actions are handled safely.** Empty `send:` and hostless web URLs
  are ignored, while empty `prompt:` remains a valid way to clear the command
  editor.
- **JSR packages can load slash-normalized dependencies.** Smudgy now accepts
  both `jsr:@scope/package` and the `jsr:/@scope/package` form emitted by Deno
  and found in published JSR packages, instead of rejecting the latter as an
  unscoped package.
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
- **ANSI text attributes display.** Bold is now real font weight, and faint,
  italic, underline, double underline, slow/fast blink, reverse video, and
  crossed-out text render instead of being discarded. A new preference,
  enabled by default for compatibility, controls whether bold text also uses
  the bright ANSI palette; explicit bright-color codes stay bright either
  way. Attribute set/reset codes apply independently, unknown codes no longer
  poison colors beside them, bare `ESC[m` resets, colon-form extended colors
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
