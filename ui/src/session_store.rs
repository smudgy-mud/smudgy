//! Daemon-owned session state.
//!
//! Sessions outlive and span windows: a `SmudgyWindow` grid holds only pane
//! references, while the session state itself — runtime channel, terminal
//! buffer, input, widgets, mapper — lives in the [`SessionStore`] on the
//! daemon. Session ids come from a single daemon-global counter, because
//! everything downstream keys on them globally: core's session registry and
//! the iced subscription identities (which hash `SessionParams` by id) would
//! both silently collide under per-window counters.

use crate::components::session_input;
use crate::theme::Element;
use crate::widgets::split_terminal_pane;
use iced::widget::{
    button, center, checkbox, column, container, mouse_area, opaque, operation, row, space, stack,
    svg, text,
};
use iced::{Alignment, Border, Color, Length, Padding, Subscription, Task};
use smudgy_widgets::{
    MapStore, MapWidgetId, TextEditorStore, WidgetRoot, with_store_context, with_text_store_context,
};
use log::info;
use smudgy_core::get_smudgy_home;
use smudgy_core::models::map_scopes::MapScopes;
use smudgy_core::models::profile::load_profile;
use smudgy_core::models::server::{ServerConfig, link_url_host, load_server, update_server};
use smudgy_core::models::settings::{ScriptSettings, Settings, load_settings};
use smudgy_core::session::runtime::pane::{
    MAIN_PANE_KEY, MAIN_PANE_NAME_ID, PaneDef, PaneKey, PaneKind, TitleBarPolicy,
};
use smudgy_core::session::runtime::{IsolateId, RuntimeAction};
use smudgy_core::session::{self, SessionEvent, SessionId};
use smudgy_core::session::{BufferUpdate, TaggedSessionEvent};
use smudgy_core::session::SessionParams;
use crate::cloud_account::CloudHandles;
use crate::terminal_buffer::selection::Selection;
use crate::terminal_buffer::{LinkClickEvent, TerminalBuffer};
use smudgy_core::session::styled_line::LinkAction;
use smudgy_cloud::{
    AreaId, AtlasId, CachedCloudMapper, CloudMapper, CompositeBackend, CredentialSource,
    LocalBackend, Mapper, PackageApiClient,
};
use smudgy_map_widget::map_view;
use smudgy_theme::builtins::container::default;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use tokio::sync::mpsc::{self};

/// All live sessions, keyed by id. Windows borrow sessions from here to
/// render and update their panes; the daemon routes session events here
/// directly. An id missing from the store is the signal that the session was
/// torn down — late events and actions for it are dropped by the daemon.
pub struct SessionStore {
    cloud: CloudHandles,
    sessions: BTreeMap<SessionId, ManagedSession>,
    next_session_id: SessionId,
}

impl SessionStore {
    pub fn new(cloud: CloudHandles) -> Self {
        Self {
            cloud,
            sessions: BTreeMap::new(),
            next_session_id: 0.into(),
        }
    }

    /// Allocates an id and creates the session state. Giving the session a
    /// pane in some window's grid is the caller's job.
    pub fn open_session(
        &mut self,
        server_name: String,
        profile_name: String,
        auto_connect: bool,
    ) -> SessionId {
        let session_id = self.next_session_id;
        self.next_session_id = self.next_session_id + 1.into();

        let session = ManagedSession::new(
            session_id,
            server_name,
            profile_name,
            self.cloud.credentials.clone(),
            self.cloud.base_url.as_str(),
            auto_connect,
        );
        self.sessions.insert(session_id, session);
        session_id
    }

    pub fn get(&self, session_id: SessionId) -> Option<&ManagedSession> {
        self.sessions.get(&session_id)
    }

    pub fn get_mut(&mut self, session_id: SessionId) -> Option<&mut ManagedSession> {
        self.sessions.get_mut(&session_id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (SessionId, &ManagedSession)> {
        self.sessions.iter().map(|(id, session)| (*id, session))
    }

    /// Shuts the session's runtime down and drops its state. Returns whether
    /// the session existed (`false` makes a repeated close a clean no-op).
    ///
    /// Teardown ordering: callers remove the store entry (this call) *before*
    /// cleaning any window grid, so events still in flight for the id fail
    /// the store lookup and are dropped — a dead session can never re-enter
    /// a grid.
    pub fn shutdown_and_remove(&mut self, session_id: SessionId) -> bool {
        match self.sessions.remove(&session_id) {
            Some(mut session) => {
                session.shutdown();
                true
            }
            None => false,
        }
    }
}

/// The UI-side display state for one non-main pane: the scrollback buffer +
/// selection for a terminal pane; widgets-only panes carry no buffer. Widget
/// trees stack over the terminal on terminal panes and are the whole body on
/// widgets-only panes (they render from the session's shared `WidgetRoot`,
/// matched by the pane's interned name id, so they need no state here).
pub struct PaneDisplay {
    pub def: PaneDef,
    buffer: Option<Rc<RefCell<TerminalBuffer>>>,
    selection: Rc<RefCell<Selection>>,
}

/// A live session: connection params, the runtime channel, and everything its
/// panes render from (terminal buffer, input, script widgets, mapper).
/// Owned by the [`SessionStore`]; windows hold only pane references to it.
pub struct ManagedSession {
    pub id: SessionId,
    /// The name of the server this session is connected to
    pub server_name: String,
    /// The name of the profile used for this connection
    pub profile_name: String,
    /// Input component for sending commands
    pub input: session_input::SessionInput,

    pub session_params: Arc<SessionParams>,

    pub mapper: Option<Mapper>,

    terminal_buffer: Rc<RefCell<TerminalBuffer>>,
    terminal_pane_selection: Rc<RefCell<Selection>>,

    /// Display state for this session's non-main panes, keyed by the
    /// never-reused `PaneKey`. Existence is core's call (`PaneOpened`/
    /// `PaneClosed`); placement lives in the windows' grids.
    panes: HashMap<PaneKey, PaneDisplay>,

    /// The main pane's header-visibility policy. The main pane has no
    /// `PaneDisplay` entry (its buffer/input live directly on the session),
    /// so the one mutable def field the UI reads is mirrored here — set via
    /// `PaneUpdated` when a script re-policies `main`.
    main_title_bar: TitleBarPolicy,

    widget_root: WidgetRoot<'static, crate::Theme, crate::Renderer>,
    map_store: MapStore,
    text_store: TextEditorStore,

    runtime_tx: Option<mpsc::UnboundedSender<RuntimeAction>>,

    connected: bool,
    /// Whether to establish a connection automatically once the runtime is
    /// ready (and to reconnect after a reload). `false` for a session opened
    /// offline, until the user presses Connect; an explicit Disconnect clears
    /// it again so a later reload doesn't silently reconnect.
    auto_connect: bool,
    /// Whether this session has ever been connected. Drives the Connect (never
    /// connected) vs Reconnect (was connected) label on the title-bar control.
    ever_connected: bool,

    /// This server's config — the address plus the OSC 8 link-trust grants —
    /// shared with the link handler and updated when the user opts in from
    /// the confirm dialog.
    server_config: Rc<RefCell<ServerConfig>>,
    /// A server-sent link awaiting the user's trust verdict. Written by the
    /// link handler (which runs inside widget event processing, so it stages
    /// state instead of publishing a message) and rendered as a dialog over
    /// the session; see [`Self::link_confirm_dialog`].
    pending_link_confirm: Rc<RefCell<Option<PendingLinkConfirm>>>,
    /// Per-session bind-on-use state (map scoping plan §3 convergence): the
    /// locate streak, Undo suppressions, and rescue-offer rate limits. Read and
    /// written by the daemon, which owns the authoritative scope store.
    pub bind_tracker: BindTracker,
    /// The current bind-on-use / cross-entry-rescue toast over this session's
    /// main pane, or `None`. One at a time — a new bind replaces it.
    toast: Option<SessionToast>,
    /// Monotonic toast generation, so a stale auto-dismiss timer can't clear a
    /// newer toast that replaced the one it was scheduled for (mirrors the
    /// automations window's `toast_gen`).
    toast_gen: u64,
}

/// One server-sent link (OSC 8) held at the trust gate, with the dialog's
/// checkbox state.
#[derive(Debug, Clone)]
struct PendingLinkConfirm {
    /// The gated action, performed verbatim on Proceed.
    action: LinkAction,
    /// What the user is shown: the full URL, or the exact command a `send:`
    /// link would transmit. Never server-relabelable.
    display: String,
    /// The URL's host (the per-host grant key); `None` for a `send:` link.
    host: Option<String>,
    grant_host: bool,
    grant_server: bool,
}

#[derive(Debug, Clone)]
pub enum Message {
    None,
    Close,
    Input(session_input::Message),
    SessionEvent(SessionEvent),
    SetMapperCurrentLocation(AreaId, Option<i32>),
    WidgetMapMessage {
        id: MapWidgetId,
        message: map_view::Message,
    },
    Reload,
    Reconnect,
    Disconnect,
    /// A click released over the main pane's terminal (bubbled — the terminal
    /// deliberately leaves presses uncaptured). Focuses the session input
    /// when the click didn't create a selection.
    TerminalClicked,
    /// Global settings changed: apply the scrollback limit and re-bake span
    /// styles here, and forward the runtime-relevant pieces to the session.
    ApplySettings(Settings),
    /// The link-trust dialog's "always allow links to <host>" checkbox.
    LinkConfirmGrantHost(bool),
    /// The link-trust dialog's "always trust links from this server" checkbox.
    LinkConfirmGrantServer(bool),
    /// Perform the pending server link, persisting any checked grants first.
    LinkConfirmProceed,
    /// Dismiss the pending server link without acting.
    LinkConfirmCancel,
    /// The bind-on-use / rescue toast's timed dismiss fired (or its close was
    /// clicked). The generation guards against a stale timer clearing a newer
    /// toast that replaced this one.
    DismissBindToast(u64),
    /// The bind/rescue toast's action button was clicked (Undo a bind, or accept
    /// a cross-entry rescue). The daemon reads the toast's staged action and
    /// applies it to the scope store, so this carries no payload.
    BindToastActionClicked,
}

/// The unit a per-server cloud-map scope association is written against, mirrored
/// from the map editor's `ScopeTarget`: a whole cloud atlas, or a genuinely
/// atlas-less cloud area. Local and ephemeral areas never become a `BindTarget`
/// — the daemon filters them out before one is formed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BindTarget {
    Atlas(AtlasId),
    Area(AreaId),
}

/// How many consecutive location updates into the same unassigned atlas (or
/// atlas-less cloud area) it takes for bind-on-use to associate it with the
/// session's server entry. Ten is long enough that a handful of incidental
/// cross-scope locate matches never bind (they can't sustain a streak against
/// the real map), yet short enough to converge within a minute of ordinary
/// play. A single speedwalk into an unassigned atlas binds immediately (see
/// [`BindTracker::observe_navigation`]) — the streak governs only the passive
/// locate signal.
pub const LOCATE_BIND_STREAK: u32 = 10;

/// Per-session bind-on-use bookkeeping (convergence, map scoping plan §3): the
/// running locate streak, the targets the user has declined (Undo) or that a
/// rescue already covered this session, and the atlases/areas a cross-entry
/// rescue has already been offered for. Pure and self-contained so the streak
/// and suppression rules are unit-testable without a live session; the daemon
/// supplies the already-resolved `(target, unassigned)` inputs.
#[derive(Debug, Default)]
pub struct BindTracker {
    /// The atlas/area the current locate streak is accruing for, and its length.
    streak: Option<(BindTarget, u32)>,
    /// Targets suppressed for the rest of the session: an Undone bind must not
    /// instantly re-bind when the streak refires.
    suppressed: HashSet<BindTarget>,
    /// Targets a cross-entry rescue has already been offered for, so the offer
    /// fires at most once per target per session.
    rescue_offered: HashSet<BindTarget>,
}

impl BindTracker {
    /// Feed one resolved locate. `target` is the atlas (or atlas-less cloud
    /// area) the located room belongs to; `unassigned` is whether that target is
    /// Unassigned for this session's entry — the only state that binds (Here is
    /// already bound; Elsewhere is the rescue path). Returns `true` exactly when
    /// the streak reaches [`LOCATE_BIND_STREAK`] and the target should bind now.
    /// Any non-unassigned or suppressed observation breaks the streak.
    pub fn observe_locate(&mut self, target: BindTarget, unassigned: bool) -> bool {
        if !unassigned || self.suppressed.contains(&target) {
            self.streak = None;
            return false;
        }
        let count = match &mut self.streak {
            Some((current, count)) if *current == target => {
                *count += 1;
                *count
            }
            _ => {
                self.streak = Some((target, 1));
                1
            }
        };
        if count >= LOCATE_BIND_STREAK {
            // Consume the streak so a fresh one must re-accrue after a bind.
            self.streak = None;
            true
        } else {
            false
        }
    }

    /// Break any accruing streak (the located area is ephemeral/local, or there
    /// is no cloud target) without binding.
    pub fn reset_streak(&mut self) {
        self.streak = None;
    }

    /// Demonstrated navigation intent (a speedwalk / find-nearest resolution):
    /// binds immediately when the destination target is unassigned and not
    /// suppressed. No streak — one navigation is enough.
    #[must_use]
    pub fn observe_navigation(&self, target: BindTarget, unassigned: bool) -> bool {
        unassigned && !self.suppressed.contains(&target)
    }

    /// Record that the user Undid a bind for `target`: suppress re-binding it for
    /// the session and drop any streak accruing for it.
    pub fn suppress(&mut self, target: BindTarget) {
        self.suppressed.insert(target);
        if matches!(&self.streak, Some((current, _)) if *current == target) {
            self.streak = None;
        }
    }

    /// Rescue-offer rate limit: returns `true` the first time `target` is
    /// offered this session, `false` afterward.
    pub fn mark_rescue_offered(&mut self, target: BindTarget) -> bool {
        self.rescue_offered.insert(target)
    }
}

/// A bottom-pill toast over a session's main pane: a short message and an
/// optional action button (Undo a bind / accept a rescue). Timed-dismissed after
/// [`BIND_TOAST_DISMISS`]; one at a time (a new bind replaces the current one).
#[derive(Debug, Clone)]
struct SessionToast {
    message: String,
    /// The action button's label and the staged action, or `None` for a
    /// message-only toast.
    action: Option<(String, ToastAction)>,
}

/// The action a bind/rescue toast's button performs, staged on the toast and
/// applied by the daemon (which owns the scope store) when the button is
/// clicked.
#[derive(Debug, Clone, Copy)]
pub enum ToastAction {
    /// Undo a bind-on-use: remove this entry from the target's associations and
    /// suppress re-binding it this session.
    UndoBind(BindTarget),
    /// Accept a cross-entry rescue: add this entry to the target's associations.
    AcceptRescue(BindTarget),
}

/// The bind/rescue toast's auto-dismiss delay. Longer than the automations
/// window's confirmation toast (2.2s) because this one carries an undo the user
/// must have time to read and act on.
const BIND_TOAST_DISMISS: std::time::Duration = std::time::Duration::from_secs(8);

/// The bind-on-use / rescue toast: a bottom-center pill with the message and an
/// optional action button. Styled after the automations window's toast (modal
/// surface). Filling the pane but transparent outside the pill, so terminal
/// clicks fall through to the layer below — only the action button captures.
fn bind_toast_overlay(toast: &SessionToast) -> Element<'static, Message> {
    let mut content = row![text(toast.message.clone()).size(13.0)]
        .spacing(12.0)
        .align_y(Alignment::Center);
    if let Some((label, _)) = &toast.action {
        content = content.push(
            button(text(label.clone()).size(13.0))
                .padding(Padding {
                    top: 4.0,
                    bottom: 4.0,
                    left: 10.0,
                    right: 10.0,
                })
                .on_press(Message::BindToastActionClicked),
        );
    }
    let pill = container(content.align_y(Alignment::Center))
        .padding(Padding {
            top: 8.0,
            bottom: 8.0,
            left: 16.0,
            right: 16.0,
        })
        .style(|theme: &crate::Theme| container::Style {
            background: Some(theme.styles.modal.body_background),
            border: theme.styles.modal.body_border,
            shadow: theme.styles.modal.shadow,
            ..Default::default()
        });

    container(column![space::vertical(), pill].align_x(Alignment::Center))
        .width(Length::Fill)
        .height(Length::Fill)
        .padding(20)
        .into()
}

/// Open a URL in the system browser, detached; a failure is logged, never
/// fatal.
fn open_url_in_browser(url: &str) {
    if let Err(e) = open::that_detached(url) {
        log::error!("Failed to open {url} in the browser: {e}");
    }
}

/// Middle-elide `s` to at most `max` chars (keeping both ends), so a long
/// unbroken token can't blow out a fixed-width dialog. Counts by `char` to
/// stay on boundaries.
fn elide_middle(s: &str, max: usize) -> String {
    let len = s.chars().count();
    if len <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1);
    let head = keep.div_ceil(2);
    let tail = keep - head;
    let head_str: String = s.chars().take(head).collect();
    let tail_str: String = s.chars().skip(len - tail).collect();
    format!("{head_str}\u{2026}{tail_str}")
}

/// The scrollback limit to fall back to when the configured value is zero.
fn scrollback_limit(settings: &Settings) -> NonZeroUsize {
    NonZeroUsize::new(settings.scrollback_length)
        .unwrap_or(NonZeroUsize::new(100_000).expect("default scrollback is non-zero"))
}

/// A small icon button for a pane's title-bar controls row (close, the
/// visibility eye). The icon color derives from the palette so it stays
/// visible when the user remaps the theme.
pub fn title_bar_icon_button<M: Clone + 'static>(
    handle: svg::Handle,
    message: M,
) -> Element<'static, M> {
    button(
        svg(handle)
            .width(11)
            .height(11)
            .style(|theme: &crate::Theme, _| svg::Style {
                color: Some(theme.styles.text.normal.scale_alpha(0.5)),
            }),
    )
    .style(smudgy_theme::builtins::button::link)
    .padding(3)
    .on_press(message)
    .into()
}

impl ManagedSession {
    /// Creates the session state for `server_name`/`profile_name`. With
    /// `auto_connect` the connection is established as soon as the runtime is
    /// ready; without it the session opens **offline** — the runtime, mapper,
    /// scripting, and automations all start, but no connection is made until
    /// the user presses Connect (lets the map editor / automations be used
    /// without a live server).
    ///
    /// `credentials` is the app-wide hot-swappable credential slot: logging
    /// in or out upgrades this session's mapper without a reconnect.
    fn new(
        id: SessionId,
        server_name: String,
        profile_name: String,
        credentials: CredentialSource,
        base_url: &str,
        auto_connect: bool,
    ) -> Self {
        let settings = load_settings();

        info!("Settings: {settings:?}");

        // Create a single shared terminal buffer, sized from settings
        let terminal_buffer = Rc::new(RefCell::new(TerminalBuffer::new_with_max_lines(
            scrollback_limit(&settings),
        )));

        // Load profile to get the subtext (caption) once
        let profile_subtext = match load_profile(&server_name, &profile_name) {
            Ok(profile) => Arc::new(profile.config.caption),
            Err(_) => Arc::new(String::new()), // Default to empty string on error
        };

        // Cloud client for smudgy:// package resolution. Shares the session's
        // hot-swappable credential slot, so logging in/out upgrades it too.
        let package_client = PackageApiClient::new(base_url, credentials.clone());

        let map_cache_dir = Self::map_cache_dir();
        // Local maps are per-server (they include auto-mapped session maps you
        // promote, which belong to the game you're playing). A pre-0.4.1 global
        // store is split into the per-server dirs once at app startup
        // ([`migrate_legacy_global_local_maps`]), so by session time this dir
        // is authoritative.
        let local_map_dir = Self::local_map_dir(&server_name);

        // The mapper always exists; with no credential the cloud tier idles
        // logged-out (cached reads still work) while the local tier stays
        // fully available on disk. Both tiers are fanned together so this
        // session's tree shows local and cloud folders side by side.
        let mapper = {
            let cloud = CachedCloudMapper::new(
                CloudMapper::with_credentials(base_url.to_string(), credentials),
                map_cache_dir.clone(),
            );
            let local = LocalBackend::new(local_map_dir);
            let backend = CompositeBackend::new(Arc::new(local), Arc::new(cloud));
            let mapper = Mapper::new(Arc::new(backend), map_cache_dir.clone());
            // Honor the user's per-area "don't use for room identification"
            // preferences. Unknown ids are preserved until their area lands,
            // so applying before load_all_areas is safe.
            if !settings.disabled_map_areas.is_empty() {
                mapper.set_disabled_areas(settings.disabled_map_areas.iter().copied().collect());
            }
            // Apply this server entry's cloud-map scope: atlases/areas
            // associated only with other entries are excluded here. Keyed on the
            // entry name and preserved until the area lands, like the disabled
            // set above. The daemon re-pushes this whenever associations change.
            let scopes = MapScopes::load();
            mapper.set_scope_exclusions(
                scopes.excluded_atlases(&server_name),
                scopes.excluded_areas(&server_name),
            );
            Some(mapper)
        };

        let widget_root = WidgetRoot::new();
        let map_store = MapStore::new();
        let text_store = TextEditorStore::new();

        let extra_script_extensions = {
            let widget_root = WidgetRoot::clone(&widget_root);
            let mapper = mapper.clone();
            Arc::new(move || vec![smudgy_widgets::ext::init(widget_root.clone(), mapper.clone())])
        };

        // Mounted widgets are engine-generation state: their callbacks are v8 handles minted
        // by the engine's isolates, dead after any engine rebuild. The runtime invokes this
        // before each engine build, so re-mounts land in an empty root.
        let on_engine_rebuild = {
            let widget_root = WidgetRoot::clone(&widget_root);
            Some(Arc::new(move || widget_root.clear()) as Arc<dyn Fn() + Send + Sync>)
        };

        Self {
            id,
            session_params: Arc::new(SessionParams {
                session_id: id,
                server_name: Arc::new(server_name.clone()),
                profile_name: Arc::new(profile_name.clone()),
                profile_subtext,
                mapper: mapper.clone(),
                package_client: Some(package_client),
                extra_script_extensions,
                on_engine_rebuild,
            }),
            server_config: Rc::new(RefCell::new(
                load_server(&server_name).map_or_else(
                    |e| {
                        // Sessions can outlive an on-disk rename; a fallback
                        // config simply has no grants, so every server link
                        // asks (and grants made now cannot persist).
                        log::warn!("Failed to load server config for '{server_name}': {e}");
                        ServerConfig::new(String::new(), 1)
                    },
                    |server| server.config,
                ),
            )),
            pending_link_confirm: Rc::new(RefCell::new(None)),
            bind_tracker: BindTracker::default(),
            toast: None,
            toast_gen: 0,
            server_name,
            profile_name,
            input: session_input::SessionInput::new().with_terminal_buffer(terminal_buffer.clone()),
            terminal_buffer: terminal_buffer.clone(),
            terminal_pane_selection: Rc::new(RefCell::new(Selection::default())),
            panes: HashMap::new(),
            main_title_bar: TitleBarPolicy::Normal,
            runtime_tx: None,
            connected: false,
            auto_connect,
            ever_connected: false,
            mapper,
            widget_root,
            map_store,
            text_store,
        }
    }

    /// Returns whether this session is currently connected
    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// Materialize the display state for a freshly opened pane. Idempotent by
    /// key (`PaneKey`s are never reused, so a duplicate event is harmless).
    pub fn open_pane(&mut self, def: PaneDef) {
        self.panes.entry(def.key).or_insert_with(|| {
            let buffer = match def.kind {
                PaneKind::Terminal => {
                    let settings = load_settings();
                    Some(Rc::new(RefCell::new(TerminalBuffer::new_with_max_lines(
                        scrollback_limit(&settings),
                    ))))
                }
                // Widgets-only: no scrollback (the widget stack is the
                // pane's whole body).
                PaneKind::Widgets => None,
            };
            PaneDisplay {
                def,
                buffer,
                selection: Rc::new(RefCell::new(Selection::default())),
            }
        });
    }

    /// The header-visibility policy for one of this session's panes — what
    /// the hosting window's `show_header` rule reads each frame. Unknown keys
    /// (a stale slot mid-teardown) fall back to `Normal`.
    pub fn title_bar_policy(&self, key: PaneKey) -> TitleBarPolicy {
        if key == MAIN_PANE_KEY {
            self.main_title_bar
        } else {
            self.panes
                .get(&key)
                .map_or(TitleBarPolicy::Normal, |pane| pane.def.title_bar)
        }
    }

    /// Slim title-bar content for a script pane: its display-cased name. Kept
    /// intrinsic-width — the bar's leftover space is the drag pick area.
    pub fn script_pane_title(&self, key: PaneKey) -> Element<'static, Message> {
        let label = self
            .panes
            .get(&key)
            .map_or_else(|| key.to_string(), |pane| pane.def.name.to_string());
        container(
            text(label)
                .size(12)
                .color(Color::from_rgba8(255, 250, 239, 0.6)),
        )
        .padding(Padding {
            top: 4.0,
            right: 10.0,
            bottom: 4.0,
            left: 10.0,
        })
        .into()
    }

    /// The body of a script pane: the widget entries targeting it, stacked
    /// over the scrollback terminal on a terminal pane and standing alone on
    /// a widgets-only pane. Targets match by the interned pane name id, so a
    /// closed-and-recreated same-name pane re-attaches its widgets.
    pub fn script_pane_body(&self, key: PaneKey) -> Element<'_, Message> {
        let body: Element<'_, Message> = match self.panes.get(&key) {
            Some(pane) => {
                let name_id = pane.def.name_id.as_u32();
                let widgets = self.widget_stack(move |target| target == Some(name_id));
                match pane.buffer.as_ref() {
                    Some(buffer) => stack![
                        split_terminal_pane::split_terminal_pane(
                            buffer.borrow(),
                            pane.selection.clone(),
                            self.link_handler(),
                        ),
                        widgets
                    ]
                    .into(),
                    None => widgets,
                }
            }
            None => iced::widget::Space::new().into(),
        };
        container(body)
            .padding(Padding {
                top: 6.0,
                right: 10.0,
                bottom: 10.0,
                left: 10.0,
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// Sends the runtime its shutdown signal. Explicit — deliberately not
    /// `Drop`-driven — so the session struct can move between containers
    /// without ever firing a stray shutdown; the daemon calls this exactly
    /// once, when the session closes (user close or window-close cascade).
    /// Taking the channel makes a repeat call structurally a no-op. A session
    /// closed before its runtime ever reported ready has no channel yet; its
    /// runtime notices the dropped event stream instead.
    fn shutdown(&mut self) {
        if let Some(tx) = self.runtime_tx.take() {
            tx.send(RuntimeAction::Shutdown).ok();
        }
    }

    /// The link-click handler this session's terminal panes call: a command link
    /// sends on THIS session (the one whose pane was clicked); a callback link is
    /// addressed to the session/isolate that minted it — sent here too, and the
    /// dispatch arm forwards it home when that is another session. Server-sent
    /// links (OSC 8: browser URLs and `send:` commands) pass the per-server
    /// trust gate first — ungranted ones stage the confirm dialog instead of
    /// acting. `None` until the runtime is ready (links echoed that early
    /// cannot exist anyway).
    fn link_handler(&self) -> Option<Rc<dyn Fn(LinkClickEvent)>> {
        let tx = self.runtime_tx.clone()?;
        let server_config = self.server_config.clone();
        let pending = self.pending_link_confirm.clone();
        Some(Rc::new(move |event: LinkClickEvent| {
            let action = match event.action {
                LinkAction::Send(command) => RuntimeAction::Send(Arc::new(command.to_string())),
                LinkAction::Callback {
                    session,
                    isolate_token,
                    id,
                } => {
                    let (isolate, instance) = IsolateId::from_widget_token(&isolate_token);
                    RuntimeAction::InvokeLinkCallback {
                        session,
                        isolate,
                        instance,
                        id,
                        shift: event.shift,
                        ctrl: event.ctrl,
                        alt: event.alt,
                    }
                }
                LinkAction::OpenUrl(url) => {
                    let host = link_url_host(&url);
                    if server_config.borrow().allows_server_link(host.as_deref()) {
                        open_url_in_browser(&url);
                    } else {
                        *pending.borrow_mut() = Some(PendingLinkConfirm {
                            display: url.to_string(),
                            action: LinkAction::OpenUrl(url),
                            host,
                            grant_host: false,
                            grant_server: false,
                        });
                    }
                    return;
                }
                LinkAction::ServerSend(command) => {
                    if server_config.borrow().allows_server_link(None) {
                        RuntimeAction::Send(Arc::new(command.to_string()))
                    } else {
                        *pending.borrow_mut() = Some(PendingLinkConfirm {
                            display: command.to_string(),
                            action: LinkAction::ServerSend(command),
                            host: None,
                            grant_host: false,
                            grant_server: false,
                        });
                        return;
                    }
                }
            };
            if let Err(e) = tx.send(action) {
                log::error!("Failed to send link action to session runtime: {e}");
            }
        }))
    }

    /// Perform a confirmed (or pre-granted) server link.
    fn perform_server_link(&self, action: &LinkAction) {
        match action {
            LinkAction::OpenUrl(url) => open_url_in_browser(url),
            LinkAction::ServerSend(command) => {
                self.send_runtime_action(RuntimeAction::Send(Arc::new(command.to_string())));
            }
            // Script links never pass through the trust gate.
            LinkAction::Send(_) | LinkAction::Callback { .. } => {}
        }
    }

    /// Send an action to the session runtime, logging instead of panicking if
    /// the runtime is gone or not yet ready. A session's runtime thread can die
    /// independently of the UI (its own panic, shutdown teardown), so a closed
    /// channel is a per-session condition to survive, never an app-wide abort.
    fn send_runtime_action(&self, action: RuntimeAction) {
        match &self.runtime_tx {
            Some(tx) => {
                if let Err(e) = tx.send(action) {
                    log::error!(
                        "Session {}: failed to send action to session runtime: {e}",
                        self.id
                    );
                }
            }
            None => log::warn!(
                "Session {}: dropping runtime action: session runtime not ready",
                self.id
            ),
        }
    }

    pub fn jsx_subscription(&self) -> Subscription<SessionId> {
        self.widget_root.subscription(self.id)
    }

    pub fn session_subscription(&self) -> Subscription<TaggedSessionEvent> {
        Subscription::run_with(self.session_params.clone(), |params| {
            session::spawn(params.clone())
        })
    }

    /// Handle session-specific messages
    pub fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Close => {
                // Session teardown is the daemon's job (store removal, grid
                // cleanup, runtime shutdown); nothing to do at this level.
                Task::none()
            }
            Message::SetMapperCurrentLocation(area_id, room_number) => {
                self.map_store
                    .set_current_location(area_id, room_number);
                Task::none()
            }
            Message::WidgetMapMessage { id, message } => {
                if let Some(update) = self.map_store.update_map(id, message) {
                    update
                        .task
                        .map(move |inner_message| Message::WidgetMapMessage {
                            id,
                            message: inner_message,
                        })
                } else {
                    Task::none()
                }
            }
            Message::Input(input_msg) => {
                let update = self.input.update(input_msg);

                match update.event {
                    Some(session_input::Event::Submit(command)) => {
                        self.send_runtime_action(RuntimeAction::Send(command));
                    }
                    Some(session_input::Event::HotkeyTriggered(id)) => {
                        self.send_runtime_action(RuntimeAction::ExecHotkey { id });
                    }
                    None => {}
                }

                update.task.map(Message::Input)
            }
            Message::SessionEvent(event) => {
                match event {
                    SessionEvent::RuntimeReady(tx) => {
                        info!("Loading automations for server: {}", self.server_name);
                        for action in session::config::load_automation_actions(&self.server_name) {
                            if let Err(e) = tx.send(action) {
                                log::error!("Failed to send automation to runtime: {}", e);
                            }
                        }

                        // Re-assert current settings: a settings change
                        // committed while the runtime was still starting up
                        // (before this channel existed) would otherwise be
                        // lost — the fan-out drops actions for not-yet-ready
                        // sessions.
                        let settings = load_settings();
                        let mut script_settings = ScriptSettings::from(&settings);
                        script_settings.palette = Some(crate::prefs::script_palette(&settings));
                        if let Err(e) = tx.send(RuntimeAction::ApplySettings {
                            command_separator: Arc::new(settings.command_separator),
                            raw_line_prefix: Arc::new(settings.raw_line_prefix),
                            log_enabled: settings.logging.enabled,
                            script_settings: Box::new(script_settings),
                        }) {
                            log::error!("Failed to send settings to runtime: {e}");
                        }

                        // A runtime reload re-emits `RuntimeReady`, so distinguish
                        // the very first readiness (before the channel is stored)
                        // from later reloads to keep the offline hint one-time.
                        let first_ready = self.runtime_tx.is_none();
                        self.runtime_tx = Some(tx);

                        if self.connected || !self.auto_connect {
                            // Already connected (e.g. a runtime reload preserved the
                            // socket), or opened offline — don't auto-connect. Orient
                            // the user the first time a fresh offline session comes up.
                            if first_ready && !self.auto_connect {
                                self.send_runtime_action(RuntimeAction::Echo(Arc::new(
                                    "Opened offline. Press Connect to go online.".to_string(),
                                )));
                            }
                            Task::none()
                        } else {
                            Task::done(Message::Reconnect)
                        }
                    }

                    SessionEvent::UpdateBuffer(buffer_updates) => {
                        for update in buffer_updates.iter() {
                            match update {
                                BufferUpdate::EnsureNewLine => {
                                    self.terminal_buffer.borrow_mut().commit_current_line();
                                }
                                BufferUpdate::Append(line) => {
                                    self.terminal_buffer.borrow_mut().extend_line(line.clone());
                                }
                                BufferUpdate::AppendTo(key, line) => {
                                    // Core validates sinks against the live registry when it
                                    // queues, and `PaneClosed` travels behind any updates that
                                    // preceded it — so a miss here is a bug: warn and drop,
                                    // never fall back to main (a raw main append would desync
                                    // the numbering parity permanently).
                                    match self.panes.get(key).and_then(|pane| pane.buffer.as_ref()) {
                                        Some(buffer) => {
                                            let mut buffer = buffer.borrow_mut();
                                            // Whole-line delivery: start a fresh line, commit it.
                                            buffer.extend_line(line.clone());
                                            buffer.commit_current_line();
                                        }
                                        None => log::warn!(
                                            "Dropping AppendTo for unknown or bufferless {key}"
                                        ),
                                    }
                                }
                                BufferUpdate::RetractOpenLine => {
                                    self.terminal_buffer.borrow_mut().retract_open_line();
                                }
                                BufferUpdate::Clear(key) => {
                                    if *key == MAIN_PANE_KEY {
                                        self.terminal_buffer.borrow_mut().clear_lines();
                                    } else if let Some(buffer) =
                                        self.panes.get(key).and_then(|pane| pane.buffer.as_ref())
                                    {
                                        buffer.borrow_mut().clear_lines();
                                    } else {
                                        log::warn!("Dropping Clear for unknown {key}");
                                    }
                                }
                            }
                        }
                        Task::none()
                    }
                    SessionEvent::PaneOpened { def, .. } => {
                        // Placement into a window grid is the daemon's job (it
                        // intercepts this event before delegating here); this
                        // side only materializes the display state.
                        self.open_pane(def);
                        Task::none()
                    }
                    SessionEvent::PaneClosed(key) => {
                        self.panes.remove(&key);
                        Task::none()
                    }
                    SessionEvent::PaneUpdated(def) => {
                        // An in-place def change (title-bar policy). Main has
                        // no PaneDisplay entry; its policy mirrors into the
                        // dedicated field the view reads.
                        if def.is_main {
                            self.main_title_bar = def.title_bar;
                        } else if let Some(pane) = self.panes.get_mut(&def.key) {
                            pane.def = def;
                        }
                        Task::none()
                    }
                    SessionEvent::ClearHotkeys => {
                        self.input.clear_hotkeys();
                        Task::none()
                    }
                    SessionEvent::RegisterHotkey(name, hotkey) => {
                        self.input.register_hotkey(name, hotkey);
                        Task::none()
                    }
                    SessionEvent::UnregisterHotkey(name) => {
                        self.input.unregister_hotkey(&name);
                        Task::none()
                    }
                    SessionEvent::PerformLineOperation {
                        line_number,
                        operation,
                    } => {
                        self.terminal_buffer
                            .borrow_mut()
                            .perform_line_operation(line_number, operation);
                        Task::none()
                    }
                    SessionEvent::SetCurrentLocation(area_id, room_number) => {
                        Task::done(Message::SetMapperCurrentLocation(area_id, room_number))
                    }
                    // Per-server map-scope evidence — handled by the daemon
                    // (which owns the scope store) before this forward; the
                    // session store has nothing to do with them.
                    SessionEvent::MapperNavigated(_)
                    | SessionEvent::OfferMapRescue { .. }
                    | SessionEvent::MapAreaCreated(_) => Task::none(),
                    SessionEvent::Connected => {
                        self.connected = true;
                        self.ever_connected = true;
                        Task::none()
                    }
                    SessionEvent::Disconnected => {
                        self.connected = false;
                        Task::none()
                    }
                    SessionEvent::StoreBindingsChanged => {
                        // Pure repaint wake: bound widget props read their store cells
                        // lock-free inside the render closures, so there is no state to
                        // update here — processing the message redraws the view.
                        Task::none()
                    }
                }
            }
            Message::Reload => {
                self.input.clear_hotkeys();
                if let Some(tx) = self.runtime_tx.as_ref() { tx.send(RuntimeAction::Reload).ok(); }
                Task::none()
            }
            Message::Reconnect => {
                info!("Connecting to server");
                // An explicit connect marks online intent, so a later reload
                // restores the connection like any normal session.
                self.auto_connect = true;
                match session::config::load_connect_action(&self.server_name, &self.profile_name) {
                    Ok(action) => self.send_runtime_action(action),
                    Err(e) => log::error!("Failed to load connection config: {e:?}"),
                }

                Task::none()
            }
            Message::TerminalClicked => {
                // The terminal's release handler already ran (it is the
                // mouse_area's content), so a drag has settled into
                // `Selected` with a non-empty range; a plain click reads as
                // `None` or an empty `Selected`. Only the selection-less
                // click focuses the input — the pre-pane behavior.
                let selected = match &*self.terminal_pane_selection.borrow() {
                    Selection::None => false,
                    Selection::Selected { from, to } => from != to,
                    Selection::Selecting { .. } => true,
                };
                if selected {
                    Task::none()
                } else {
                    operation::focus(self.input.input_id())
                }
            }
            Message::Disconnect => {
                info!("Disconnecting from server");
                // Respect the explicit disconnect: don't let a later reload
                // silently reconnect. The runtime emits `Disconnected` back,
                // which flips `connected` off.
                self.auto_connect = false;
                self.send_runtime_action(RuntimeAction::Disconnect);
                Task::none()
            }
            Message::ApplySettings(settings) => {
                {
                    let mut terminal_buffer = self.terminal_buffer.borrow_mut();
                    terminal_buffer.set_max_lines(scrollback_limit(&settings));
                    terminal_buffer.refresh_styles();
                }
                for pane in self.panes.values() {
                    if let Some(buffer) = pane.buffer.as_ref() {
                        let mut buffer = buffer.borrow_mut();
                        buffer.set_max_lines(scrollback_limit(&settings));
                        buffer.refresh_styles();
                    }
                }

                let mut script_settings = ScriptSettings::from(&settings);
                script_settings.palette = Some(crate::prefs::script_palette(&settings));
                self.send_runtime_action(RuntimeAction::ApplySettings {
                    command_separator: Arc::new(settings.command_separator),
                    raw_line_prefix: Arc::new(settings.raw_line_prefix),
                    log_enabled: settings.logging.enabled,
                    script_settings: Box::new(script_settings),
                });

                Task::none()
            }
            Message::LinkConfirmGrantHost(value) => {
                if let Some(pending) = self.pending_link_confirm.borrow_mut().as_mut() {
                    pending.grant_host = value;
                }
                Task::none()
            }
            Message::LinkConfirmGrantServer(value) => {
                if let Some(pending) = self.pending_link_confirm.borrow_mut().as_mut() {
                    pending.grant_server = value;
                }
                Task::none()
            }
            Message::LinkConfirmCancel => {
                self.pending_link_confirm.borrow_mut().take();
                Task::none()
            }
            Message::LinkConfirmProceed => {
                let pending = self.pending_link_confirm.borrow_mut().take();
                if let Some(pending) = pending {
                    if pending.grant_host || pending.grant_server {
                        // Persist by re-reading the on-disk config and applying
                        // only this grant, so a concurrent session's grant or an
                        // address edit made since this session opened is not
                        // clobbered by writing back a stale whole-config
                        // snapshot. Fall back to our in-memory copy if the load
                        // fails. The in-memory copy is updated to match either
                        // way, so the gate reflects the grant immediately.
                        let mut config = load_server(&self.server_name)
                            .map_or_else(|_| self.server_config.borrow().clone(), |s| s.config);
                        if pending.grant_server {
                            config.trust_all_links = true;
                        }
                        if pending.grant_host
                            && let Some(host) = &pending.host
                            && !config
                                .trusted_link_hosts
                                .iter()
                                .any(|t| t.eq_ignore_ascii_case(host))
                        {
                            config.trusted_link_hosts.push(host.clone());
                        }
                        if let Err(e) = update_server(&self.server_name, config.clone()) {
                            log::error!(
                                "Failed to persist link-trust grants for '{}': {e}",
                                self.server_name
                            );
                        }
                        *self.server_config.borrow_mut() = config;
                    }
                    self.perform_server_link(&pending.action);
                }
                Task::none()
            }
            Message::DismissBindToast(generation) => {
                // Only clear if this is the toast the timer was scheduled for; a
                // newer bind may have replaced it (bumping the generation).
                if generation == self.toast_gen {
                    self.toast = None;
                }
                Task::none()
            }
            Message::BindToastActionClicked => {
                // The daemon owns the scope store, so it intercepts this in its
                // `SessionAction` handler (reading the staged action, applying it,
                // and clearing the toast) before this arm is ever reached. Clear
                // defensively in case that interception is bypassed.
                self.toast = None;
                Task::none()
            }
            Message::None => {
                Task::none()
            }
        }
    }

    /// Show a bind-on-use / rescue toast over this session's main pane and
    /// schedule its timed dismiss. Replaces any current toast (one at a time)
    /// and bumps the generation so the previous toast's timer becomes inert.
    pub fn show_bind_toast(
        &mut self,
        message: String,
        action: Option<(String, ToastAction)>,
    ) -> Task<Message> {
        self.toast_gen += 1;
        let generation = self.toast_gen;
        self.toast = Some(SessionToast { message, action });
        Task::perform(
            async move { tokio::time::sleep(BIND_TOAST_DISMISS).await },
            move |()| Message::DismissBindToast(generation),
        )
    }

    /// Take the staged action of the current toast and clear it — the daemon's
    /// half of an action-button click (it then applies the action to the scope
    /// store). Returns `None` if there is no toast or it is message-only.
    pub fn take_toast_action(&mut self) -> Option<ToastAction> {
        let action = self.toast.take().and_then(|toast| toast.action);
        action.map(|(_, action)| action)
    }

    /// Title-bar content for this session's pane: the profile/server label,
    /// styled as a tab. Deliberately intrinsic-width — pane_grid's drag pick
    /// area is the title bar minus the bounds of the content *and* controls,
    /// so a fill-width header would leave nothing to drag the pane by.
    pub fn title_content(&self, is_active: bool) -> Element<'_, Message> {
        let title = text(format!("{} ({})", self.profile_name, self.server_name))
            .size(13)
            .color(Color::from_rgba8(
                255,
                250,
                239,
                if is_active { 1.0 } else { 0.45 },
            ));

        container(title)
            .padding(Padding {
                top: 5.0,
                right: 12.0,
                bottom: 5.0,
                left: 12.0,
            })
            .style(move |_: &crate::Theme| container::Style {
                background: Some(
                    Color::from_rgba8(255, 255, 255, if is_active { 0.08 } else { 0.03 }).into(),
                ),
                border: Border {
                    radius: iced::border::Radius {
                        top_left: 6.0,
                        top_right: 6.0,
                        bottom_right: 0.0,
                        bottom_left: 0.0,
                    },
                    ..Border::default()
                },
                ..Default::default()
            })
            .into()
    }

    /// Title-bar controls: the connection toggle and session close. pane_grid
    /// renders controls outside the drag pick area, so these stay plain
    /// clicks even mid-bar.
    pub fn title_controls(&self) -> Element<'_, Message> {
        // The connection control: Disconnect when live, otherwise Reconnect
        // (was connected before) or Connect (opened offline, never connected).
        let (conn_label, conn_message) = if self.connected {
            ("Disconnect", Message::Disconnect)
        } else if self.ever_connected {
            ("Reconnect", Message::Reconnect)
        } else {
            ("Connect", Message::Reconnect)
        };

        let connection_button = button(text(conn_label).size(12))
            .style(smudgy_theme::builtins::button::subtle)
            .padding([2, 10])
            .on_press(conn_message);

        let close_button =
            title_bar_icon_button(crate::assets::hero_icons::X_MARK.clone(), Message::Close);

        row![connection_button, close_button]
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
    }

    /// The pane body under the title bar: the terminal (with the
    /// script-widget overlay stacked over it) above the command input.
    /// Activation on click is handled by the parent grid's `on_click`.
    pub fn pane_body(&self) -> Element<'_, Message> {
        // A click released on the terminal focuses the session input (unless
        // it made a selection — decided in the handler). Wrapping only the
        // terminal layer keeps overlay widgets out of it: the stack delivers
        // pointer events top-layer-first, and an interactive overlay widget
        // under the cursor captures or levitates them away before this layer.
        let terminal = mouse_area(split_terminal_pane::split_terminal_pane(
            self.terminal_buffer.borrow(),
            self.terminal_pane_selection.clone(),
            self.link_handler(),
        ))
        .on_release(Message::TerminalClicked);

        // The main pane hosts the untargeted overlay entries plus anything
        // explicitly targeting "main" (name id 0 in every registry).
        let main_id = MAIN_PANE_NAME_ID.as_u32();
        let widgets = self.widget_stack(move |target| target.is_none() || target == Some(main_id));

        let terminal_area = stack![terminal, widgets];

        // Map input messages to session messages
        let input = self.input.view().map(Message::Input);

        let body: Element<'_, Message> = column![terminal_area, input]
            .spacing(10)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();

        // A bind-on-use / rescue toast floats over the body as a bottom pill,
        // below any modal link dialog (which stacks last, so it stays on top).
        let body = match &self.toast {
            Some(toast) => stack![body, bind_toast_overlay(toast)].into(),
            None => body,
        };

        // A server link held at the trust gate renders its confirm dialog over
        // the whole session body — terminal *and* input — so it is truly modal
        // for this session (other sessions stay interactive).
        let body = match self.pending_link_confirm.borrow().as_ref() {
            Some(pending) => stack![body, self.link_confirm_dialog(pending)].into(),
            None => body,
        };

        container(body)
            .padding(Padding {
                top: 6.0,
                right: 10.0,
                bottom: 10.0,
                left: 10.0,
            })
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }

    /// The trust-gate dialog for a server-sent link: the verbatim destination
    /// (never server-relabelable), a perform/cancel pair, and the two opt-in
    /// grants — per-host (URL links only) and trust-everything-from-this-
    /// server. Clicking the dimmed backdrop cancels.
    fn link_confirm_dialog(&self, pending: &PendingLinkConfirm) -> Element<'static, Message> {
        let (title, verb) = match &pending.action {
            LinkAction::OpenUrl(_) => ("The server wants to open a link in your browser", "Open"),
            _ => ("The server wants to send a command as you", "Send"),
        };

        // A server can make the URL/command up to the OSC 8 URI cap (8 KiB);
        // middle-elide so a long unbroken token can't overflow the card and
        // push the buttons off-screen. The user still sees both ends — enough
        // to judge the destination.
        let display = elide_middle(&pending.display, 180);

        let mut body = column![
            text(title).size(15),
            container(text(display).size(13))
                .padding(8)
                .width(Length::Fill)
                .style(|_: &crate::Theme| container::Style {
                    background: Some(Color::from_rgba8(0, 0, 0, 0.35).into()),
                    border: Border {
                        radius: 4.0.into(),
                        ..Border::default()
                    },
                    ..Default::default()
                }),
        ]
        .spacing(12);

        if let Some(host) = &pending.host {
            body = body.push(
                checkbox(pending.grant_host)
                    .label(format!("Always allow links to {host}"))
                    .on_toggle(Message::LinkConfirmGrantHost)
                    .size(15)
                    .text_size(13),
            );
        }
        body = body.push(
            checkbox(pending.grant_server)
                .label("Always trust links from this server")
                .on_toggle(Message::LinkConfirmGrantServer)
                .size(15)
                .text_size(13),
        );
        body = body.push(
            row![
                space::horizontal(),
                button(text("Cancel").size(13))
                    .style(smudgy_theme::builtins::button::subtle)
                    .padding([4, 14])
                    .on_press(Message::LinkConfirmCancel),
                button(text(verb).size(13))
                    .padding([4, 14])
                    .on_press(Message::LinkConfirmProceed),
            ]
            .spacing(8)
            .align_y(Alignment::Center),
        );

        let card = container(body)
            .padding(16)
            .max_width(560)
            .style(|_: &crate::Theme| container::Style {
                background: Some(Color::from_rgba8(32, 32, 38, 1.0).into()),
                border: Border {
                    radius: 8.0.into(),
                    color: Color::from_rgba8(255, 255, 255, 0.12),
                    width: 1.0,
                },
                ..Default::default()
            });

        opaque(
            mouse_area(
                center(opaque(card)).style(|_: &crate::Theme| container::Style {
                    background: Some(Color::from_rgba8(0, 0, 0, 0.55).into()),
                    ..Default::default()
                }),
            )
            .on_press(Message::LinkConfirmCancel),
        )
    }

    /// The stack of this session's script widgets whose pane target passes
    /// `filter` (each entry's interned pane name id; `None` = the untargeted
    /// main overlay), with widget interactions routed back to the creating
    /// isolate.
    fn widget_stack(
        &self,
        filter: impl Fn(Option<u32>) -> bool,
    ) -> Element<'_, Message> {
        with_store_context(&self.map_store, || {
            with_text_store_context(&self.text_store, || {
                self.widget_root.view(filter, || Box::new(default))
            })
        })
        .map(|widget_message| match widget_message {
            smudgy_widgets::WidgetMessage::InvokeCallback {
                callback,
                isolate,
                args,
            } => {
                let (isolate, instance) = IsolateId::from_widget_token(&isolate.0);
                self.send_runtime_action(RuntimeAction::ExecuteJavascriptFunction {
                    isolate,
                    instance,
                    function: callback,
                    args,
                });
                Message::None
            }
            smudgy_widgets::WidgetMessage::Noop => Message::None,
            // Apply the edit to the editor's buffer (UI-thread store), and on a real text change
            // fire the script's `onChange` with the buffer's new full text via the creating isolate.
            smudgy_widgets::WidgetMessage::TextEditorAction {
                key,
                action,
                on_change,
                isolate,
            } => {
                if let Some(text) = self.text_store.perform(&key, action)
                    && let Some(callback) = on_change
                {
                    let (isolate, instance) = IsolateId::from_widget_token(&isolate.0);
                    self.send_runtime_action(RuntimeAction::ExecuteJavascriptFunction {
                        isolate,
                        instance,
                        function: callback,
                        args: vec![text],
                    });
                }
                Message::None
            }
            smudgy_widgets::WidgetMessage::MapMessage { id, message } => {
                Message::WidgetMapMessage { id, message }
            }
        })
    }

    fn map_cache_dir() -> PathBuf {
        get_smudgy_home()
            .map(|dir| dir.join("maps"))
            .unwrap_or_else(|_| std::env::temp_dir().join("smudgy").join("maps"))
    }

    /// Authoritative, never-purged on-disk store for this server's local maps.
    /// Distinct from [`Self::map_cache_dir`] (the viewer-namespaced,
    /// `/sync`-purged cloud cache) — the local tier owns these bytes. Per-server
    /// under `<home>/<server>/local/`, alongside the server's aliases/modules/logs.
    fn local_map_dir(server_name: &str) -> PathBuf {
        get_smudgy_home()
            .map(|dir| dir.join(server_name).join("local"))
            .unwrap_or_else(|_| {
                std::env::temp_dir()
                    .join("smudgy")
                    .join(server_name)
                    .join("local")
            })
    }

}

/// One-shot migration of the pre-0.4.1 global local-map store (`<home>/local/`)
/// into every server's per-server store (`<home>/<server>/local/`). Legacy maps
/// carry no game identity, so the only lossless split is to give every server
/// the full set (verbatim file copies preserve ids, keeping in-set exits
/// linked) and let the user prune per server. The legacy dir is deleted once
/// every copy has landed — its absence is what makes the migration one-shot,
/// so no per-server sentinel is needed.
///
/// Fail-safe by construction: a destination file that already exists is never
/// overwritten (a retry after a partial failure must not clobber maps edited
/// since), and the legacy dir survives any failed copy so the next launch
/// retries. With no valid servers to receive the maps, nothing happens. A
/// `<home>/local/` holding a `server.json` is not the legacy store but a
/// server the user named "local", and is left alone.
///
/// Runs at app startup, before any session or map editor opens a
/// [`LocalBackend`] over the per-server dirs.
pub fn migrate_legacy_global_local_maps() {
    let Ok(home) = get_smudgy_home() else {
        return;
    };
    let servers = match smudgy_core::models::server::list_servers() {
        Ok(servers) => servers,
        Err(err) => {
            log::warn!("local map migration: listing servers failed: {err}");
            return;
        }
    };
    let dests: Vec<PathBuf> = servers.iter().map(|s| s.path.join("local")).collect();
    migrate_global_local_maps(&home.join("local"), &dests);
}

/// The path-explicit core of [`migrate_legacy_global_local_maps`], factored out
/// so the copy/cleanup semantics are unit-testable without the process-global
/// home.
fn migrate_global_local_maps(legacy: &Path, server_local_dirs: &[PathBuf]) {
    if !legacy.is_dir() || legacy.join("server.json").exists() {
        return;
    }
    if server_local_dirs.is_empty() {
        // No destination can receive the maps yet; keep the store for a launch
        // where one exists.
        return;
    }

    let mut complete = true;
    for dir in server_local_dirs {
        if dir.starts_with(legacy) {
            continue;
        }
        // Sweep the pre-release `.seeded-from-global` sentinel (an earlier,
        // never-shipped form of this migration); inert but untidy.
        let _ = std::fs::remove_file(dir.join(".seeded-from-global"));
        for sub in ["areas", "atlases"] {
            let from = legacy.join(sub);
            if !from.is_dir() {
                continue;
            }
            let entries = match std::fs::read_dir(&from) {
                Ok(entries) => entries,
                Err(err) => {
                    log::warn!("local map migration: read {} failed: {err}", from.display());
                    complete = false;
                    continue;
                }
            };
            let to = dir.join(sub);
            if let Err(err) = std::fs::create_dir_all(&to) {
                log::warn!("local map migration: mkdir {} failed: {err}", to.display());
                complete = false;
                continue;
            }
            for entry in entries.flatten() {
                let path = entry.path();
                let is_json = path
                    .extension()
                    .is_some_and(|ext| ext.eq_ignore_ascii_case("json"));
                if !is_json {
                    continue;
                }
                let dest = to.join(entry.file_name());
                if dest.exists() {
                    continue;
                }
                if let Err(err) = std::fs::copy(&path, &dest) {
                    log::warn!(
                        "local map migration: copy {} -> {} failed: {err}",
                        path.display(),
                        dest.display()
                    );
                    complete = false;
                }
            }
        }
    }

    if complete
        && let Err(err) = std::fs::remove_dir_all(legacy)
    {
        log::warn!(
            "local map migration: removing migrated store {} failed: {err}",
            legacy.display()
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn atlas_target(n: u128) -> BindTarget {
        BindTarget::Atlas(AtlasId(smudgy_cloud::Uuid::from_u128(n)))
    }

    /// The streak binds only after `LOCATE_BIND_STREAK` *consecutive*
    /// unassigned locates into the same target; a different target or a
    /// non-unassigned locate resets it.
    #[test]
    fn locate_streak_binds_only_when_sustained() {
        let mut tracker = BindTracker::default();
        let target = atlas_target(1);

        // One short of the threshold: no bind yet.
        for _ in 0..LOCATE_BIND_STREAK - 1 {
            assert!(!tracker.observe_locate(target, true));
        }
        // The threshold locate binds.
        assert!(tracker.observe_locate(target, true));
        // The streak is consumed: a fresh one must re-accrue.
        assert!(!tracker.observe_locate(target, true));
    }

    #[test]
    fn a_different_target_resets_the_streak() {
        let mut tracker = BindTracker::default();
        let a = atlas_target(1);
        let b = atlas_target(2);
        for _ in 0..LOCATE_BIND_STREAK - 1 {
            assert!(!tracker.observe_locate(a, true));
        }
        // A locate into a different unassigned target restarts the count.
        assert!(!tracker.observe_locate(b, true));
        // So `a` needs the full run again — one more `a` is not enough.
        assert!(!tracker.observe_locate(a, true));
    }

    #[test]
    fn a_non_unassigned_locate_resets_the_streak() {
        let mut tracker = BindTracker::default();
        let target = atlas_target(1);
        for _ in 0..LOCATE_BIND_STREAK - 1 {
            assert!(!tracker.observe_locate(target, true));
        }
        // A Here/Elsewhere (unassigned == false) locate breaks the streak.
        assert!(!tracker.observe_locate(target, false));
        // The next unassigned locate is only streak length 1.
        assert!(!tracker.observe_locate(target, true));
    }

    #[test]
    fn navigation_binds_immediately_but_respects_suppression() {
        let tracker_target = atlas_target(1);
        let tracker = BindTracker::default();
        // One unassigned navigation is enough.
        assert!(tracker.observe_navigation(tracker_target, true));
        // A non-unassigned target never binds.
        assert!(!tracker.observe_navigation(tracker_target, false));
    }

    #[test]
    fn undo_suppresses_rebinding_for_the_session() {
        let mut tracker = BindTracker::default();
        let target = atlas_target(1);
        tracker.suppress(target);
        // Neither signal re-binds a suppressed target, however many locates.
        for _ in 0..LOCATE_BIND_STREAK * 2 {
            assert!(!tracker.observe_locate(target, true));
        }
        assert!(!tracker.observe_navigation(target, true));
        // A different target is unaffected.
        assert!(tracker.observe_navigation(atlas_target(2), true));
    }

    #[test]
    fn rescue_offer_fires_at_most_once_per_target() {
        let mut tracker = BindTracker::default();
        let target = atlas_target(1);
        assert!(tracker.mark_rescue_offered(target), "first offer");
        assert!(!tracker.mark_rescue_offered(target), "not offered again");
        assert!(tracker.mark_rescue_offered(atlas_target(2)), "a different target still offers");
    }

    fn write(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
    }

    fn temp_root(tag: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "smudgy-migrate-{tag}-{:?}",
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap()
        ))
    }

    /// The full split: every server receives every legacy area/atlas json,
    /// non-json strays are not copied, pre-release sentinels are swept, a
    /// destination file that already exists is preserved verbatim, and the
    /// legacy store is deleted afterward.
    #[test]
    fn migration_splits_into_every_server_and_cleans_up() {
        let root = temp_root("split");
        let legacy = root.join("local");
        let aard = root.join("Aardwolf").join("local");
        let achaea = root.join("Achaea").join("local");
        write(&legacy.join("areas").join("a1.json"), "{}");
        write(&legacy.join("areas").join("a2.json"), "{}");
        write(&legacy.join("atlases").join("f1.json"), "{}");
        // A stray non-json file must not be copied.
        write(&legacy.join("areas").join("notes.txt"), "ignore me");
        // Aardwolf already has its own a1 (edited post-split retry, say) and a
        // sentinel from the pre-release per-server seed design.
        write(&aard.join("areas").join("a1.json"), r#"{"mine":true}"#);
        write(&aard.join(".seeded-from-global"), "");

        migrate_global_local_maps(&legacy, &[aard.clone(), achaea.clone()]);

        for server in [&aard, &achaea] {
            assert!(server.join("areas").join("a2.json").exists());
            assert!(server.join("atlases").join("f1.json").exists());
            assert!(!server.join("areas").join("notes.txt").exists());
            assert!(!server.join(".seeded-from-global").exists());
        }
        assert_eq!(
            std::fs::read_to_string(aard.join("areas").join("a1.json")).unwrap(),
            r#"{"mine":true}"#,
            "an existing destination file is never overwritten"
        );
        assert!(achaea.join("areas").join("a1.json").exists());
        assert!(!legacy.exists(), "the migrated store is cleaned up");

        std::fs::remove_dir_all(&root).ok();
    }

    /// A failed copy keeps the legacy store for a retry on the next launch;
    /// once the obstruction is gone the retry completes and cleans up.
    #[test]
    fn migration_keeps_legacy_store_until_every_copy_lands() {
        let root = temp_root("failsafe");
        let legacy = root.join("local");
        let server = root.join("Aardwolf").join("local");
        write(&legacy.join("areas").join("a1.json"), "{}");
        // A file where the areas/ subdir must go makes create_dir_all fail.
        write(&server.join("areas"), "obstruction");

        migrate_global_local_maps(&legacy, &[server.clone()]);
        assert!(legacy.exists(), "a failed copy must not trigger cleanup");

        std::fs::remove_file(server.join("areas")).unwrap();
        migrate_global_local_maps(&legacy, &[server.clone()]);
        assert!(server.join("areas").join("a1.json").exists());
        assert!(!legacy.exists());

        std::fs::remove_dir_all(&root).ok();
    }

    /// No-op guards: a missing legacy store, an empty server list (nothing can
    /// receive the maps), and a `<home>/local/` that is actually a server named
    /// "local" all leave the world untouched.
    #[test]
    fn migration_noop_guards() {
        let root = temp_root("guards");
        let legacy = root.join("local");
        let server = root.join("Achaea").join("local");

        // Missing legacy store: nothing is created.
        migrate_global_local_maps(&legacy, &[server.clone()]);
        assert!(!server.exists());

        // No servers: the store survives to migrate on a later launch.
        write(&legacy.join("areas").join("a1.json"), "{}");
        migrate_global_local_maps(&legacy, &[]);
        assert!(legacy.join("areas").join("a1.json").exists());

        // A server the user named "local" is not the legacy store.
        write(&legacy.join("server.json"), "{}");
        migrate_global_local_maps(&legacy, &[server.clone()]);
        assert!(!server.exists());
        assert!(legacy.join("areas").join("a1.json").exists());

        std::fs::remove_dir_all(&root).ok();
    }
}
