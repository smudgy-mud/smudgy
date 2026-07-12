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
use iced::widget::{button, column, container, mouse_area, operation, row, stack, svg, text};
use iced::{Alignment, Border, Color, Length, Padding, Subscription, Task};
use smudgy_widgets::{
    MapStore, MapWidgetId, TextEditorStore, WidgetRoot, with_store_context, with_text_store_context,
};
use log::info;
use smudgy_core::get_smudgy_home;
use smudgy_core::models::profile::load_profile;
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
    AreaId, CachedCloudMapper, CloudMapper, CompositeBackend, CredentialSource, LocalBackend,
    Mapper, PackageApiClient,
};
use smudgy_map_widget::map_view;
use smudgy_theme::builtins::container::default;
use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::path::PathBuf;
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
        let local_map_dir = Self::local_map_dir();

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
    /// dispatch arm forwards it home when that is another session. `None` until the
    /// runtime is ready (links echoed that early cannot exist anyway).
    fn link_handler(&self) -> Option<Rc<dyn Fn(LinkClickEvent)>> {
        let tx = self.runtime_tx.clone()?;
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
            };
            if let Err(e) = tx.send(action) {
                log::error!("Failed to send link action to session runtime: {e}");
            }
        }))
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
            Message::None => {
                Task::none()
            }
        }
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

        container(
            column![terminal_area, input]
                .spacing(10)
                .width(Length::Fill)
                .height(Length::Fill),
        )
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

    /// Authoritative, never-purged on-disk store for local maps. Distinct
    /// from [`Self::map_cache_dir`] (the viewer-namespaced, `/sync`-purged
    /// cloud cache) — the local tier owns these bytes.
    fn local_map_dir() -> PathBuf {
        get_smudgy_home()
            .map(|dir| dir.join("local"))
            .unwrap_or_else(|_| std::env::temp_dir().join("smudgy").join("local"))
    }
}
