use std::sync::Arc;

use iced::{
    Event as IcedEvent, Length, Point, Size, Subscription, Task,
    alignment::{Horizontal, Vertical},
    keyboard,
    widget::{
        PaneGrid, center, column, container, mouse_area, opaque, operation, pane_grid, row, stack,
        svg, text,
    },
    window,
};
use smudgy_cloud::{AreaId, Mapper};
use smudgy_core::session::SessionId;
use smudgy_core::session::runtime::pane::{
    MAIN_PANE_KEY, PaneKey, PanePlacement, SplitDirection, TitleBarPolicy,
};

use crate::{
    assets,
    cloud_account::CloudHandles,
    components::{modal, resize_grips, toolbar},
    pane_drag,
    pane_layout::{self, SplitSizing, WindowLayout},
    session_store::{self, SessionStore},
    theme::{self, Element as ThemedElement},
    update::Update,
};

/// Spawn the bundled `smudgy_inspector` DevTools window for a session's v8
/// inspector endpoint. Resolves the helper next to the running executable (so it
/// works both from `cargo run` and an installed bundle); failures are logged, not
/// fatal.
pub(crate) fn spawn_inspector(addr: std::net::SocketAddr) {
    let exe_name = if cfg!(windows) {
        "smudgy_inspector.exe"
    } else {
        "smudgy_inspector"
    };
    let program = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|dir| dir.join(exe_name)))
        .unwrap_or_else(|| std::path::PathBuf::from(exe_name));
    let mut command = std::process::Command::new(&program);
    command.arg(addr.to_string());
    // The helper is a console-subsystem binary, so spawning it from the GUI app
    // would otherwise pop a stray console window on Windows. CREATE_NO_WINDOW
    // suppresses it; the helper still runs, and its diagnostics remain visible when
    // it's launched directly from a terminal (which doesn't pass this flag).
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    match command.spawn() {
        Ok(_) => log::info!("Launched smudgy_inspector for {addr}"),
        Err(e) => log::warn!("Failed to launch {}: {e}", program.display()),
    }
}

#[derive(Debug, Clone)]
pub enum Message {
    ToolbarAction(toolbar::Message),
    ModalMessage(modal::Message),
    ModalEvent(modal::Event),
    CloseModal,
    EscapePressed(window::Id),
    /// `Tab` / `Shift+Tab` while a modal form is open: walk focus to the
    /// next/previous field. Carries the originating window so only that window
    /// reacts (mirrors `EscapePressed`).
    FocusNext(window::Id),
    FocusPrevious(window::Id),
    ResizeGripPressed(window::Direction),
    WindowResized(window::Id),
    SetMaximized(bool),
    /// Activate a session in this window (the daemon sends this after
    /// transplanting a pane here, so the drop also moves the user's focus).
    SetActiveSession(SessionId),
    SessionPaneUserAction {
        session_id: SessionId,
        msg: session_store::Message,
    },
    /// A left press anywhere in a pane (title bar or body): activate that
    /// pane's session.
    PaneClicked(pane_grid::Pane),
    PaneDragged(pane_grid::DragEvent),
    PaneResized(pane_grid::ResizeEvent),
    /// The title-bar eye toggle: flip a pane between visible and hidden.
    /// Hidden is a soft display state — the pane's session keeps running;
    /// the slot just leaves the derived grid while the toolbar is collapsed.
    TogglePaneVisibility(PaneRef),
    OpenSettingsPressed,
    /// The user clicked an "out of date" / "upgrade available" download link.
    OpenDownloadPage,
    /// "Dismiss" on the soft upgrade popup (this session only).
    DismissUpgrade,
    /// "Dismiss for this version" on the soft upgrade popup (persisted).
    DismissUpgradeForVersion,
}

#[derive(Debug, Clone)]
pub enum Event {
    CreateNewScriptEditorWindow {
        server_name: Arc<String>,
        session_id: SessionId,
    },
    CreateNewMapEditorWindow {
        mapper: Mapper,
        /// The originating session's server entry — the scope context the map
        /// editor filters and writes cloud-map associations against.
        server_name: Arc<String>,
    },
    SetMapperCurrentLocation(AreaId, Option<i32>),
    /// The user closed a session (title-bar ✕). Teardown — store removal,
    /// runtime shutdown, grid cleanup across all windows, the empty-window
    /// rule — is the daemon's job.
    CloseSession(SessionId),
    /// A pane pick started in this window's grid. The daemon's DragController
    /// records it; every terminal outcome (drop, cancel, tear-out) resolves
    /// against this record.
    PaneDragPicked {
        pane: pane_grid::Pane,
        slot: PaneRef,
    },
    /// A drag ended in a native in-window drop (already applied to this
    /// window's grid); the daemon just clears its drag record.
    PaneDragEnded,
    /// pane_grid published `Canceled` for this pick: a plain click, a release
    /// over this window, or a release outside it. Disambiguating those — and
    /// executing a cross-window transplant or tear-out — is the daemon's job
    /// (it owns the window map and the screen-space tracking).
    PaneDragCanceled(pane_grid::Pane),
    OpenSettingsWindow,
    OpenDownloadPage,
    DismissUpgrade,
    DismissUpgradeForVersion,
}

/// Grid payload: a reference into the daemon's session store identifying
/// which session pane fills this slot. `key == MAIN_PANE_KEY` is the
/// session's fused output+input pane; any other key is a script-created pane
/// whose display state lives in the session's pane map.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PaneRef {
    pub session_id: SessionId,
    pub key: PaneKey,
}

/// The pane grid's inter-pane spacing (must match the `.spacing()` set on the
/// `PaneGrid` widget — `pane_regions` and the layout model's px→ratio math
/// need the real value).
const GRID_SPACING: f32 = 4.0;

/// pane_grid's default minimum pane size (no `.min_size()` override is set).
const GRID_MIN_SIZE: f32 = 50.0;

pub struct SmudgyWindow {
    window_id: window::Id,
    cloud: CloudHandles,
    toolbar_expanded: bool,
    maximized: bool,
    modal: Option<modal::Modal>,
    /// The pane grid's on-screen size, recorded each layout pass by the
    /// `responsive` wrapper in `view`. The layout model's px→ratio math and
    /// transplant hit-testing measure against this (a frame stale at worst;
    /// zero before the first layout, in which case pixel sizings fall back
    /// to even splits until the next rebuild).
    grid_area: std::cell::Cell<Size>,
    /// The declarative layout model this window's grid is derived from
    /// (flexible-panes plan §2.12): ordered session clusters, each a split
    /// tree. Every structural mutation lands here first; `rebuild_grid`
    /// then re-derives `grid` via `State::with_configuration`.
    layout: WindowLayout<PaneRef>,
    /// The rendered pane grid, rebuilt from `layout`. `None` while the model
    /// is empty — a `pane_grid::State` cannot represent an empty grid, and
    /// `None` is what selects the empty connect state in `view`. The session
    /// state a slot references lives in the daemon's [`SessionStore`].
    /// `pane_grid::Pane`/`Split` ids are minted fresh on every rebuild, so
    /// they must never be stored across updates (stale ids miss cleanly).
    grid: Option<pane_grid::State<PaneRef>>,
    /// Maps the current grid's divider ids back to model edges, refreshed on
    /// every rebuild — how a user divider drag writes through to the model.
    split_targets: std::collections::BTreeMap<pane_grid::Split, pane_layout::EdgeTarget>,
    /// Slots the user toggled hidden (the title-bar eye). Hidden panes drop
    /// out of the derived grid while the toolbar is collapsed; with the
    /// toolbar expanded every pane renders (hidden ones under a veil) so the
    /// toggle stays reachable. Pruned when a slot leaves this window.
    hidden_panes: std::collections::HashSet<PaneRef>,
    active_session_id: Option<SessionId>,
    /// The session that was active before the current one. Used when the
    /// active session closes: the press that clicks a pane's close button
    /// also activates that pane (pane_grid publishes `on_click` for every
    /// press), so restoring this session — not an arbitrary one — keeps
    /// keyboard focus where the user was actually working.
    previous_active_session_id: Option<SessionId>,
}

/// The pane_grid axis (and whether the new pane is the first child) for a
/// script split direction. The new pane is natively the second child; a
/// `left`/`top` placement puts it first instead.
fn direction_axis(direction: SplitDirection) -> (pane_grid::Axis, bool) {
    match direction {
        SplitDirection::Left => (pane_grid::Axis::Vertical, true),
        SplitDirection::Right => (pane_grid::Axis::Vertical, false),
        SplitDirection::Top => (pane_grid::Axis::Horizontal, true),
        SplitDirection::Bottom => (pane_grid::Axis::Horizontal, false),
    }
}

/// `event::listen_with` filter mapping an uncaptured Escape press to a message
/// tagged with the window it happened in.
fn escape_pressed(
    event: IcedEvent,
    status: iced::event::Status,
    window_id: window::Id,
) -> Option<Message> {
    match (event, status) {
        (
            IcedEvent::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Escape),
                ..
            }),
            iced::event::Status::Ignored,
        ) => Some(Message::EscapePressed(window_id)),
        _ => None,
    }
}

/// `event::listen_with` filter mapping an uncaptured `Tab` / `Shift+Tab` press to
/// a focus-traversal message. A focused `text_input`/`text_editor` does not
/// capture `Tab`, so the press arrives here as `Status::Ignored` (same path as
/// `escape_pressed`). Used to make the connect/onboarding forms keyboard-navigable.
fn tab_pressed(
    event: IcedEvent,
    status: iced::event::Status,
    window_id: window::Id,
) -> Option<Message> {
    match (event, status) {
        (
            IcedEvent::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Named(keyboard::key::Named::Tab),
                modifiers,
                ..
            }),
            iced::event::Status::Ignored,
        ) => Some(if modifiers.shift() {
            Message::FocusPrevious(window_id)
        } else {
            Message::FocusNext(window_id)
        }),
        _ => None,
    }
}

impl SmudgyWindow {
    pub fn new(window_id: window::Id, cloud: CloudHandles) -> Self {
        Self {
            window_id,
            cloud,
            toolbar_expanded: true,
            maximized: false,
            modal: None,
            grid_area: std::cell::Cell::new(Size::ZERO),
            layout: WindowLayout::new(),
            grid: None,
            split_targets: std::collections::BTreeMap::new(),
            hidden_panes: std::collections::HashSet::new(),
            active_session_id: None,
            previous_active_session_id: None,
        }
    }

    /// Re-derive the rendered grid from the layout model — called after every
    /// structural model mutation, and whenever the effective pane visibility
    /// changes. Pixel sizings resolve against the grid's current on-screen
    /// size; user-owned ratios carry verbatim. A rebuild mints fresh
    /// `Pane`/`Split` ids, so callers must not hold ids across it.
    fn rebuild_grid(&mut self) {
        // Toolbar expanded is rearrange mode: hidden panes stay in the build
        // (marked by their veil) so they can be re-shown and rearranged.
        // Collapsed, hidden panes drop out — unless every pane is hidden, in
        // which case the hidden state is ignored (an all-hidden window would
        // otherwise render the empty connect state over live sessions).
        let hidden = &self.hidden_panes;
        let show_all =
            self.toolbar_expanded || self.layout.panes().iter().all(|slot| hidden.contains(slot));
        let built = if show_all {
            self.layout
                .build(self.grid_area.get(), GRID_SPACING, GRID_MIN_SIZE)
        } else {
            self.layout
                .build_filtered(self.grid_area.get(), GRID_SPACING, GRID_MIN_SIZE, |slot| {
                    !hidden.contains(&slot)
                })
        };
        match built {
            Some((config, mirror)) => {
                let state = pane_grid::State::with_configuration(config);
                self.split_targets = pane_layout::split_targets(state.layout(), &mirror);
                self.grid = Some(state);
            }
            None => {
                self.grid = None;
                self.split_targets.clear();
            }
        }
    }

    // Hosting queries read the layout model, not the rendered grid: the grid
    // omits hidden panes while the toolbar is collapsed, but a hidden pane is
    // still hosted here (it must still re-home, close with its session, and
    // count toward the empty-window rule).

    /// Whether this window's layout holds a pane of `session_id`.
    fn hosts_session(&self, session_id: SessionId) -> bool {
        self.layout
            .panes()
            .iter()
            .any(|slot| slot.session_id == session_id)
    }

    /// Whether this window's layout holds the pane `(session_id, key)`.
    pub fn hosts_pane(&self, session_id: SessionId, key: PaneKey) -> bool {
        self.layout.contains(PaneRef { session_id, key })
    }

    /// The sessions whose MAIN pane lives in this window. Closing the window
    /// closes exactly these sessions; other sessions' script panes hosted
    /// here re-home next to their main pane instead.
    pub fn hosted_main_sessions(&self) -> Vec<SessionId> {
        let mut ids: Vec<SessionId> = self
            .layout
            .panes()
            .into_iter()
            .filter(|slot| slot.key == MAIN_PANE_KEY)
            .map(|slot| slot.session_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Every pane slot this window hosts (the daemon collects these before a
    /// window closes, to re-home surviving sessions' panes).
    pub fn pane_refs(&self) -> Vec<PaneRef> {
        self.layout.panes()
    }

    /// The sessions this window hosts any pane for.
    pub fn hosted_sessions(&self) -> Vec<SessionId> {
        let mut ids: Vec<SessionId> = self
            .layout
            .panes()
            .into_iter()
            .map(|slot| slot.session_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Drop every slot belonging to `session_id` from the layout model (and
    /// the derived grid). Returns `true` when the removal emptied the grid —
    /// the caller applies the empty-window rule.
    fn remove_session_slots(&mut self, session_id: SessionId) -> bool {
        let victims: Vec<PaneRef> = self
            .layout
            .panes()
            .into_iter()
            .filter(|slot| slot.session_id == session_id)
            .collect();
        if victims.is_empty() {
            return false;
        }
        for slot in victims {
            self.layout.remove(slot);
        }
        self.hidden_panes
            .retain(|slot| slot.session_id != session_id);
        self.rebuild_grid();
        self.grid.is_none()
    }

    /// A session was torn down (closed from this window's title bar or by the
    /// daemon's window-close cascade): drop its panes from this window's grid
    /// and repair the active-session state. Returns the follow-up task plus
    /// whether the removal emptied this window's grid — the daemon closes an
    /// emptied window unless it is the last smudgy window.
    pub fn handle_session_removed(
        &mut self,
        session_id: SessionId,
        sessions: &SessionStore,
    ) -> (Task<Message>, bool) {
        let emptied = self.remove_session_slots(session_id);

        if self.previous_active_session_id == Some(session_id) {
            self.previous_active_session_id = None;
        }

        (self.repair_active_session(sessions), emptied)
    }

    /// Re-point this window's active session when it no longer hosts a pane
    /// of the current one (its panes were removed or transplanted away).
    /// Prefer the session that was active before it: the press on a pane's
    /// close button also activates that pane (pane_grid publishes `on_click`
    /// for every press), so the closed session is usually active by the time
    /// the close lands even when the user was working elsewhere. Fall back to
    /// the lowest hosted id (deterministic where the grid's map order is not).
    pub fn repair_active_session(&mut self, sessions: &SessionStore) -> Task<Message> {
        if let Some(active) = self.active_session_id {
            if self.hosts_session(active) {
                return Task::none();
            }
            self.active_session_id = None;
        }
        let fallback = self
            .previous_active_session_id
            .filter(|id| self.hosts_session(*id))
            .or_else(|| self.hosted_sessions().into_iter().min());
        match fallback {
            Some(fallback) => self.set_active_session(fallback, sessions),
            None => Task::none(),
        }
    }

    /// Create session context information for the toolbar
    fn create_session_context(&self, sessions: &SessionStore) -> toolbar::SessionContext {
        if let Some(active_id) = self.active_session_id {
            if let Some(active_session) = sessions.get(active_id) {
                toolbar::SessionContext {
                    has_active_session: true,
                    is_connected: active_session.is_connected(),
                    server_name: active_session.server_name.clone(),
                }
            } else {
                toolbar::SessionContext::default()
            }
        } else {
            toolbar::SessionContext::default()
        }
    }

    pub fn subscription(&self) -> Subscription<Message> {
        // Session event streams are daemon-level subscriptions (sessions
        // outlive windows); only window-scoped listeners live here.
        let mut subscriptions: Vec<Subscription<Message>> = Vec::new();

        if self.modal.is_some() {
            subscriptions.push(iced::event::listen_with(escape_pressed));
            // Tab traversal for the modal's form fields. `focus_next`/
            // `focus_previous` are window-global, so this only cleanly cycles the
            // modal fields when no other focusable widgets are present (true for
            // the empty-session onboarding flow). With the modal opened over active
            // sessions, Tab could reach a session input behind it.
            subscriptions.push(iced::event::listen_with(tab_pressed));
        }

        // The window has no native frame, so maximization can only change
        // through actions that also resize it (our maximize button, OS snap,
        // Win+Up...). Refresh our cached maximized state whenever this window
        // resizes, to keep the maximize/restore button and the resize grips
        // honest.
        subscriptions.push(window::resize_events().map(|(id, _size)| Message::WindowResized(id)));

        Subscription::batch(subscriptions)
    }

    /// Flip the toolbar between expanded and collapsed. The hidden-pane
    /// filter reads this state, so the grid is re-derived whenever hidden
    /// panes exist and the state actually changes.
    fn set_toolbar_expanded(&mut self, expanded: bool) {
        if self.toolbar_expanded == expanded {
            return;
        }
        self.toolbar_expanded = expanded;
        if !self.hidden_panes.is_empty() {
            self.rebuild_grid();
        }
    }

    /// Set the active session, deactivating all others
    fn set_active_session(
        &mut self,
        session_id: SessionId,
        sessions: &SessionStore,
    ) -> Task<Message> {
        if self.active_session_id != Some(session_id) {
            self.previous_active_session_id = self.active_session_id;
        }
        self.active_session_id = Some(session_id);

        // Focus the session's input only when its main pane is in this
        // window; otherwise keyboard focus stays where it was (activation
        // never propagates across windows).
        if !self.hosts_pane(session_id, MAIN_PANE_KEY) {
            return Task::none();
        }
        if let Some(session) = sessions.get(session_id) {
            let input_id = session.input.input_id();
            operation::focus(input_id)
        } else {
            Task::none()
        }
    }

    /// Place a freshly opened script pane: split off the reference pane
    /// (falling back to the session's main pane, then a fresh top-level
    /// cluster) toward the requested direction. The pixel size request is
    /// stored on the model and resolved against the reference region's
    /// extent at every rebuild — the piece that makes placement independent
    /// of session/script creation order (§2.12). The split lands in this
    /// window because the caller (the daemon) picked the window hosting the
    /// reference pane.
    pub fn place_session_pane(
        &mut self,
        session_id: SessionId,
        key: PaneKey,
        placement: PanePlacement,
    ) {
        let slot = PaneRef { session_id, key };
        if self.layout.contains(slot) {
            return;
        }
        let (axis, new_first) = direction_axis(placement.direction);
        let sizing = placement
            .size_px
            .map_or(SplitSizing::Ratio(0.5), |px| SplitSizing::Px {
                px,
                sized_first: new_first,
            });
        let reference = PaneRef {
            session_id,
            key: placement.reference,
        };
        let main = PaneRef {
            session_id,
            key: MAIN_PANE_KEY,
        };
        let placed = self
            .layout
            .split_leaf(reference, axis, new_first, sizing, slot)
            || self.layout.split_leaf(main, axis, new_first, sizing, slot);
        if !placed {
            // Neither the reference nor the session's main pane is here (the
            // window vanished mid-flight): take an even top-level share.
            self.layout.push_cluster(slot);
        }
        self.rebuild_grid();
    }

    /// Drop one pane's slot from this window's layout. Returns `true` when
    /// the removal emptied the grid — the caller applies the empty-window
    /// rule.
    pub fn remove_pane_slot(&mut self, session_id: SessionId, key: PaneKey) -> bool {
        let slot = PaneRef { session_id, key };
        if !self.layout.remove(slot) {
            return false;
        }
        self.hidden_panes.remove(&slot);
        self.rebuild_grid();
        self.grid.is_none()
    }

    /// The slot behind one of this grid's internal pane ids (pane ids are
    /// State-internal and never reused within a State, so a stale id is a
    /// clean miss).
    pub fn pane_slot(&self, pane: pane_grid::Pane) -> Option<PaneRef> {
        self.grid
            .as_ref()
            .and_then(|grid| grid.panes.get(&pane).copied())
    }

    /// The slot's current on-screen size (logical), for sizing a torn-out
    /// window after it. `None` before the first layout pass.
    pub fn pane_size(&self, slot: PaneRef) -> Option<Size> {
        let grid = self.grid.as_ref()?;
        let area = self.grid_area.get();
        if area.width <= 0.0 || area.height <= 0.0 {
            return None;
        }
        let pane = grid
            .panes
            .iter()
            .find_map(|(pane, s)| (*s == slot).then_some(*pane))?;
        grid.layout()
            .pane_regions(GRID_SPACING, GRID_MIN_SIZE, area)
            .get(&pane)
            .map(|region| region.size())
    }

    /// Insert a transplanted pane at a drop point in this window
    /// (window-local logical coordinates relative to `window_size`, the
    /// daemon-tracked size). The grid fills the window below the
    /// toolbar/banner chrome, so its offset is the window height minus the
    /// recorded grid height; a point over the chrome or inter-pane spacing
    /// clamps to the nearest pane region. Region semantics mirror pane_grid's
    /// center-vs-edge thirds, with Center meaning "split the hovered pane
    /// along its longer axis" (the native center-swap has no cross-window
    /// analogue).
    pub fn accept_transplant(&mut self, slot: PaneRef, window_point: Point, window_size: Size) {
        let Some(grid) = self.grid.as_ref() else {
            self.layout.push_cluster(slot);
            self.rebuild_grid();
            return;
        };

        let area = self.grid_area.get();

        if area.width <= 0.0 || area.height <= 0.0 {
            // Never laid out (a window opened this same frame): take an even
            // top-level share rather than guessing at geometry.
            self.layout.push_cluster(slot);
            self.rebuild_grid();
            return;
        }

        let offset_y = (window_size.height - area.height).max(0.0);
        let point = Point::new(
            window_point.x.clamp(0.0, area.width - 1.0),
            (window_point.y - offset_y).clamp(0.0, area.height - 1.0),
        );
        let regions = grid
            .layout()
            .pane_regions(GRID_SPACING, GRID_MIN_SIZE, area);
        let hovered = regions
            .iter()
            .find(|(_, region)| region.contains(point))
            .or_else(|| {
                // The clamp can land on inter-pane spacing: take the region
                // whose center is nearest.
                regions.iter().min_by(|(_, a), (_, b)| {
                    point
                        .distance(a.center())
                        .total_cmp(&point.distance(b.center()))
                })
            })
            .map(|(pane, region)| (*pane, *region));
        let Some((target, bounds)) = hovered else {
            log::warn!("No drop region for a transplanted pane; grid unchanged");
            return;
        };
        let region = pane_drag::region_for(bounds, point);
        self.insert_split(slot, target, region, bounds.size());
    }

    /// Split the model leaf behind the grid pane `target` to make room for
    /// `slot`, per the drop region, then rebuild. From here on the moved
    /// pane's position is user-owned (it may sit inside another session's
    /// cluster — cluster coherence governs automatic placement only).
    /// `bounds` is the target pane's current extent (only the Center axis
    /// choice reads it).
    fn insert_split(
        &mut self,
        slot: PaneRef,
        target: pane_grid::Pane,
        region: pane_drag::DropRegion,
        bounds: Size,
    ) {
        let Some(target_slot) = self.pane_slot(target) else {
            log::warn!(
                "No layout leaf for the drop target of {} {}",
                slot.session_id,
                slot.key
            );
            return;
        };
        let (axis, new_first) = match region {
            pane_drag::DropRegion::Left => (pane_grid::Axis::Vertical, true),
            pane_drag::DropRegion::Right => (pane_grid::Axis::Vertical, false),
            pane_drag::DropRegion::Top => (pane_grid::Axis::Horizontal, true),
            pane_drag::DropRegion::Bottom => (pane_grid::Axis::Horizontal, false),
            pane_drag::DropRegion::Center => (
                if bounds.width >= bounds.height {
                    pane_grid::Axis::Vertical
                } else {
                    pane_grid::Axis::Horizontal
                },
                false,
            ),
        };
        if !self
            .layout
            .split_leaf(target_slot, axis, new_first, SplitSizing::Ratio(0.5), slot)
        {
            log::warn!(
                "Failed to split a layout leaf for transplanted pane {} {}",
                slot.session_id,
                slot.key
            );
            return;
        }
        self.rebuild_grid();
    }

    /// Apply a native in-window drop to the layout model, then rebuild.
    /// Center-on-pane swaps the two leaves (matching pane_grid's native
    /// semantics); a pane-edge drop splits the hovered leaf evenly; a
    /// whole-grid edge drop lands at the top level — a new leading/trailing
    /// cluster for Left/Right, a wrap of the entire layout for Top/Bottom.
    fn apply_native_drop(&mut self, pane: pane_grid::Pane, target: pane_grid::Target) {
        let Some(dragged) = self.pane_slot(pane) else {
            return;
        };
        match target {
            pane_grid::Target::Pane(target_pane, region) => {
                let Some(target_slot) = self.pane_slot(target_pane) else {
                    return;
                };
                if target_slot == dragged {
                    return;
                }
                match region {
                    pane_grid::Region::Center => self.layout.swap(dragged, target_slot),
                    pane_grid::Region::Edge(edge) => {
                        let (axis, new_first) = match edge {
                            pane_grid::Edge::Left => (pane_grid::Axis::Vertical, true),
                            pane_grid::Edge::Right => (pane_grid::Axis::Vertical, false),
                            pane_grid::Edge::Top => (pane_grid::Axis::Horizontal, true),
                            pane_grid::Edge::Bottom => (pane_grid::Axis::Horizontal, false),
                        };
                        self.layout.remove(dragged);
                        if !self.layout.split_leaf(
                            target_slot,
                            axis,
                            new_first,
                            SplitSizing::Ratio(0.5),
                            dragged,
                        ) {
                            // The target was the removed leaf's only sibling
                            // and collapsed away — never lose the pane.
                            self.layout.push_cluster(dragged);
                        }
                    }
                }
            }
            pane_grid::Target::Edge(edge) => {
                self.layout.remove(dragged);
                match edge {
                    pane_grid::Edge::Left => self.layout.insert_cluster_front(dragged),
                    pane_grid::Edge::Right => self.layout.push_cluster(dragged),
                    pane_grid::Edge::Top => {
                        self.layout
                            .wrap_all(pane_grid::Axis::Horizontal, true, dragged);
                    }
                    pane_grid::Edge::Bottom => {
                        self.layout
                            .wrap_all(pane_grid::Axis::Horizontal, false, dragged);
                    }
                }
            }
        }
        self.rebuild_grid();
    }

    /// Seed a freshly opened tear-out window with its single transplanted
    /// pane. The toolbar starts collapsed: the window exists to show the
    /// pane, not the first-run connect flow.
    pub fn adopt_torn_out_pane(&mut self, slot: PaneRef) {
        debug_assert!(
            self.grid.is_none(),
            "tear-out windows start with an empty grid"
        );
        self.layout.push_cluster(slot);
        self.rebuild_grid();
        self.set_toolbar_expanded(false);
    }

    /// Open a new session for `server_name`/`profile_name` with its pane in
    /// this window (the window whose connect modal launched it — necessarily
    /// the focused one), make it active, collapse the toolbar, and dismiss
    /// the modal. `auto_connect` selects online (connect once the runtime is
    /// ready) vs offline. The session state itself lives in the daemon's
    /// store; this window only takes the pane.
    fn open_session(
        &mut self,
        server_name: String,
        profile_name: String,
        auto_connect: bool,
        sessions: &mut SessionStore,
    ) -> Task<Message> {
        let session_id = sessions.open_session(server_name, profile_name, auto_connect);

        // The new session's main pane becomes a new top-level cluster,
        // dividing the window evenly against the existing session clusters
        // (§2.12) — deterministic regardless of whether other sessions'
        // scripts have created their panes yet.
        self.layout.push_cluster(PaneRef {
            session_id,
            key: MAIN_PANE_KEY,
        });
        self.rebuild_grid();

        // Set this as the active session (will deactivate others)
        let focus_task = self.set_active_session(session_id, sessions);

        self.set_toolbar_expanded(false);
        self.modal = None;

        focus_task
    }

    pub fn update(
        &mut self,
        message: Message,
        sessions: &mut SessionStore,
    ) -> Update<Message, Event> {
        match message {
            Message::ToolbarAction(action) => match action {
                toolbar::Message::ToggleExpand => {
                    self.set_toolbar_expanded(!self.toolbar_expanded);
                    Update::none()
                }
                toolbar::Message::ConnectPressed => {
                    // `opening()` loads servers + the first server's profiles
                    // synchronously so the modal renders fully populated (no
                    // loading-state flash).
                    let connect_state = modal::connect::State::opening();
                    let new_modal = modal::Modal::Connect(connect_state);
                    let modal_init_task: Task<modal::Message> = new_modal.initial_task();
                    self.modal = Some(new_modal);
                    Update::with_task(modal_init_task.map(Message::ModalMessage))
                }
                toolbar::Message::SettingsPressed => Update::with_event(Event::OpenSettingsWindow),
                toolbar::Message::DragWindow => Update::with_task(window::drag(self.window_id)),
                toolbar::Message::MinimizePressed => {
                    Update::with_task(window::minimize(self.window_id, true))
                }
                toolbar::Message::ToggleMaximizePressed => {
                    Update::with_task(window::toggle_maximize(self.window_id))
                }
                toolbar::Message::ClosePressed => {
                    // Cleanup happens in main.rs via window::close_events()
                    Update::with_task(window::close(self.window_id))
                }
                toolbar::Message::AutomationsPressed => {
                    // Only allow automation actions when there's an active session
                    if let Some(active_id) = self.active_session_id {
                        if let Some(active_session) = sessions.get(active_id) {
                            Update::with_event(Event::CreateNewScriptEditorWindow {
                                server_name: Arc::new(active_session.server_name.clone()),
                                session_id: active_id,
                            })
                        } else {
                            log::warn!(
                                "Active session ID {} not found in the session store",
                                active_id
                            );
                            Update::none()
                        }
                    } else {
                        log::info!("AutomationsPressed ignored - no active session");
                        Update::none()
                    }
                }
                toolbar::Message::MapEditorPressed => {
                    if let Some(active_id) = self.active_session_id {
                        if let Some(active_session) = sessions.get(active_id) {
                            active_session
                                .mapper
                                .as_ref()
                                .map(|mapper| {
                                    Update::with_event(Event::CreateNewMapEditorWindow {
                                        mapper: mapper.clone(),
                                        server_name: Arc::new(active_session.server_name.clone()),
                                    })
                                })
                                .unwrap_or_else(Update::none)
                        } else {
                            log::warn!(
                                "Active session ID {} not found in the session store",
                                active_id
                            );
                            Update::none()
                        }
                    } else {
                        log::info!("AutomationsPressed ignored - no active session");
                        Update::none()
                    }
                }
            },
            Message::ModalMessage(msg) => {
                if let Some(m) = self.modal.as_mut() {
                    let (task, event) = m.update(msg);
                    if let Some(evt) = event {
                        return self.update(Message::ModalEvent(evt), sessions);
                    }
                    Update::with_task(task.map(Message::ModalMessage))
                } else {
                    Update::none()
                }
            }
            Message::ModalEvent(event) => match event {
                modal::Event::Connect(connect_event) => match connect_event {
                    modal::ConnectEvent::CloseModalRequested => {
                        self.modal = None;
                        Update::none()
                    }
                    modal::ConnectEvent::Connect(server_name, profile_name) => {
                        log::info!("Connect requested for {profile_name} on {server_name}");
                        Update::with_task(self.open_session(
                            server_name,
                            profile_name,
                            true,
                            sessions,
                        ))
                    }
                    modal::ConnectEvent::OpenOffline(server_name, profile_name) => {
                        log::info!("Open offline requested for {profile_name} on {server_name}");
                        Update::with_task(self.open_session(
                            server_name,
                            profile_name,
                            false,
                            sessions,
                        ))
                    }
                },
            },
            Message::CloseModal => {
                self.modal = None;
                Update::none()
            }
            Message::EscapePressed(window_id) => {
                if window_id == self.window_id {
                    self.modal = None;
                }
                Update::none()
            }
            Message::FocusNext(window_id) => {
                if window_id == self.window_id {
                    Update::with_task(operation::focus_next())
                } else {
                    Update::none()
                }
            }
            Message::FocusPrevious(window_id) => {
                if window_id == self.window_id {
                    Update::with_task(operation::focus_previous())
                } else {
                    Update::none()
                }
            }
            Message::ResizeGripPressed(direction) => {
                Update::with_task(window::drag_resize(self.window_id, direction))
            }
            Message::WindowResized(window_id) => {
                if window_id == self.window_id {
                    Update::with_task(
                        window::is_maximized(self.window_id).map(Message::SetMaximized),
                    )
                } else {
                    Update::none()
                }
            }
            Message::SetMaximized(maximized) => {
                self.maximized = maximized;
                Update::none()
            }
            Message::SetActiveSession(session_id) => {
                let focus_task = self.set_active_session(session_id, sessions);
                Update::with_task(focus_task)
            }
            Message::SessionPaneUserAction { session_id, msg } => match msg {
                // Session teardown is the daemon's job: it removes the store
                // entry first, then cleans every window's grid (this one
                // included) and applies the empty-window rule.
                session_store::Message::Close => {
                    Update::with_event(Event::CloseSession(session_id))
                }
                session_store::Message::SetMapperCurrentLocation(area_id, room_number) => {
                    // Keep the session's own map widgets in step, and bubble
                    // up for the standalone map editor windows.
                    let task = sessions
                        .get_mut(session_id)
                        .map(|session| {
                            session
                                .update(session_store::Message::SetMapperCurrentLocation(
                                    area_id,
                                    room_number,
                                ))
                                .map(move |pane_msg| Message::SessionPaneUserAction {
                                    session_id,
                                    msg: pane_msg,
                                })
                        })
                        .unwrap_or_else(Task::none);
                    Update::new(
                        task,
                        Some(Event::SetMapperCurrentLocation(area_id, room_number)),
                    )
                }
                msg => {
                    if let Some(session) = sessions.get_mut(session_id) {
                        Update::with_task(session.update(msg).map(move |pane_msg| {
                            Message::SessionPaneUserAction {
                                session_id,
                                msg: pane_msg,
                            }
                        }))
                    } else {
                        // The session was torn down with this action already
                        // in flight; dropping it is the designed behavior.
                        log::debug!("Dropping action for closed session {session_id}");
                        Update::none()
                    }
                }
            },
            Message::PaneClicked(pane) => {
                let clicked_session = self
                    .grid
                    .as_ref()
                    .and_then(|grid| grid.panes.get(&pane))
                    .map(|slot| slot.session_id);
                // A stale pane can reach this handler when a close processed
                // earlier in the same update batch removed it; the lookup
                // guard makes that a no-op.
                if let Some(session_id) = clicked_session {
                    // Clicking into a pane returns to the distraction-free
                    // state — except while headers are toolbar-gated and the
                    // toolbar is expanded (rearrange mode): pane_grid
                    // publishes `on_click` for every press, including the one
                    // that begins a header drag, so collapsing here would
                    // hide every drag handle mid-gesture. Rearrange mode ends
                    // via the toolbar toggle instead.
                    if !(crate::prefs::current().hide_pane_headers && self.toolbar_expanded) {
                        self.set_toolbar_expanded(false);
                    }
                    // Re-activating the already-active session would run the
                    // focus operation again, stealing keyboard focus from any
                    // focusable overlay widget the user just clicked into
                    // (pane_grid publishes `on_click` for every press, even
                    // ones a child widget captured).
                    if self.active_session_id == Some(session_id) {
                        Update::none()
                    } else {
                        Update::with_task(self.set_active_session(session_id, sessions))
                    }
                } else {
                    Update::none()
                }
            }
            Message::PaneDragged(pane_grid::DragEvent::Picked { pane }) => {
                match self.pane_slot(pane) {
                    Some(slot) => Update::with_event(Event::PaneDragPicked { pane, slot }),
                    None => Update::none(),
                }
            }
            Message::PaneDragged(pane_grid::DragEvent::Dropped { pane, target }) => {
                // In-window drops apply to the layout model (then rebuild),
                // so drag and deterministic placement share one source of
                // truth; from here on the pane's position is user-owned.
                self.apply_native_drop(pane, target);
                Update::with_event(Event::PaneDragEnded)
            }
            Message::PaneDragged(pane_grid::DragEvent::Canceled { pane }) => {
                // No local layout change — but the release may have landed in
                // another window (or on the desktop); the daemon decides.
                Update::with_event(Event::PaneDragCanceled(pane))
            }
            Message::PaneResized(pane_grid::ResizeEvent { split, ratio }) => {
                // Applied natively (no rebuild — a divider drag emits a
                // stream of these) and mirrored into the model, where it
                // converts the edge to a user-owned ratio.
                if let Some(grid) = self.grid.as_mut() {
                    grid.resize(split, ratio);
                }
                if let Some(target) = self.split_targets.get(&split) {
                    let target = target.clone();
                    self.layout.set_split_ratio(&target, ratio);
                }
                Update::none()
            }
            Message::TogglePaneVisibility(slot) => {
                if !self.hidden_panes.remove(&slot) {
                    self.hidden_panes.insert(slot);
                }
                // With the toolbar expanded the toggle only changes the veil
                // (the grid renders everything); collapsed — reachable when
                // the pane's header is pinned or the global hide setting is
                // off — the pane leaves or rejoins the grid immediately.
                if !self.toolbar_expanded {
                    self.rebuild_grid();
                }
                Update::none()
            }
            Message::OpenSettingsPressed => Update::with_event(Event::OpenSettingsWindow),
            Message::OpenDownloadPage => Update::with_event(Event::OpenDownloadPage),
            Message::DismissUpgrade => Update::with_event(Event::DismissUpgrade),
            Message::DismissUpgradeForVersion => {
                Update::with_event(Event::DismissUpgradeForVersion)
            }
        }
    }

    pub fn view<'a>(&'a self, sessions: &'a SessionStore) -> ThemedElement<'a, Message> {
        let session_context = self.create_session_context(sessions);
        let toolbar_element =
            toolbar::view(self.toolbar_expanded, self.maximized, &session_context);

        // Header-visibility rule (§2.11): a pane's title bar is attached only
        // when its policy pins it, the toolbar is expanded, or the global
        // hide setting is off. A headerless pane renders body-only and is not
        // draggable (pane_grid's pick area needs a title bar) — dividers
        // still resize it; expanding the toolbar restores rearranging.
        let hide_headers = crate::prefs::current().hide_pane_headers;

        let main_content_area: ThemedElement<Message> = if self.grid.is_some() {
            // The responsive wrapper records the grid's on-screen size each
            // layout pass; the layout model's px->ratio math and transplant
            // hit-testing measure against it.
            iced::widget::responsive(move |size| {
                self.grid_area.set(size);
                let grid = self
                    .grid
                    .as_ref()
                    .expect("the grid presence was checked before building the view");
                let panes = PaneGrid::new(grid, |_pane, slot, _is_maximized| {
                    let session_id = slot.session_id;
                    let Some(session) = sessions.get(session_id) else {
                        // A slot must always reference a live session; render the
                        // desync as an empty pane rather than panicking mid-frame.
                        debug_assert!(false, "grid slot references unknown session {session_id}");
                        return pane_grid::Content::new(iced::widget::Space::new());
                    };
                    let is_active = self.active_session_id == Some(session_id);
                    let is_hidden = self.hidden_panes.contains(slot);
                    let wrap = move |msg| Message::SessionPaneUserAction { session_id, msg };

                    let body: ThemedElement<'_, Message> = if slot.key == MAIN_PANE_KEY {
                        session.pane_body().map(wrap)
                    } else {
                        session.script_pane_body(slot.key).map(wrap)
                    };
                    // A hidden pane that still renders (toolbar expanded, or
                    // every pane hidden) is marked, not removed: a veil of
                    // the window background with a red ✕, showing what
                    // collapsing the toolbar will drop.
                    let body: ThemedElement<'_, Message> = if is_hidden {
                        let veil = container(
                            svg(assets::hero_icons::X_MARK.clone())
                                .width(24)
                                .height(24)
                                .style(|theme: &crate::Theme, _| svg::Style {
                                    color: Some(theme.styles.text.error),
                                }),
                        )
                        .width(Length::Fill)
                        .height(Length::Fill)
                        .align_x(Horizontal::Center)
                        .align_y(Vertical::Center)
                        .style(theme::builtins::container::pane_hidden_overlay);
                        stack(vec![body, veil.into()]).into()
                    } else {
                        body
                    };
                    let content = pane_grid::Content::new(body);

                    let show_header = session.title_bar_policy(slot.key)
                        == TitleBarPolicy::AlwaysShow
                        || self.toolbar_expanded
                        || !hide_headers;
                    if !show_header {
                        return content;
                    }

                    let visibility_button = session_store::title_bar_icon_button(
                        if is_hidden {
                            assets::hero_icons::EYE_SLASH.clone()
                        } else {
                            assets::hero_icons::EYE.clone()
                        },
                        Message::TogglePaneVisibility(*slot),
                    );
                    // The split resize grab band reaches (spacing + leeway) / 2
                    // - spacing / 2 = 4px past the pane edge into its content;
                    // the padding keeps the buttons clear of it so an edge press
                    // on a control cannot start a divider resize instead.
                    let controls_padding = iced::Padding {
                        top: 3.0,
                        right: 6.0,
                        bottom: 0.0,
                        left: 0.0,
                    };
                    // The active session's panes tint their whole header band
                    // (the pre-pane UI carried this on the session tab).
                    let bar_style = move |theme: &crate::Theme| {
                        if is_active {
                            theme::builtins::container::pane_title_bar_active(theme)
                        } else {
                            theme::builtins::container::pane_title_bar(theme)
                        }
                    };

                    // The title bar is the pane's drag handle: pane_grid's pick
                    // area is the bar minus the content and controls bounds, so
                    // the content must stay intrinsic-width. Main panes carry the
                    // session header + connect/close controls; script panes a slim
                    // name bar (no close — script panes die only by `pane.close()`
                    // or session end) plus the visibility eye.
                    let title_bar = if slot.key == MAIN_PANE_KEY {
                        let controls = container(
                            row![session.title_controls().map(wrap), visibility_button]
                                .spacing(8)
                                .align_y(Vertical::Center),
                        )
                        .padding(controls_padding);
                        pane_grid::TitleBar::new(session.title_content(is_active).map(wrap))
                            .controls(pane_grid::Controls::new(controls))
                            .always_show_controls()
                            .padding(2)
                            .style(bar_style)
                    } else {
                        let controls =
                            container(row![visibility_button].spacing(8).align_y(Vertical::Center))
                                .padding(controls_padding);
                        pane_grid::TitleBar::new(session.script_pane_title(slot.key).map(wrap))
                            .controls(pane_grid::Controls::new(controls))
                            .always_show_controls()
                            .padding(2)
                            .style(bar_style)
                    };

                    content.title_bar(title_bar)
                })
                .width(Length::Fill)
                .height(Length::Fill)
                .spacing(GRID_SPACING)
                .on_click(Message::PaneClicked)
                .on_drag(Message::PaneDragged)
                .on_resize(8, Message::PaneResized);

                panes.into()
            })
            .into()
        } else {
            // Empty session: an actionable empty state — icon chip, heading,
            // one-line subtext, and a single primary action that opens the Connect
            // modal (so first-run users don't have to discover the menu bar).
            let chip = container(
                text(assets::bootstrap_icons::LIGHTNING)
                    .font(assets::fonts::BOOTSTRAP_ICONS)
                    .size(28)
                    .style(theme::builtins::text::muted),
            )
            .width(Length::Fixed(64.0))
            .height(Length::Fixed(64.0))
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center)
            .style(theme::builtins::container::icon_chip);

            container(
                column![
                    chip,
                    text(crate::i18n::t!("shell-no-sessions")).size(22),
                    text(crate::i18n::t!("shell-connect-help"))
                        .style(theme::builtins::text::muted),
                    iced::widget::button(
                        text(crate::i18n::t!("shell-connect-action"))
                            .font(assets::fonts::GEIST_VF)
                    )
                        .style(theme::builtins::button::primary)
                        .padding([10, 22])
                        .on_press(Message::ToolbarAction(toolbar::Message::ConnectPressed)),
                ]
                .spacing(16)
                .align_x(Horizontal::Center),
            )
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(Horizontal::Center)
            .align_y(Vertical::Center)
            .into()
        };

        // "Verify your email" banner: shown while the signed-in account is
        // unverified, since friends/sharing/sync are gated server-side.
        let snapshot = self.cloud.snapshot.get();
        let banner: Option<ThemedElement<Message>> = snapshot.show_verify_banner().then(|| {
            container(
                row![
                    text(crate::i18n::t!("shell-verify-email")).size(13),
                    iced::widget::button(text(crate::i18n::t!("shell-open-settings")).size(12))
                        .style(theme::builtins::button::secondary)
                        .padding([2, 8])
                        .on_press(Message::OpenSettingsPressed),
                ]
                .spacing(12)
                .align_y(Vertical::Center),
            )
            .width(Length::Fill)
            .padding([6, 12])
            .style(theme::builtins::container::modal_title_bar)
            .into()
        });

        // "Out of date" banner: shown once the cloud rejects this build as too
        // old (HTTP 426). Carries a click-to-open download link.
        let upgrade_banner: Option<ThemedElement<Message>> =
            snapshot.show_upgrade_banner().then(|| {
                container(
                    row![
                        text(crate::i18n::t!("shell-client-outdated"))
                        .size(13),
                        iced::widget::button(
                            text(crate::i18n::t!(
                                "shell-download-at",
                                "url" => crate::DOWNLOAD_URL
                            ))
                            .size(12),
                        )
                        .style(theme::builtins::button::secondary)
                        .padding([2, 8])
                        .on_press(Message::OpenDownloadPage),
                    ]
                    .spacing(12)
                    .align_y(Vertical::Center),
                )
                .width(Length::Fill)
                .padding([6, 12])
                .style(theme::builtins::container::modal_title_bar)
                .into()
            });

        let mut layout = column![toolbar_element.map(Message::ToolbarAction)];
        if let Some(banner) = banner {
            layout = layout.push(banner);
        }
        if let Some(upgrade_banner) = upgrade_banner {
            layout = layout.push(upgrade_banner);
        }
        let main_layout: ThemedElement<_> = layout
            .push(main_content_area)
            .width(Length::Fill)
            .height(Length::Fill)
            .into();

        let main_layout: ThemedElement<Message> = if let Some(modal) = &self.modal {
            let modal_view = modal.view().map(Message::ModalMessage);
            stack(vec![
                main_layout,
                opaque(
                    mouse_area(
                        center(opaque(modal_view)).style(theme::builtins::container::overlay),
                    )
                    .on_press(Message::CloseModal),
                ),
            ])
            .into()
        } else {
            main_layout
        };

        // Soft "upgrade available" popup: a weaker, dismissable overlay shown
        // when the server signaled a newer version (snapshot.upgrade_prompt).
        let main_layout: ThemedElement<Message> = if let Some(version) = snapshot.upgrade_prompt() {
            let popup = container(
                column![
                    text(crate::i18n::t!("shell-update-available")).size(18),
                    text(crate::i18n::t!("shell-update-ready", "version" => version)).size(13),
                    iced::widget::button(text(crate::i18n::t!("shell-visit-download")).size(13))
                        .style(theme::builtins::button::primary)
                        .padding([8, 18])
                        .on_press(Message::OpenDownloadPage),
                    text(crate::DOWNLOAD_URL).size(11),
                    row![
                        iced::widget::button(text(crate::i18n::t!("shell-remind-later")).size(12))
                            .style(theme::builtins::button::secondary)
                            .padding([6, 12])
                            .on_press(Message::DismissUpgrade),
                        iced::widget::button(text(crate::i18n::t!("shell-skip-version")).size(12))
                            .style(theme::builtins::button::link)
                            .padding([6, 12])
                            .on_press(Message::DismissUpgradeForVersion),
                    ]
                    .spacing(10)
                    .align_y(Vertical::Center),
                ]
                .spacing(14)
                .align_x(Horizontal::Center)
                .width(Length::Fill),
            )
            .width(Length::Fixed(380.0))
            .padding(24)
            .style(theme::builtins::container::modal_card);
            stack(vec![
                main_layout,
                opaque(
                    mouse_area(center(opaque(popup)).style(theme::builtins::container::overlay))
                        // Click the backdrop to dismiss — the gentle, session-only
                        // dismissal (not the permanent "skip this version").
                        .on_press(Message::DismissUpgrade),
                ),
            ])
            .into()
        } else {
            main_layout
        };

        if self.maximized {
            // No resize grips while maximized; the OS rejects resizing anyway
            // and the strips would steal clicks at the screen edges.
            main_layout
        } else {
            stack(vec![
                main_layout,
                resize_grips::view(Message::ResizeGripPressed),
            ])
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
        }
    }
}
