//! Browser single-window composition using Smudgy's shared iced surfaces.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, HashMap};
use std::num::NonZeroUsize;
use std::rc::Rc;
use std::sync::Arc;

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{
    PaneGrid, button, center, column, container, mouse_area, opaque, pane_grid, row, stack, text,
};
use iced::{Element, Event as IcedEvent, Fill, Point, Rectangle, Size, Task, keyboard, mouse};
use smudgy_engine::ConnectionState;
use smudgy_session_model::StyledLine;
use smudgy_session_model::input_policy::CommandSyntax;
use smudgy_session_model::pane::{
    MAIN_PANE_KEY, PaneKey, PanePlacement, PaneRef, SplitDirection, TabPosition,
};
use smudgy_session_model::system_row::{Severity, SystemRow};
use smudgy_session_model::workspace as dto;
use smudgy_session_model::{DEFAULT_SCROLLBACK_LINES, InputOp, SessionId};
use smudgy_theme::{Theme, builtins};

use crate::{connect_manager, wire::SessionUpdate};
use smudgy_ui_shared::{
    assets::GEIST,
    bounds_probe::BoundsProbe,
    connect_modal, crt_cat, drag_overlay, main_toolbar, pane_drag,
    pane_groups::{self, Blueprint, EdgeTarget, GroupLayout, SplitSizing, Tab, TabId},
    pane_workspace, session_input,
    settings_appearance::{self, Appearance, Change as AppearanceChange},
    settings_input::{self, Change as InputChange, InputPreferences},
    settings_theme,
    split_terminal_pane::{ScrolledLayout, TerminalViewHandle, split_terminal_pane},
    tab_host::TabHost,
    tab_press, tab_strip,
    terminal_buffer::TerminalBuffer,
    update::Update,
    workspace_snapshot::{self, PaneRecord},
};

/// Host callback for a pane's quantized character-grid dimensions.
pub type GridChangeHandler = Rc<dyn Fn(SessionId, u16, u16)>;

#[derive(Debug, Clone)]
pub enum BrowserPaneEvent {
    Opened {
        key: PaneKey,
        name: String,
        hidden: bool,
        placement: PanePlacement,
    },
    Updated {
        key: PaneKey,
        hidden: bool,
    },
    Echo {
        key: PaneKey,
        text: String,
    },
    Clear(PaneKey),
    Closed(PaneKey),
}

#[derive(Debug, Clone)]
pub enum Message {
    Toolbar(main_toolbar::Message),
    OpenConnect,
    ClosePanel,
    AnimationFrame(iced::time::Instant),
    Disconnect(SessionId),
    Reconnect(SessionId),
    CloseSession(SessionId),
    SessionInput(SessionId, session_input::Message),
    PaneClicked(pane_grid::Pane),
    TabSelected(TabId),
    TabStrip(pane_groups::GroupId, tab_strip::Event),
    DragMoved(Point),
    DragReleased,
    DragCanceled,
    PaneResized(pane_grid::ResizeEvent),
    Manager(connect_manager::Message),
    Appearance(settings_appearance::Message),
    Input(settings_input::Message),
    Syntax(settings_input::SyntaxMessage),
    Theme(settings_theme::Message),
}

/// One ordered presentation update emitted by a session worker.
struct SessionUiState {
    id: SessionId,
    title: String,
    connection: ConnectionState,
    terminal: Rc<RefCell<TerminalBuffer>>,
    terminal_view: TerminalViewHandle,
    session_input: session_input::SessionInput,
    has_live_row: bool,
    server_echo: bool,
    script_panes: Vec<ScriptPaneUiState>,
}

struct ScriptPaneUiState {
    key: PaneKey,
    name: String,
    hidden: bool,
    terminal: Rc<RefCell<TerminalBuffer>>,
    terminal_view: TerminalViewHandle,
}

#[derive(Debug)]
struct BrowserTabDrag {
    tab: TabId,
    slot: PaneRef,
    source_group: pane_groups::GroupId,
    target: Option<pane_drag::ClassifiedTarget>,
}

impl SessionUiState {
    fn new(id: SessionId, endpoint: &str, scrollback_lines: NonZeroUsize) -> Self {
        let terminal = Rc::new(RefCell::new(
            TerminalBuffer::new_with_max_lines_and_capacity(
                scrollback_lines,
                DEFAULT_SCROLLBACK_LINES,
            ),
        ));
        let terminal_view = TerminalViewHandle::default();
        let session_input = session_input::SessionInput::new()
            .with_terminal_buffer(Rc::clone(&terminal))
            .with_terminal_view(terminal_view.clone());
        Self {
            id,
            title: endpoint_title(endpoint),
            connection: ConnectionState::Connecting,
            terminal,
            terminal_view,
            session_input,
            has_live_row: false,
            server_echo: false,
            script_panes: Vec::new(),
        }
    }

    fn apply(&mut self, update: SessionUpdate, mask_input_on_server_echo: bool) -> Task<Message> {
        let just_disconnected = self.connection != ConnectionState::Disconnected
            && update.connection == ConnectionState::Disconnected;
        let mut target = self.terminal.borrow_mut();
        if update.reset {
            target.clear_lines();
            self.has_live_row = false;
        } else if self.has_live_row {
            target.begin_open_line_replacement();
            target.finish_open_line_replacement(None);
            self.has_live_row = false;
        }
        for line in update.rows {
            target.push_line(Arc::new(line));
        }
        if let Some(line) = update.live {
            target.extend_line(Arc::new(line));
            self.has_live_row = true;
        }
        if just_disconnected && !update.status.is_empty() {
            target.append_system_row(SystemRow::plain_notice(Severity::Info, &update.status));
            self.has_live_row = false;
        }
        drop(target);

        self.connection = update.connection;
        self.server_echo = update.server_echo;
        let id = self.id;
        self.session_input
            .set_telnet_mask(self.server_echo && mask_input_on_server_echo)
            .map_message(move |message| Message::SessionInput(id, message))
            .task
    }
}

/// The single-window browser shell. Every session owns its own presentation
/// buffer and input; the browser host pairs it with one dedicated worker.
pub struct ClientUiState {
    pub manager: connect_manager::State,
    pane_layout: GroupLayout<PaneRef>,
    pane_grid: Option<pane_grid::State<pane_groups::GroupId>>,
    split_targets: BTreeMap<pane_grid::Split, EdgeTarget>,
    grid_bounds: Cell<Rectangle>,
    strip_bands: RefCell<HashMap<pane_groups::GroupId, Rectangle>>,
    tab_spans: RefCell<HashMap<TabId, Rectangle>>,
    strip_scroll: RefCell<HashMap<pane_groups::GroupId, f32>>,
    tab_drag: Option<BrowserTabDrag>,
    drag_cursor: Option<Point>,
    toolbar_expanded: bool,
    connect_panel: bool,
    settings_panel: bool,
    appearance: settings_appearance::State,
    input: settings_input::State,
    syntax: settings_input::SyntaxState,
    theme: settings_theme::State,
    cat_clock: crt_cat::Clock,
    sessions: Vec<SessionUiState>,
    active_session: Option<SessionId>,
    next_session: u32,
    grid_change: Option<GridChangeHandler>,
}

impl Default for ClientUiState {
    fn default() -> Self {
        Self::new()
    }
}

impl ClientUiState {
    #[must_use]
    pub fn new() -> Self {
        Self {
            manager: connect_manager::State::default(),
            pane_layout: GroupLayout::new(),
            pane_grid: None,
            split_targets: BTreeMap::new(),
            grid_bounds: Cell::new(Rectangle::new(Point::ORIGIN, Size::ZERO)),
            strip_bands: RefCell::new(HashMap::new()),
            tab_spans: RefCell::new(HashMap::new()),
            strip_scroll: RefCell::new(HashMap::new()),
            tab_drag: None,
            drag_cursor: None,
            toolbar_expanded: true,
            connect_panel: false,
            settings_panel: false,
            appearance: settings_appearance::State::new(Appearance::default()),
            input: settings_input::State::new(InputPreferences::default()),
            syntax: settings_input::SyntaxState::new(CommandSyntax::default()),
            theme: settings_theme::State::new("Smudgy"),
            cat_clock: crt_cat::Clock::default(),
            sessions: Vec::new(),
            active_session: None,
            next_session: 1,
            grid_change: None,
        }
    }

    /// Add a new session cluster to the main-window pane grid.
    ///
    /// # Panics
    ///
    /// Panics if a generated session ID is already present in the grid or the
    /// validated scrollback preference is unexpectedly zero.
    pub fn open_session(&mut self, endpoint: impl Into<String>) -> SessionId {
        let endpoint = endpoint.into();
        let id = SessionId::from(self.next_session);
        self.next_session = self.next_session.saturating_add(1);
        self.pane_layout
            .push_cluster(Tab::bound(PaneRef::main(id)))
            .expect("a fresh session id is unique");
        let limit = NonZeroUsize::new(self.appearance.value().scrollback_lines)
            .expect("validated scrollback preference is nonzero");
        self.sessions
            .push(SessionUiState::new(id, &endpoint, limit));
        self.active_session = Some(id);
        self.rebuild_grid();
        self.connect_panel = false;
        id
    }

    fn rebuild_grid(&mut self) {
        if let Some((configuration, mirror)) =
            self.pane_layout
                .build_filtered(Size::new(1_200.0, 800.0), 4.0, 80.0, |tab| {
                    tab.binding().is_some_and(|pane| !self.pane_hidden(*pane))
                })
        {
            let grid = pane_grid::State::with_configuration(configuration);
            self.split_targets = pane_groups::split_targets(grid.layout(), &mirror);
            self.pane_grid = Some(grid);
        } else {
            self.split_targets.clear();
            self.pane_grid = None;
        }
    }

    pub fn resize_pane(&mut self, event: pane_grid::ResizeEvent) {
        if let Some(grid) = self.pane_grid.as_mut() {
            grid.resize(event.split, event.ratio);
        }
        if let Some(target) = self.split_targets.get(&event.split) {
            self.pane_layout.set_split_ratio(target, event.ratio);
        }
    }

    /// Drop one session's presentation state and collapse its pane group.
    pub fn close_session(&mut self, id: SessionId) -> bool {
        let Some(index) = self.sessions.iter().position(|session| session.id == id) else {
            return false;
        };
        let tabs = self
            .pane_layout
            .panes()
            .into_iter()
            .filter_map(|tab| {
                (tab.binding().is_some_and(|pane| pane.session_id == id)).then_some(tab.id())
            })
            .collect::<Vec<_>>();
        if tabs.is_empty() {
            return false;
        }
        for tab in tabs {
            let _ = self.pane_layout.remove_tab(tab);
        }
        self.sessions.remove(index);
        if self.active_session == Some(id) {
            self.active_session = self.sessions.last().map(|session| session.id);
        }
        self.rebuild_grid();
        true
    }

    #[must_use]
    pub fn active_session(&self) -> Option<SessionId> {
        self.active_session
    }

    pub fn activate_pane(&mut self, pane: pane_grid::Pane) {
        let Some(group) = self.pane_grid.as_ref().and_then(|grid| grid.get(pane)) else {
            return;
        };
        self.active_session = self
            .pane_layout
            .selected(*group)
            .and_then(|tab| self.pane_layout.tab(tab))
            .and_then(Tab::binding)
            .map(|pane| pane.session_id);
    }

    pub fn select_tab(&mut self, tab: TabId) -> bool {
        let Some(session) = self
            .pane_layout
            .tab(tab)
            .and_then(Tab::binding)
            .map(|pane| pane.session_id)
        else {
            return false;
        };
        if !self.pane_layout.select(tab) {
            return false;
        }
        self.active_session = Some(session);
        true
    }

    #[must_use]
    pub fn tab_session(&self, tab: TabId) -> Option<SessionId> {
        self.pane_layout
            .tab(tab)
            .and_then(Tab::binding)
            .map(|pane| pane.session_id)
    }

    #[must_use]
    pub fn drag_active(&self) -> bool {
        self.tab_drag.is_some()
    }

    /// Apply a shared tab-strip interaction. Returns `true` when durable
    /// workspace state changed and the browser host should persist it.
    pub fn handle_tab_strip(
        &mut self,
        group: pane_groups::GroupId,
        event: tab_strip::Event,
    ) -> bool {
        match event {
            tab_strip::Event::Select(tab) => self.select_tab(tab),
            tab_strip::Event::Scrolled(offset) => {
                self.strip_scroll.borrow_mut().insert(group, offset);
                false
            }
            tab_strip::Event::Drag(tab, event) => match event {
                tab_press::Event::DragStarted { point, .. } => {
                    let (Some(slot), Some(source_group)) = (
                        self.pane_layout.tab(tab).and_then(Tab::binding).copied(),
                        self.pane_layout.group_of(tab),
                    ) else {
                        return false;
                    };
                    let point = tab_strip::ground_to_window(
                        point,
                        self.strip_scroll
                            .borrow()
                            .get(&group)
                            .copied()
                            .unwrap_or(0.0),
                    );
                    self.tab_drag = Some(BrowserTabDrag {
                        tab,
                        slot,
                        source_group,
                        target: None,
                    });
                    self.drag_cursor = Some(point);
                    self.update_drag_target(point);
                    false
                }
                tab_press::Event::DragReleased { point: Some(point) } => {
                    let point = tab_strip::ground_to_window(
                        point,
                        self.strip_scroll
                            .borrow()
                            .get(&group)
                            .copied()
                            .unwrap_or(0.0),
                    );
                    self.finish_drag(Some(point))
                }
                tab_press::Event::DragReleased { point: None }
                | tab_press::Event::CaptureLost { dragging: true } => self.finish_drag(None),
                tab_press::Event::Pressed { .. }
                | tab_press::Event::ModifiedPress
                | tab_press::Event::Click
                | tab_press::Event::CaptureLost { dragging: false } => false,
            },
            // These are routed by the browser application because they own
            // workers and session lifetime, not pane presentation state.
            tab_strip::Event::Connect(_)
            | tab_strip::Event::Disconnect(_)
            | tab_strip::Event::CloseSession(_)
            | tab_strip::Event::ToggleVisibility(_) => false,
        }
    }

    pub fn drag_moved(&mut self, point: Point) {
        if self.tab_drag.is_some() {
            self.drag_cursor = Some(point);
            self.update_drag_target(point);
        }
    }

    pub fn release_drag(&mut self) -> bool {
        self.finish_drag(self.drag_cursor)
    }

    pub fn finish_drag(&mut self, point: Option<Point>) -> bool {
        if let Some(point) = point {
            self.update_drag_target(point);
        }
        let Some(drag) = self.tab_drag.take() else {
            return false;
        };
        self.drag_cursor = None;
        if self
            .pane_layout
            .tab(drag.tab)
            .and_then(Tab::binding)
            .copied()
            != Some(drag.slot)
            || self.pane_layout.group_of(drag.tab) != Some(drag.source_group)
        {
            return false;
        }
        let Some(target) = drag.target else {
            return false;
        };
        let swap_partner = match target.action {
            pane_drag::DragAction::Swap { group } => {
                self.pane_layout.effective_selected(group, |tab| {
                    tab.binding().is_some_and(|pane| !self.pane_hidden(*pane))
                })
            }
            _ => None,
        };
        let Some(effect) = pane_workspace::apply_local_drop(
            &mut self.pane_layout,
            drag.tab,
            target.action,
            swap_partner,
        ) else {
            return false;
        };
        let _ = self.pane_layout.select(drag.tab);
        self.active_session = self.tab_session(drag.tab);
        if matches!(effect, pane_workspace::DropEffect::LayoutChanged) {
            self.rebuild_grid();
        }
        true
    }

    fn update_drag_target(&mut self, point: Point) {
        let Some(drag) = self.tab_drag.as_ref() else {
            return;
        };
        let bounds = self.grid_bounds.get();
        if bounds.width <= 0.0 || bounds.height <= 0.0 {
            return;
        }
        let point = Point::new(point.x - bounds.x, point.y - bounds.y);
        let Some(grid) = self.pane_grid.as_ref() else {
            return;
        };
        let regions = grid.layout().pane_regions(4.0, 80.0, bounds.size());
        let bands = self.strip_bands.borrow();
        let spans = self.tab_spans.borrow();
        let scroll = self.strip_scroll.borrow();
        let mut geometries = Vec::with_capacity(regions.len());
        for (pane, group) in &grid.panes {
            let Some(region) = regions.get(pane) else {
                continue;
            };
            let band_height = bands
                .get(group)
                .map_or(pane_drag::DEFAULT_HEADER_BAND, |band| band.height + 4.0);
            let offset = scroll.get(group).copied().unwrap_or(0.0);
            let tabs = self
                .pane_layout
                .tabs(*group)
                .into_iter()
                .flatten()
                .enumerate()
                .filter(|(_, tab)| tab.binding().is_some_and(|pane| !self.pane_hidden(*pane)))
                .filter_map(|(slot, tab)| {
                    spans.get(&tab.id()).map(|span| pane_drag::TabBand {
                        start: span.x - bounds.x - offset,
                        end: span.x - bounds.x + span.width - offset,
                        slot,
                    })
                })
                .collect();
            geometries.push(pane_drag::PaneTargetGeom {
                group: *group,
                bounds: *region,
                band_height,
                tabs,
            });
        }
        let source_solo = self
            .pane_layout
            .tabs(drag.source_group)
            .is_some_and(|tabs| tabs.len() == 1);
        let target = pane_drag::classify_target(
            bounds.size(),
            point,
            &geometries,
            Some(drag.source_group),
            source_solo,
        );
        if let Some(drag) = self.tab_drag.as_mut() {
            drag.target = target;
        }
    }

    /// Report the actual rendered terminal cell grid to the host's transport.
    pub fn set_grid_change_handler(&mut self, handler: GridChangeHandler) {
        self.grid_change = Some(handler);
    }

    pub fn handle_chrome(&mut self, message: &Message) {
        match message {
            Message::Toolbar(main_toolbar::Message::ToggleExpand) => {
                self.toolbar_expanded = !self.toolbar_expanded;
            }
            Message::Toolbar(main_toolbar::Message::Connect) | Message::OpenConnect => {
                self.connect_panel = true;
                self.settings_panel = false;
            }
            Message::Toolbar(main_toolbar::Message::Settings) => {
                self.settings_panel = true;
                self.connect_panel = false;
            }
            Message::ClosePanel => {
                self.connect_panel = false;
                self.settings_panel = false;
            }
            Message::AnimationFrame(now) if self.sessions.is_empty() => self.cat_clock.tick(*now),
            _ => {}
        }
    }

    #[must_use]
    pub fn needs_animation_frames(&self) -> bool {
        self.sessions.is_empty()
    }

    #[must_use]
    pub fn session_count(&self) -> usize {
        self.sessions.len()
    }

    #[must_use]
    pub fn appearance(&self) -> Appearance {
        self.appearance.value()
    }

    pub fn set_appearance(&mut self, appearance: Appearance) {
        self.appearance.replace(appearance);
        self.set_scrollback_lines(appearance.scrollback_lines);
    }

    pub fn update_appearance(
        &mut self,
        message: settings_appearance::Message,
    ) -> Option<AppearanceChange> {
        let change = self.appearance.update(message);
        if let Some(AppearanceChange::ScrollbackLines(lines)) = change {
            self.set_scrollback_lines(lines);
        }
        change
    }

    fn set_scrollback_lines(&mut self, lines: usize) {
        // The window preference owns this cap. An older in-flight worker
        // frame must not restore the previous value after a settings edit.
        let limit = NonZeroUsize::new(lines).expect("validated scrollback preference is nonzero");
        for session in &mut self.sessions {
            let mut terminal = session.terminal.borrow_mut();
            if terminal.max_lines() != lines {
                terminal.set_max_lines(limit);
            }
            drop(terminal);
            for pane in &mut session.script_panes {
                let mut terminal = pane.terminal.borrow_mut();
                if terminal.max_lines() != lines {
                    terminal.set_max_lines(limit);
                }
            }
        }
    }

    #[must_use]
    pub fn input_preferences(&self) -> InputPreferences {
        self.input.value()
    }

    #[must_use]
    pub fn command_syntax(&self) -> &CommandSyntax {
        self.syntax.value()
    }

    pub fn set_command_syntax(&mut self, syntax: CommandSyntax) {
        self.syntax.replace(syntax);
    }

    pub fn update_command_syntax(&mut self, message: settings_input::SyntaxMessage) -> bool {
        self.syntax.update(message)
    }

    #[must_use]
    pub fn theme_name(&self) -> &str {
        self.theme.selected()
    }

    pub fn set_theme(&mut self, theme: impl Into<String>) {
        self.theme.replace(theme);
    }

    pub fn update_theme(&mut self, message: settings_theme::Message) -> bool {
        self.theme.update(message).is_some()
    }

    #[must_use]
    pub fn theme(&self) -> Theme {
        smudgy_ui_shared::prefs::app_theme_for_palette(smudgy_ui_shared::prefs::palette_by_name(
            self.theme.selected(),
        ))
    }

    pub fn set_input_preferences(&mut self, preferences: InputPreferences) -> Task<Message> {
        self.input.replace(preferences);
        self.refresh_input_masks()
    }

    pub fn update_input_preferences(
        &mut self,
        message: settings_input::Message,
    ) -> (Option<InputChange>, Task<Message>) {
        let change = self.input.update(message);
        let task = if matches!(change, Some(InputChange::MaskInputOnServerEcho(_))) {
            self.refresh_input_masks()
        } else {
            Task::none()
        };
        (change, task)
    }

    fn refresh_input_masks(&mut self) -> Task<Message> {
        let mask = self.input.value().mask_input_on_server_echo;
        Task::batch(self.sessions.iter_mut().map(|session| {
            let id = session.id;
            session
                .session_input
                .set_telnet_mask(session.server_echo && mask)
                .map_message(move |message| Message::SessionInput(id, message))
                .task
        }))
    }

    pub fn session_ids(&self) -> impl Iterator<Item = SessionId> + '_ {
        self.sessions.iter().map(|session| session.id)
    }

    /// Capture the same versioned split forest used by native workspace
    /// files. The host supplies stable slot identities, never pane-grid IDs.
    pub fn snapshot_clusters(
        &self,
        mut slot_of: impl FnMut(SessionId) -> Option<u64>,
    ) -> Vec<dto::Cluster> {
        workspace_snapshot::clusters(&self.pane_layout, &mut |tab| {
            let pane = *tab.binding()?;
            if pane.key != MAIN_PANE_KEY {
                // Browser script panes are recreated by their script. Until
                // placeholder restore lands, do not persist a tab that this
                // host cannot safely bind before startup.
                return None;
            }
            Some(PaneRecord {
                slot: slot_of(pane.session_id)?,
                identity: dto::PaneIdentity::Main,
                hidden: false,
            })
        })
    }

    /// Install a fully resolved one-window layout after the host has
    /// validated its session bindings and handled omitted sessions.
    pub fn install_blueprint(
        &mut self,
        clusters: Vec<(f32, Blueprint<PaneRef>)>,
        active: Option<SessionId>,
    ) {
        self.pane_layout = GroupLayout::from_blueprint(clusters);
        self.active_session = active
            .filter(|id| self.sessions.iter().any(|session| session.id == *id))
            .or_else(|| self.sessions.last().map(|session| session.id));
        self.rebuild_grid();
    }

    /// The browser's current fallback arrangement: one main pane per
    /// session. Script panes can later extend this without changing the
    /// workspace format or the named-layout store.
    pub fn reset_layout(&mut self) {
        let clusters = self
            .session_ids()
            .map(|id| {
                (
                    1.0,
                    Blueprint::Group {
                        tabs: vec![Tab::bound(PaneRef::main(id))],
                        selected: 0,
                    },
                )
            })
            .collect();
        self.install_blueprint(clusters, self.active_session);
    }

    pub fn set_session_error(&mut self, id: SessionId, error: impl Into<String>) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.connection = ConnectionState::Disconnected;
            let mut terminal = session.terminal.borrow_mut();
            terminal.append_system_row(SystemRow::plain_notice(Severity::Warn, &error.into()));
            session.has_live_row = false;
        }
    }

    pub fn set_session_connecting(&mut self, id: SessionId) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.connection = ConnectionState::Connecting;
        }
    }

    pub fn set_session_disconnected(&mut self, id: SessionId) {
        if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
            session.connection = ConnectionState::Disconnected;
        }
    }

    fn pane_hidden(&self, pane: PaneRef) -> bool {
        if pane.key == MAIN_PANE_KEY {
            return false;
        }
        self.sessions
            .iter()
            .find(|session| session.id == pane.session_id)
            .and_then(|session| {
                session
                    .script_panes
                    .iter()
                    .find(|script| script.key == pane.key)
            })
            .is_none_or(|pane| pane.hidden)
    }

    /// # Panics
    ///
    /// Panics only if the already-validated scrollback preference is zero.
    #[allow(clippy::too_many_lines)]
    pub fn apply_browser_pane_event(&mut self, id: SessionId, event: BrowserPaneEvent) {
        match event {
            BrowserPaneEvent::Opened {
                key,
                name,
                hidden,
                placement,
            } => {
                if key == MAIN_PANE_KEY
                    || self
                        .sessions
                        .iter()
                        .find(|session| session.id == id)
                        .is_none_or(|session| {
                            session.script_panes.iter().any(|pane| pane.key == key)
                        })
                {
                    return;
                }
                let reference = PaneRef {
                    session_id: id,
                    key: placement.reference(),
                };
                let Some(reference_tab) = self
                    .pane_layout
                    .panes()
                    .into_iter()
                    .find(|tab| tab.binding() == Some(&reference))
                    .map(Tab::id)
                else {
                    return;
                };
                let Some(reference_group) = self.pane_layout.group_of(reference_tab) else {
                    return;
                };
                let limit = NonZeroUsize::new(self.appearance.value().scrollback_lines)
                    .expect("validated scrollback preference is nonzero");
                let terminal = Rc::new(RefCell::new(
                    TerminalBuffer::new_with_max_lines_and_capacity(
                        limit,
                        DEFAULT_SCROLLBACK_LINES,
                    ),
                ));
                let new_ref = PaneRef {
                    session_id: id,
                    key,
                };
                let tab = Tab::bound(new_ref);
                let new_tab_id = tab.id();
                let placed = match placement {
                    PanePlacement::Split {
                        direction, size_px, ..
                    } => {
                        let (axis, new_first) = match direction {
                            SplitDirection::Left => (pane_grid::Axis::Vertical, true),
                            SplitDirection::Right => (pane_grid::Axis::Vertical, false),
                            SplitDirection::Top => (pane_grid::Axis::Horizontal, true),
                            SplitDirection::Bottom => (pane_grid::Axis::Horizontal, false),
                        };
                        let sizing =
                            size_px.map_or(SplitSizing::Ratio(0.5), |px| SplitSizing::Px {
                                px,
                                sized_first: new_first,
                            });
                        self.pane_layout
                            .split_group(reference_group, axis, new_first, sizing, tab)
                            .is_ok()
                    }
                    PanePlacement::Tab {
                        position, selected, ..
                    } => {
                        let reference_index = self
                            .pane_layout
                            .tabs(reference_group)
                            .and_then(|tabs| {
                                tabs.iter()
                                    .position(|candidate| candidate.id() == reference_tab)
                            })
                            .unwrap_or(0);
                        let index = match position {
                            TabPosition::Before => reference_index,
                            TabPosition::After => reference_index + 1,
                            TabPosition::End => usize::MAX,
                        };
                        let inserted = self
                            .pane_layout
                            .insert_tab(reference_group, index, tab)
                            .is_ok();
                        if inserted && selected {
                            let _ = self.pane_layout.select(new_tab_id);
                        }
                        inserted
                    }
                };
                if !placed {
                    return;
                }
                let session = self
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == id)
                    .expect("the pane owner was checked above");
                session.script_panes.push(ScriptPaneUiState {
                    key,
                    name,
                    hidden,
                    terminal,
                    terminal_view: TerminalViewHandle::default(),
                });
                self.rebuild_grid();
            }
            BrowserPaneEvent::Updated { key, hidden } => {
                if let Some(pane) = self
                    .sessions
                    .iter_mut()
                    .find(|session| session.id == id)
                    .and_then(|session| {
                        session.script_panes.iter_mut().find(|pane| pane.key == key)
                    })
                {
                    pane.hidden = hidden;
                    self.rebuild_grid();
                }
            }
            BrowserPaneEvent::Echo { key, text } => {
                let target = self.sessions.iter_mut().find(|session| session.id == id);
                let Some(target) = target else { return };
                let terminal = if key == MAIN_PANE_KEY {
                    if target.has_live_row {
                        let mut terminal = target.terminal.borrow_mut();
                        terminal.begin_open_line_replacement();
                        terminal.finish_open_line_replacement(None);
                        target.has_live_row = false;
                    }
                    Rc::clone(&target.terminal)
                } else {
                    let Some(pane) = target.script_panes.iter().find(|pane| pane.key == key) else {
                        return;
                    };
                    Rc::clone(&pane.terminal)
                };
                for line in text.split('\n') {
                    terminal
                        .borrow_mut()
                        .push_line(Arc::new(StyledLine::from_echo_str(line)));
                }
            }
            BrowserPaneEvent::Clear(key) => {
                let Some(session) = self.sessions.iter_mut().find(|session| session.id == id)
                else {
                    return;
                };
                if key == MAIN_PANE_KEY {
                    session.terminal.borrow_mut().clear_lines();
                    session.has_live_row = false;
                } else if let Some(pane) = session.script_panes.iter().find(|pane| pane.key == key)
                {
                    pane.terminal.borrow_mut().clear_lines();
                }
            }
            BrowserPaneEvent::Closed(key) => {
                let binding = PaneRef {
                    session_id: id,
                    key,
                };
                if let Some(tab) = self
                    .pane_layout
                    .panes()
                    .into_iter()
                    .find(|tab| tab.binding() == Some(&binding))
                    .map(Tab::id)
                {
                    let _ = self.pane_layout.remove_tab(tab);
                }
                if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
                    session.script_panes.retain(|pane| pane.key != key);
                }
                self.rebuild_grid();
            }
        }
    }

    #[must_use]
    pub fn session_connection(&self, id: SessionId) -> Option<ConnectionState> {
        self.sessions
            .iter()
            .find(|session| session.id == id)
            .map(|session| session.connection)
    }

    /// Put an unsent command back exactly like native's rescue scope: the
    /// text is selected so Enter retries it and typing replaces it.
    pub fn recover_failed_submission(
        &mut self,
        id: SessionId,
        text: Arc<String>,
        notice: &str,
    ) -> Task<Message> {
        let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) else {
            return Task::none();
        };
        session
            .terminal
            .borrow_mut()
            .append_system_row(SystemRow::plain_notice(Severity::Warn, notice));
        session.has_live_row = false;
        session
            .session_input
            .apply_script_op(&InputOp::Propose(text))
            .map_message(move |message| Message::SessionInput(id, message))
            .task
    }

    pub fn apply_session_update(&mut self, id: SessionId, update: SessionUpdate) -> Task<Message> {
        let mask = self.input.value().mask_input_on_server_echo;
        self.sessions
            .iter_mut()
            .find(|session| session.id == id)
            .map_or_else(Task::none, |session| session.apply(update, mask))
    }

    pub fn update_session_input(
        &mut self,
        id: SessionId,
        message: session_input::Message,
    ) -> Update<Message, session_input::Event> {
        self.sessions
            .iter_mut()
            .find(|session| session.id == id)
            .map_or_else(Update::none, |session| {
                session
                    .session_input
                    .update(message)
                    .map_message(move |message| Message::SessionInput(id, message))
            })
    }
}

/// Resolve the browser-renderable portion of the common workspace schema.
/// Unsupported script and hidden panes fail visibly rather than being
/// silently dropped from a native-authored layout.
///
/// # Errors
///
/// Returns a description when a tab or session is unavailable in this browser window.
pub fn blueprint_from_dto(
    node: &dto::Node,
    session_of: &impl Fn(u64) -> Option<SessionId>,
) -> Result<Blueprint<PaneRef>, String> {
    match node {
        dto::Node::Group(group) => {
            let tabs = group
                .tabs
                .iter()
                .map(|pane| {
                    if pane.id != dto::PaneIdentity::Main || pane.hidden {
                        return Err(
                            "This browser build cannot yet display script or hidden panes".into(),
                        );
                    }
                    let session = session_of(pane.slot).ok_or_else(|| {
                        format!("Layout session slot {} is unavailable", pane.slot)
                    })?;
                    Ok(Tab::bound(PaneRef::main(session)))
                })
                .collect::<Result<Vec<_>, String>>()?;
            if tabs.is_empty() || group.selected >= tabs.len() {
                return Err("Layout tab group has no valid selection".into());
            }
            Ok(Blueprint::Group {
                tabs,
                selected: group.selected,
            })
        }
        dto::Node::Split(split) => Ok(Blueprint::Split {
            axis: match split.axis {
                dto::Axis::Horizontal => pane_grid::Axis::Horizontal,
                dto::Axis::Vertical => pane_grid::Axis::Vertical,
            },
            sizing: match split.sizing {
                dto::Sizing::Ratio(ratio) => SplitSizing::Ratio(ratio),
                dto::Sizing::Px { px, sized_first } => SplitSizing::Px { px, sized_first },
            },
            a: Box::new(blueprint_from_dto(&split.a, session_of)?),
            b: Box::new(blueprint_from_dto(&split.b, session_of)?),
        }),
    }
}

/// Reject a native-authored pane shape this browser shell cannot yet render,
/// before the host opens or closes any sessions.
///
/// # Errors
///
/// Returns a description when any cluster contains an unsupported pane.
pub fn validate_browser_clusters(clusters: &[dto::Cluster]) -> Result<(), String> {
    for cluster in clusters {
        let _ = blueprint_from_dto(&cluster.root, &|_| Some(SessionId::from(0)))?;
    }
    Ok(())
}

/// Browser-window event projection used only while a pane drag is live.
/// Captured events are included so terminal contents cannot strand a drag.
#[must_use]
#[allow(clippy::needless_pass_by_value)] // `event::listen_with` owns this argument.
pub fn pane_drag_event(
    event: IcedEvent,
    _status: iced::event::Status,
    _window: iced::window::Id,
) -> Option<Message> {
    match event {
        IcedEvent::Mouse(mouse::Event::CursorMoved { position }) => {
            Some(Message::DragMoved(position))
        }
        IcedEvent::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left)) => {
            Some(Message::DragReleased)
        }
        IcedEvent::Keyboard(keyboard::Event::KeyPressed {
            key: keyboard::Key::Named(keyboard::key::Named::Escape),
            ..
        }) => Some(Message::DragCanceled),
        _ => None,
    }
}

/// The shared Smudgy client surface. Session engines and physical transports
/// remain owned by the platform host's per-session workers.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn view<'a>(
    ui: &'a ClientUiState,
    label: &impl Fn(&'static str) -> String,
) -> Element<'a, Message, Theme> {
    let main_content: Element<'_, Message, Theme> = if let Some(grid) = &ui.pane_grid {
        // Native reveals otherwise-hidden headers while its toolbar is expanded.
        // Browser panes have no per-pane pin policy yet.
        let show_header =
            show_pane_header(ui.appearance.value().hide_pane_headers, ui.toolbar_expanded);
        let panes = PaneGrid::new(grid, |_pane, group, _maximized| {
            let Some(members) = ui.pane_layout.tabs(*group).and_then(|tabs| {
                tabs.iter()
                    .filter(|tab| tab.binding().is_some_and(|pane| !ui.pane_hidden(*pane)))
                    .map(|tab| Some((tab.id(), *tab.binding()?)))
                    .collect::<Option<Vec<_>>>()
            }) else {
                return pane_grid::Content::new(iced::widget::Space::new());
            };
            let selected = ui.pane_layout.effective_selected(*group, |tab| {
                tab.binding().is_some_and(|pane| !ui.pane_hidden(*pane))
            });
            let selected_index = members
                .iter()
                .position(|(tab, _)| Some(*tab) == selected)
                .unwrap_or(0);
            let pane_bodies =
                members
                    .iter()
                    .copied()
                    .filter_map(|(_, pane)| {
                        let member = ui
                            .sessions
                            .iter()
                            .find(|session| session.id == pane.session_id)?;
                        if pane.key == MAIN_PANE_KEY {
                            let terminal = split_terminal_pane(
                                member.terminal.borrow(),
                                member.terminal_view.clone(),
                                None,
                                None,
                                ui.grid_change.as_ref().map(|handler| {
                                    let handler = Rc::clone(handler);
                                    let id = member.id;
                                    Rc::new(move |columns, rows| handler(id, columns, rows))
                                        as Rc<dyn Fn(u16, u16)>
                                }),
                                None,
                                ScrolledLayout::SplitWithLiveTail,
                                None,
                            );
                            Some(
                                column![
                                    terminal,
                                    container(member.session_input.view_with_font_size(None).map(
                                        move |message| Message::SessionInput(member.id, message)
                                    ))
                                    .padding([5, 8])
                                    .style(builtins::container::pane_title_bar_active),
                                ]
                                .width(Fill)
                                .height(Fill)
                                .into(),
                            )
                        } else {
                            let script = member
                                .script_panes
                                .iter()
                                .find(|script| script.key == pane.key)?;
                            Some(split_terminal_pane(
                                script.terminal.borrow(),
                                script.terminal_view.clone(),
                                None,
                                None,
                                None,
                                None,
                                ScrolledLayout::SplitWithLiveTail,
                                None,
                            ))
                        }
                    })
                    .collect();
            let body: Element<'_, Message, Theme> = TabHost::new(
                members.iter().map(|(tab, _)| *tab).collect(),
                pane_bodies,
                selected_index,
            )
            .into();
            if !show_header {
                return pane_grid::Content::new(body);
            }
            let durable_selected = ui.pane_layout.selected(*group);
            let descriptors = members.iter().filter_map(|(tab, pane)| {
                let member = ui
                    .sessions
                    .iter()
                    .find(|session| session.id == pane.session_id)?;
                let title = if pane.key == MAIN_PANE_KEY {
                    member.title.clone()
                } else {
                    member
                        .script_panes
                        .iter()
                        .find(|script| script.key == pane.key)
                        .map_or_else(|| member.title.clone(), |script| script.name.clone())
                };
                Some(tab_strip::TabDescriptor {
                    id: *tab,
                    label: title,
                    main: pane.key == MAIN_PANE_KEY,
                    selected: durable_selected == Some(*tab),
                    rendered: selected == Some(*tab),
                    hidden: false,
                    active_session: ui.active_session == Some(pane.session_id),
                    connected: member.connection == ConnectionState::Connected,
                    ever_connected: member.connection != ConnectionState::Connecting,
                })
            });
            let strip = tab_strip::view(
                iced::advanced::widget::Id::from(format!("web-pane-tab-strip-{}", group.as_u64())),
                descriptors,
                |tab| iced::advanced::widget::Id::from(format!("web-pane-tab-{}", tab.as_u64())),
                tab_strip::StripContext {
                    drag_live: ui.tab_drag.is_some(),
                    modifiers: keyboard::Modifiers::default(),
                    visibility_eyes: false,
                    labels: tab_strip::StripLabels::default(),
                    on_strip_bounds: Box::new(move |bounds| {
                        ui.strip_bands.borrow_mut().insert(*group, bounds);
                    }),
                    on_tab_bounds: Rc::new(move |tab, bounds| {
                        ui.tab_spans.borrow_mut().insert(tab, bounds);
                    }),
                },
                move |event| Message::TabStrip(*group, event),
            );
            pane_grid::Content::new(body).title_bar(
                pane_grid::TitleBar::new(strip)
                    .padding(2)
                    .style(builtins::container::pane_title_bar_active),
            )
        })
        .width(Fill)
        .height(Fill)
        .spacing(4)
        .on_click(Message::PaneClicked)
        .on_resize(8, Message::PaneResized);
        let panes = BoundsProbe::new(panes, |bounds| ui.grid_bounds.set(bounds));
        let mut rects = Vec::new();
        if let Some(target) = ui.tab_drag.as_ref().and_then(|drag| drag.target.as_ref()) {
            rects.push(drag_overlay::OverlayRect {
                bounds: target.highlight,
                role: drag_overlay::OverlayRole::Target,
            });
            if let Some(caret) = target.caret {
                rects.push(drag_overlay::OverlayRect {
                    bounds: caret,
                    role: drag_overlay::OverlayRole::Caret,
                });
            }
        }
        stack![panes, drag_overlay::TargetOverlay::new(rects)].into()
    } else {
        empty_state(ui.cat_clock.seconds())
    };

    let body: Element<'_, Message, Theme> = column![
        main_toolbar::view(ui.toolbar_expanded, ui.active_session.is_some()).map(Message::Toolbar),
        main_content
    ]
    .width(Fill)
    .height(Fill)
    .into();

    let main: Element<'_, Message, Theme> = container(body)
        .width(Fill)
        .height(Fill)
        .style(builtins::container::opaque)
        .into();
    if ui.connect_panel {
        let modal = connect_modal::frame(
            "Connect",
            connect_manager::view(&ui.manager).map(Message::Manager),
        );
        stack![
            main,
            opaque(
                mouse_area(center(opaque(modal)).style(builtins::container::overlay))
                    .on_press(Message::ClosePanel)
            )
        ]
        .into()
    } else if ui.settings_panel {
        let modal = connect_modal::frame_with_size(
            "Settings",
            settings_panel(&ui.appearance, &ui.input, &ui.syntax, &ui.theme, label),
            640.0,
            500.0,
        );
        stack![
            main,
            opaque(
                mouse_area(center(opaque(modal)).style(builtins::container::overlay))
                    .on_press(Message::ClosePanel)
            )
        ]
        .into()
    } else {
        main
    }
}

fn show_pane_header(hide_pane_headers: bool, toolbar_expanded: bool) -> bool {
    !hide_pane_headers || toolbar_expanded
}

fn empty_state(seconds: f32) -> Element<'static, Message, Theme> {
    container(
        column![
            crt_cat::view(seconds),
            text("No active sessions").font(GEIST).size(22),
            text("Connect to a server to get started.")
                .font(GEIST)
                .style(builtins::text::muted),
            button(text("Connect to a server").font(GEIST))
                .style(builtins::button::primary)
                .padding([10, 22])
                .on_press(Message::OpenConnect),
        ]
        .spacing(16)
        .align_x(Horizontal::Center),
    )
    .width(Fill)
    .height(Fill)
    .align_x(Horizontal::Center)
    .align_y(Vertical::Center)
    .into()
}

fn settings_panel<'a>(
    appearance: &'a settings_appearance::State,
    input: &'a settings_input::State,
    syntax: &'a settings_input::SyntaxState,
    theme: &'a settings_theme::State,
    label: &impl Fn(&'static str) -> String,
) -> Element<'a, Message, Theme> {
    container(
        column![
            iced::widget::scrollable(
                column![
                    settings_appearance::view(appearance, label).map(Message::Appearance),
                    settings_theme::view(theme, label).map(Message::Theme),
                    settings_appearance::theme_extended_colors_view(appearance, label)
                        .map(Message::Appearance),
                    settings_appearance::scrollback_view(appearance, label)
                        .map(Message::Appearance),
                    settings_appearance::pane_headers_view(appearance, label)
                        .map(Message::Appearance),
                    iced::widget::rule::horizontal(1),
                    text(label("preferences-input")).size(15),
                    settings_input::syntax_view(syntax, label).map(Message::Syntax),
                    settings_input::view(input, label).map(Message::Input),
                ]
                .spacing(12)
            )
            .height(Fill),
            row![
                iced::widget::Space::new().width(Fill),
                button("Close")
                    .style(builtins::button::secondary)
                    .on_press(Message::ClosePanel),
            ]
            .align_y(iced::Alignment::Center),
        ]
        .spacing(10),
    )
    .padding(15)
    .width(Fill)
    .height(Fill)
    .into()
}

fn endpoint_title(endpoint: &str) -> String {
    endpoint
        .split_once("://")
        .map_or(endpoint, |(_, rest)| rest)
        .split('/')
        .next()
        .filter(|host| !host.is_empty())
        .unwrap_or("Session")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pane_header_policy_matches_native_toolbar_override() {
        assert!(show_pane_header(false, false));
        assert!(show_pane_header(false, true));
        assert!(!show_pane_header(true, false));
        assert!(show_pane_header(true, true));
    }

    fn line(text: &str) -> StyledLine {
        StyledLine::new(text, Vec::new())
    }

    fn update(rows: &[&str], live: Option<&str>, reset: bool) -> SessionUpdate {
        SessionUpdate {
            sequence: 0,
            connection: ConnectionState::Connected,
            status: "Connected".to_owned(),
            server_echo: false,
            gmcp_messages: 3,
            max_lines: smudgy_session_model::DEFAULT_SCROLLBACK_LINES,
            revision: rows.len() as u64,
            committed: rows.len(),
            reset,
            rows: rows.iter().map(|text| line(text)).collect(),
            live: live.map(line),
        }
    }

    #[test]
    fn sessions_get_independent_grid_clusters_and_buffers() {
        let mut ui = ClientUiState::new();
        let first = ui.open_session("wss://first.example/ws");
        let second = ui.open_session("wss://second.example/ws");

        assert_eq!(ui.session_count(), 2);
        assert!(ui.pane_grid.is_some());
        assert_eq!(ui.pane_layout.groups_depth_first().len(), 2);

        drop(ui.apply_session_update(first, update(&["one"], Some("prompt"), true)));
        let first_session = ui
            .sessions
            .iter()
            .find(|session| session.id == first)
            .unwrap();
        let second_session = ui
            .sessions
            .iter()
            .find(|session| session.id == second)
            .unwrap();
        assert_eq!(first_session.terminal.borrow().len(), 2);
        assert!(second_session.terminal.borrow().is_empty());
    }

    #[test]
    fn changing_scrollback_trims_open_pane_buffers() {
        let mut ui = ClientUiState::new();
        let id = ui.open_session("wss://example.org/ws");
        let rows: Vec<_> = (0..110).map(|index| format!("line {index}")).collect();
        let row_refs: Vec<_> = rows.iter().map(String::as_str).collect();
        drop(ui.apply_session_update(id, update(&row_refs, None, true)));
        assert_eq!(ui.sessions[0].terminal.borrow().len(), 110);

        ui.set_appearance(Appearance {
            scrollback_lines: 100,
            ..Appearance::default()
        });
        assert_eq!(ui.sessions[0].terminal.borrow().len(), 100);
        // A worker frame emitted before the edit still advertises its old
        // limit; it must not expand the window-side buffer again.
        drop(ui.apply_session_update(id, update(&["late frame"], None, false)));
        assert_eq!(ui.sessions[0].terminal.borrow().len(), 100);
        assert_eq!(ui.sessions[0].terminal.borrow().max_lines(), 100);
    }

    #[test]
    fn script_panes_share_layout_scrollback_and_terminal_operations() {
        let mut ui = ClientUiState::new();
        let id = ui.open_session("wss://example.org/ws");
        let chat = PaneKey::from_u32(1);
        let alerts = PaneKey::from_u32(2);

        ui.apply_browser_pane_event(
            id,
            BrowserPaneEvent::Opened {
                key: chat,
                name: "Chat".into(),
                hidden: false,
                placement: PanePlacement::Split {
                    reference: MAIN_PANE_KEY,
                    direction: SplitDirection::Right,
                    size_px: Some(280.0),
                },
            },
        );
        ui.apply_browser_pane_event(
            id,
            BrowserPaneEvent::Opened {
                key: alerts,
                name: "Alerts".into(),
                hidden: false,
                placement: PanePlacement::Tab {
                    reference: chat,
                    position: TabPosition::After,
                    selected: true,
                },
            },
        );
        assert_eq!(ui.pane_layout.groups_depth_first().len(), 2);
        assert!(ui.pane_layout.contains_binding(PaneRef {
            session_id: id,
            key: chat
        }));
        assert!(ui.pane_layout.contains_binding(PaneRef {
            session_id: id,
            key: alerts
        }));

        ui.apply_browser_pane_event(
            id,
            BrowserPaneEvent::Echo {
                key: alerts,
                text: "one\ntwo".into(),
            },
        );
        let pane = &ui.sessions[0].script_panes[1];
        assert_eq!(pane.terminal.borrow().len(), 2);

        ui.set_appearance(Appearance {
            scrollback_lines: 100,
            ..Appearance::default()
        });
        assert!(
            ui.sessions[0]
                .script_panes
                .iter()
                .all(|pane| pane.terminal.borrow().max_lines() == 100)
        );

        ui.apply_browser_pane_event(
            id,
            BrowserPaneEvent::Updated {
                key: chat,
                hidden: true,
            },
        );
        assert!(ui.sessions[0].script_panes[0].hidden);
        ui.apply_browser_pane_event(id, BrowserPaneEvent::Clear(alerts));
        assert!(ui.sessions[0].script_panes[1].terminal.borrow().is_empty());
        ui.apply_browser_pane_event(id, BrowserPaneEvent::Closed(alerts));
        assert_eq!(ui.sessions[0].script_panes.len(), 1);
        assert!(!ui.pane_layout.contains_binding(PaneRef {
            session_id: id,
            key: alerts
        }));
    }

    #[test]
    fn held_server_echo_reapplies_the_input_mask_preference() {
        let mut ui = ClientUiState::new();
        let id = ui.open_session("wss://example.org/ws");
        let mut echo = update(&[], None, false);
        echo.server_echo = true;
        drop(ui.apply_session_update(id, echo));
        drop(ui.update_session_input(id, session_input::Message::InputChanged("secret".into())));
        let masked = ui.update_session_input(id, session_input::Message::Submit);
        assert!(matches!(
            masked.event,
            Some(session_input::Event::Submit { masked: true, .. })
        ));

        let (change, task) = ui
            .update_input_preferences(settings_input::Message::MaskInputOnServerEchoToggled(false));
        assert_eq!(change, Some(InputChange::MaskInputOnServerEcho(false)));
        drop(task);
        drop(ui.update_session_input(id, session_input::Message::InputChanged("visible".into())));
        let visible = ui.update_session_input(id, session_input::Message::Submit);
        assert!(matches!(
            visible.event,
            Some(session_input::Event::Submit { masked: false, .. })
        ));
    }

    #[test]
    fn closing_sessions_releases_their_panes_and_restores_the_empty_state() {
        let mut ui = ClientUiState::new();
        let first = ui.open_session("wss://first.example/ws");
        let second = ui.open_session("wss://second.example/ws");
        let first_buffer = Rc::downgrade(&ui.sessions[0].terminal);

        assert!(ui.close_session(first));
        assert_eq!(ui.session_count(), 1);
        assert!(!ui.pane_layout.contains_binding(PaneRef::main(first)));
        assert!(ui.pane_grid.is_some());
        assert!(first_buffer.upgrade().is_none());
        assert!(!ui.close_session(first));

        assert!(ui.close_session(second));
        assert_eq!(ui.session_count(), 0);
        assert!(ui.pane_grid.is_none());
        assert!(ui.needs_animation_frames());
    }

    #[test]
    fn versioned_split_capture_round_trips_through_browser_blueprint() {
        let mut ui = ClientUiState::new();
        let first = ui.open_session("wss://first.example/ws");
        let second = ui.open_session("wss://second.example/ws");
        ui.install_blueprint(
            vec![(
                1.0,
                Blueprint::Split {
                    axis: pane_grid::Axis::Vertical,
                    sizing: SplitSizing::Ratio(0.4),
                    a: Box::new(Blueprint::Group {
                        tabs: vec![Tab::bound(PaneRef::main(first))],
                        selected: 0,
                    }),
                    b: Box::new(Blueprint::Group {
                        tabs: vec![Tab::bound(PaneRef::main(second))],
                        selected: 0,
                    }),
                },
            )],
            Some(first),
        );
        let captured = ui.snapshot_clusters(|id| Some(u64::from(u32::from(id))));
        assert!(matches!(captured[0].root, dto::Node::Split(_)));
        let rebuilt = captured
            .iter()
            .map(|cluster| {
                (
                    cluster.weight,
                    blueprint_from_dto(&cluster.root, &|slot| {
                        u32::try_from(slot).ok().map(SessionId::from)
                    })
                    .unwrap(),
                )
            })
            .collect();
        ui.install_blueprint(rebuilt, Some(second));
        assert_eq!(ui.active_session(), Some(second));
        assert_eq!(
            ui.snapshot_clusters(|id| Some(u64::from(u32::from(id)))),
            captured
        );
        ui.reset_layout();
        assert_eq!(
            ui.snapshot_clusters(|id| Some(u64::from(u32::from(id))))
                .len(),
            2
        );
    }

    #[test]
    fn browser_blueprint_rejects_an_unrenderable_native_script_pane() {
        let node = dto::Node::Group(dto::Group {
            tabs: vec![dto::Pane {
                slot: 1,
                id: dto::PaneIdentity::Script {
                    namespace: dto::Namespace::User,
                    name: "map".into(),
                    display: None,
                },
                hidden: false,
            }],
            selected: 0,
        });
        assert!(blueprint_from_dto(&node, &|_| Some(SessionId::from(1))).is_err());
    }

    #[test]
    fn browser_tab_group_restores_and_persists_its_selected_session() {
        let mut ui = ClientUiState::new();
        let first = ui.open_session("wss://first.example/ws");
        let second = ui.open_session("wss://second.example/ws");
        let node = dto::Node::Group(dto::Group {
            tabs: [1, 2]
                .map(|slot| dto::Pane {
                    slot,
                    id: dto::PaneIdentity::Main,
                    hidden: false,
                })
                .into(),
            selected: 1,
        });
        let blueprint = blueprint_from_dto(&node, &|slot| match slot {
            1 => Some(first),
            2 => Some(second),
            _ => None,
        })
        .unwrap();
        ui.install_blueprint(vec![(1.0, blueprint)], Some(second));
        let group = ui.pane_layout.groups_depth_first()[0];
        assert_eq!(ui.pane_layout.tabs(group).unwrap().len(), 2);
        let first_tab = ui.pane_layout.tabs(group).unwrap()[0].id();
        assert_eq!(
            ui.pane_layout.selected(group),
            Some(ui.pane_layout.tabs(group).unwrap()[1].id())
        );

        assert!(ui.select_tab(first_tab));
        assert_eq!(ui.active_session(), Some(first));
        let captured = ui.snapshot_clusters(|id| Some(u64::from(u32::from(id))));
        assert!(matches!(
            &captured[0].root,
            dto::Node::Group(dto::Group { tabs, selected: 0 }) if tabs.len() == 2
        ));
    }

    #[test]
    fn divider_drag_survives_a_new_session_and_grid_rebuild() {
        let mut ui = ClientUiState::new();
        ui.open_session("wss://first.example/ws");
        ui.open_session("wss://second.example/ws");
        let split = *ui.split_targets.keys().next().unwrap();
        ui.resize_pane(pane_grid::ResizeEvent { split, ratio: 0.7 });

        ui.open_session("wss://third.example/ws");
        let weights = ui.pane_layout.structure();
        let ratio = weights[0].0 / (weights[0].0 + weights[1].0);
        assert!((ratio - 0.7).abs() < 0.001);
        assert_eq!(ui.split_targets.len(), 2);
    }

    #[test]
    fn connect_action_opens_the_modal_until_a_session_starts_or_it_is_closed() {
        let mut ui = ClientUiState::new();
        ui.handle_chrome(&Message::OpenConnect);
        assert!(ui.connect_panel);
        ui.handle_chrome(&Message::ClosePanel);
        assert!(!ui.connect_panel);
        ui.handle_chrome(&Message::Toolbar(main_toolbar::Message::Connect));
        assert!(ui.connect_panel);
        ui.open_session("wss://example.test/ws");
        assert!(!ui.connect_panel);
    }

    #[test]
    fn a_new_delta_replaces_the_mutable_prompt_before_appending() {
        let mut ui = ClientUiState::new();
        let id = ui.open_session("wss://example.test/ws");
        drop(ui.apply_session_update(id, update(&["one"], Some("old prompt"), true)));
        drop(ui.apply_session_update(id, update(&["two"], Some("new prompt"), false)));

        let session = &ui.sessions[0];
        let text: Vec<_> = session
            .terminal
            .borrow()
            .iter_rev()
            .map(|line| line.styled_line.text.clone())
            .collect();
        assert_eq!(text, ["new prompt", "two", "one"]);
        assert_eq!(session.connection, ConnectionState::Connected);
    }

    #[test]
    fn disconnect_and_worker_errors_remain_visible_in_scrollback() {
        let mut ui = ClientUiState::new();
        let id = ui.open_session("wss://example.test/ws");
        drop(ui.apply_session_update(id, update(&["welcome"], Some("prompt"), true)));

        let mut disconnected = update(&[], None, false);
        disconnected.connection = ConnectionState::Disconnected;
        disconnected.status = "WebSocket closed (1006)".to_owned();
        drop(ui.apply_session_update(id, disconnected));
        ui.set_session_error(id, "worker failed");

        let lines: Vec<_> = ui.sessions[0]
            .terminal
            .borrow()
            .iter_rev()
            .map(|line| line.styled_line.text.clone())
            .collect();
        assert_eq!(
            lines,
            ["worker failed", "WebSocket closed (1006)", "welcome"]
        );
    }

    #[test]
    fn worker_error_preserves_an_open_prompt() {
        let mut ui = ClientUiState::new();
        let id = ui.open_session("wss://example.test/ws");
        drop(ui.apply_session_update(id, update(&[], Some("prompt> "), true)));

        ui.set_session_error(id, "worker failed");

        let lines: Vec<_> = ui.sessions[0]
            .terminal
            .borrow()
            .iter_rev()
            .map(|line| line.styled_line.text.clone())
            .collect();
        assert_eq!(lines, ["worker failed", "prompt> "]);
    }

    #[test]
    fn endpoint_titles_use_the_authority() {
        assert_eq!(
            endpoint_title("wss://last-outpost.com/ws/telnet/"),
            "last-outpost.com"
        );
        assert_eq!(endpoint_title("custom-session"), "custom-session");
    }
}
