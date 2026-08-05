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
    MAIN_PANE_KEY, PaneKey, PanePlacement, SplitDirection, TabPosition, TitleBarPolicy,
};

use rustc_hash::FxHashMap;

use crate::{
    assets,
    cloud_account::CloudHandles,
    components::{self, modal, resize_grips, tab_strip, toolbar},
    pane_drag::{self, ClassifiedTarget, DropRegion, GridEdgeSide, TabDrag},
    pane_groups::{self, GroupId, GroupLayout, SplitSizing, Tab, TabId},
    session_store::{self, SessionStore},
    theme::{self, Element as ThemedElement},
    update::Update,
    widgets::{self, drag_overlay, tab_press},
    workspace::TemplateSource,
    workspace::binding,
    workspace::restore::{DescriptorKey, PendingPane, SessionVacancy, VacantPane},
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
    PaneResized(pane_grid::ResizeEvent),
    /// The title-bar eye toggle: flip a pane between visible and hidden.
    /// Hidden is a soft display state — the pane's session keeps running;
    /// the slot just leaves the derived grid while the toolbar is collapsed.
    TogglePaneVisibility(PaneRef),
    /// A user action on a group's tab strip (select, drag transitions,
    /// connection toggle, close, visibility, scroll mirror).
    TabStrip(GroupId, tab_strip::Event),
    /// `Ctrl+Tab` / `Ctrl+Shift+Tab`: cycle tab selection within the focus
    /// group. Carries the originating window so only that window reacts.
    CycleTab {
        window_id: window::Id,
        backwards: bool,
    },
    /// QA-only (debug builds, `SMUDGY_TAB_GROUPS_DEV=1`): `Ctrl+Shift+G`
    /// merges the focus group into its neighbor group — a keyboard fallback
    /// for forming multi-tab groups. Compiled out of release builds.
    #[cfg(debug_assertions)]
    DevMergeGroup(window::Id),
    OpenSettingsPressed,
    /// A user template apply (named layout or last-session restore) found
    /// live sessions the template omits: route the keep-or-close questions
    /// into this window's Layouts modal (reopening it if the user already
    /// dismissed it — the answer is required before anything mutates). An
    /// occupied modal surface parks the prompt until it frees up rather
    /// than clobbering it.
    PromptLayoutAnswers {
        server: String,
        source: TemplateSource,
        rows: Vec<modal::layouts::OmittedRow>,
    },
    /// The daemon finished (or failed) a layout save on this window's
    /// behalf: annotate the open Layouts modal.
    LayoutSaveOutcome(modal::layouts::SaveOutcome),
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
    /// The user clicked a pane's title-bar eyeball. The window already
    /// flipped its local state optimistically; the daemon reports the toggle
    /// to the pane's session runtime (`PaneUserHidden`), which owns the def —
    /// the echoed `PaneUpdated` then converges every consumer.
    PaneVisibilityToggled {
        slot: PaneRef,
        hidden: bool,
    },
    /// A tab's press surface was pressed (true press point, window-local
    /// logical). Not yet a drag: the daemon records the press candidate —
    /// it owns the gesture from here, so a widget subtree rebuilt
    /// mid-gesture cannot lose it — and refreshes stale window
    /// origins/scales so a drag that follows hit-tests fresh geometry.
    TabDragPressed {
        tab: TabId,
        slot: PaneRef,
        group: GroupId,
        point: Point,
    },
    /// A tab press crossed the drag deadband: the drag is live. The daemon
    /// records it (tab identity + pane binding + source group snapshot) and
    /// switches to full cursor tracking.
    TabDragStarted {
        tab: TabId,
        slot: PaneRef,
        group: GroupId,
        press: Point,
        point: Point,
    },
    /// The dragged tab's press surface saw the release — the fast path.
    /// `None` means the cursor position was unavailable: a cancel, never a
    /// fabricated origin. The daemon's drag-gated subscription remains the
    /// authoritative terminal for releases this widget never sees.
    TabDragReleased {
        point: Option<Point>,
    },
    /// The drag ended without a usable release (source-window capture
    /// loss); the daemon cancels with zero mutation.
    TabDragCanceled {
        reason: &'static str,
    },
    OpenSettingsWindow,
    OpenDownloadPage,
    DismissUpgrade,
    DismissUpgradeForVersion,
    /// Capture `server`'s window footprint and save it as the named layout
    /// `name` (the daemon owns capture — it spans windows). The acting
    /// session is this window's active one; `server` is its server.
    SaveLayout {
        server: String,
        name: String,
    },
    /// Apply the named layout `name` from `server`'s store — the full user
    /// flow (spawn missing slots, keep-or-close for omitted sessions, may
    /// create windows).
    ApplyLayout {
        server: String,
        name: String,
    },
    /// Restore `server`'s last-session snapshot (the connect surface's
    /// per-server affordance) — the same full user flow a named-layout
    /// apply runs, reading the template from `<server>/last-session.json`.
    RestoreLastSession {
        server: String,
    },
    /// Re-run a prompted apply with every omitted session answered.
    ApplyLayoutWithAnswers {
        server: String,
        source: TemplateSource,
        close: Vec<SessionId>,
        keep: Vec<SessionId>,
    },
    /// Release the session's retained slot geometry (vacancies and pending
    /// placeholders) and re-place its panes from current script
    /// definitions.
    ResetSessionLayout(SessionId),
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

/// Identify the one input a tab selection displaced. Script selection only
/// blurs a pane actually obscured by the tab change; an ordinary chrome
/// re-selection also releases the prior focus group when activation moves.
fn displaced_input_for_selection(
    request_focus: bool,
    rendered: Option<PaneRef>,
    previously_rendered: Option<PaneRef>,
    previously_focused: Option<PaneRef>,
) -> Option<PaneRef> {
    if rendered != previously_rendered {
        previously_rendered
    } else if request_focus {
        previously_focused
    } else {
        None
    }
}

/// One group's rendered pane before and after a pane payload exchange —
/// `None` on either side when the group rendered nothing (every member tab
/// hidden under a collapsed toolbar, or unbound).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RenderedSlotChange {
    before: Option<PaneRef>,
    after: Option<PaneRef>,
}

/// Active-session identity follows the rendered position through a pane
/// swap. iced widget focus is positional: whatever focus lived at a group's
/// selected slot belongs, after the swap, to the pane now rendering there.
/// So for each involved group, when the swap changed which pane renders at
/// that slot and the pane previously on screen there belonged to the active
/// session, activation moves to the newly rendered pane's session —
/// whatever kind of pane that is, exactly as if its tab had been selected.
/// A slot whose rendered pane did not change moves no attention, so a swap
/// between two off-screen panes changes nothing; a slot that now renders
/// nothing offers no session to follow, so activation stays put and the
/// caller's hosting repair settles it.
fn active_after_pane_swap(
    active: Option<SessionId>,
    changes: &[RenderedSlotChange],
) -> Option<SessionId> {
    let current = active?;
    for change in changes {
        if change.before == change.after {
            continue;
        }
        if let Some(before) = change.before
            && before.session_id == current
        {
            return Some(change.after.map_or(current, |after| after.session_id));
        }
    }
    active
}

/// A pre-swap snapshot of the rendered-slot facts a pane payload exchange
/// can change in one window: each involved group and the pane it rendered
/// before the exchange. Captured by
/// [`SmudgyWindow::pane_swap_render_probe`] before the model mutates and
/// consumed by [`SmudgyWindow::settle_active_session_after_pane_swap`]
/// once every half of the exchange (payloads and hidden state) has landed.
#[derive(Debug, Default)]
pub struct PaneSwapRenderProbe {
    groups: smallvec::SmallVec<[(GroupId, Option<PaneRef>); 2]>,
}

/// The strip position keyboard cycling lands on: the next (or previous) slot
/// from `current`, wrapping around the strip.
fn cycle_index(current: usize, len: usize, backwards: bool) -> usize {
    debug_assert!(len > 0);
    if backwards {
        (current + len - 1) % len
    } else {
        (current + 1) % len
    }
}

/// The pane grid's inter-pane spacing (must match the `.spacing()` set on the
/// `PaneGrid` widget — `pane_regions` and the layout model's px→ratio math
/// need the real value).
const GRID_SPACING: f32 = 4.0;

/// pane_grid's default minimum pane size (no `.min_size()` override is set).
const GRID_MIN_SIZE: f32 = 50.0;

/// The pane title bar's `.padding(2)` contribution to the header band (top
/// plus bottom), added to the measured strip height when classifying header
/// drops.
const TITLE_BAR_PADDING: f32 = 4.0;

/// The window's view of the daemon-owned drag state, handed in per view
/// pass. Windows keep no drag flags of their own: everything drag-shaped in
/// the view (temporary header bands, press-surface resets, the target
/// overlay) derives from this.
#[derive(Debug, Clone, Copy, Default)]
pub struct DragViewContext<'a> {
    /// A pane drag is live somewhere in the app.
    pub live: bool,
    /// Window-level keyboard modifier state for the tab press surfaces.
    pub modifiers: keyboard::Modifiers,
    /// The classified target under the cursor when this window is the
    /// hovered one — exactly what the drop will apply.
    pub target: Option<&'a ClassifiedTarget>,
}

/// The strip-facing identity of an unbound tab: the label its durable
/// record supplies and the stored eyeball preference it will apply when its
/// pane arrives. Placeholders carry no session, so the connection facts a
/// bound descriptor derives all render as absent.
struct PlaceholderDescriptor {
    label: String,
    hidden: bool,
}

/// The pane name a durable descriptor stands for, namespace-independent —
/// the placeholder tab's strip label.
fn descriptor_pane_name(key: &DescriptorKey) -> &str {
    match key {
        DescriptorKey::User { name } | DescriptorKey::Package { name, .. } => name,
    }
}

/// A keep-or-close prompt waiting for the modal surface to free up.
struct PendingLayoutPrompt {
    server: String,
    source: TemplateSource,
    rows: Vec<modal::layouts::OmittedRow>,
}

pub struct SmudgyWindow {
    window_id: window::Id,
    cloud: CloudHandles,
    toolbar_expanded: bool,
    maximized: bool,
    modal: Option<modal::Modal>,
    /// A keep-or-close prompt that arrived while the modal surface was
    /// otherwise occupied (a Connect form mid-use, or the Layouts menu deep
    /// in a rename/save stage). Parked rather than clobbering: nothing
    /// mutates until the questions are answered, so the prompt can wait, and
    /// the apply plan is revalidated against the live workspace when the
    /// answers come back — a parked prompt can never act on stale state.
    /// Delivered by `deliver_pending_layout_prompt` the moment the surface
    /// frees up; a newer prompt replaces an older parked one (the older
    /// apply's plan would fail revalidation anyway).
    pending_layout_prompt: Option<PendingLayoutPrompt>,
    /// The pane grid's on-screen size, recorded each layout pass by the
    /// `responsive` wrapper in `view`. The layout model's px→ratio math and
    /// transplant hit-testing measure against this (a frame stale at worst;
    /// zero before the first layout, in which case pixel sizings fall back
    /// to even splits until the next rebuild).
    grid_area: std::cell::Cell<Size>,
    /// The declarative layout model this window's grid is derived from:
    /// ordered session clusters, each a split tree whose leaves are stable
    /// tab groups. Every structural mutation lands here first;
    /// `rebuild_grid` then re-derives `grid` via `State::with_configuration`.
    layout: GroupLayout<PaneRef>,
    /// Each hosted pane's tab id — the O(1) bridge between the daemon's
    /// `PaneRef`-based interfaces and the group model's tab identities.
    /// Maintained by every hosting/removal path in lockstep with `layout`;
    /// its key set is exactly the set of panes this window hosts.
    bindings: FxHashMap<PaneRef, TabId>,
    /// The rendered pane grid, rebuilt from `layout`. `None` while the model
    /// is empty — a `pane_grid::State` cannot represent an empty grid, and
    /// `None` is what selects the empty connect state in `view`. The payload
    /// is the stable group id; which pane renders inside a slot is resolved
    /// against the model at view time. The session state a slot references
    /// lives in the daemon's [`SessionStore`]. `pane_grid::Pane`/`Split` ids
    /// are minted fresh on every rebuild, so they must never be stored
    /// across updates (stale ids miss cleanly).
    grid: Option<pane_grid::State<GroupId>>,
    /// Structural model mutations mark this instead of rebuilding the grid
    /// eagerly; [`Self::flush_grid_rebuild`] re-derives the grid once per
    /// update cycle, so a composite daemon operation (relocate, swap,
    /// tear-out) costs one rebuild however many mutations it lands.
    /// Between the first mutation of a cycle and the flush, `grid` is
    /// stale: within-cycle logic reads the model, and the grid-derived
    /// answers (`pane_size`, `pane_slot`) are read before the cycle's
    /// first mutation.
    grid_dirty: bool,
    /// Persisted state changed since the daemon's last workspace sweep:
    /// every structural mutation plus the grid-invisible ones (tab
    /// selection, eyeball state, the active session). Collected once per
    /// update cycle by [`Self::take_workspace_dirty`]; a mutation's entire
    /// persistence cost is this boolean store.
    workspace_dirty: bool,
    /// Maps the current grid's divider ids back to model edges, refreshed on
    /// every rebuild — how a user divider drag writes through to the model.
    /// An `EdgeTarget` is only valid against the rebuild that emitted it;
    /// this map is re-derived after every structural mutation so no stale
    /// target can be applied.
    split_targets: std::collections::BTreeMap<pane_grid::Split, pane_groups::EdgeTarget>,
    /// Slots the user toggled hidden (the title-bar eye). Hidden panes drop
    /// out of the derived grid while the toolbar is collapsed; with the
    /// toolbar expanded every pane renders (hidden ones under a veil) so the
    /// toggle stays reachable. Pruned when a slot leaves this window.
    hidden_panes: std::collections::HashSet<PaneRef>,
    /// The group keyboard tab-cycling operates on: the group of the last
    /// clicked pane or selected tab. Falls back to the active session's main
    /// group when unset or stale.
    focus_group: Option<GroupId>,
    /// Stable widget ids for each group's tab strip (the scroll anchor the
    /// reveal task targets), cached so the view path doesn't re-allocate
    /// them every pass. Pruned on rebuild.
    strip_ids: std::cell::RefCell<FxHashMap<GroupId, iced::advanced::widget::Id>>,
    /// Stable widget ids for each tab's strip anchor, cached like
    /// `strip_ids`.
    tab_anchors: std::cell::RefCell<FxHashMap<TabId, iced::advanced::widget::Id>>,
    /// Each group's last measured on-screen size (logical), recorded
    /// whenever the group is present in the derived grid. This is what lets
    /// [`Self::pane_size`] honor the stale-until-rendered size contract for
    /// panes whose group has left the grid (hidden under a collapsed
    /// toolbar). Entries retire with their group.
    group_sizes: std::cell::RefCell<FxHashMap<GroupId, Size>>,
    active_session_id: Option<SessionId>,
    /// The session that was active before the current one. Used when the
    /// active session closes: the press that clicks a pane's close button
    /// also activates that pane (pane_grid publishes `on_click` for every
    /// press), so restoring this session — not an arbitrary one — keeps
    /// keyboard focus where the user was actually working.
    previous_active_session_id: Option<SessionId>,
    /// Draw-time strip-band mirror: each group's tab-strip bounds
    /// (window-local), recorded every paint. Supplies the header band
    /// height for drag classification. Pruned with its group.
    strip_bands: std::cell::RefCell<FxHashMap<GroupId, iced::Rectangle>>,
    /// Draw-time tab-span mirror: each tab's bounds in its strip's content
    /// space (un-scrolled). Paired with [`Self::strip_scroll`] to compute
    /// on-screen spans for insertion-slot classification. Pruned with its
    /// tab.
    tab_spans: std::cell::RefCell<FxHashMap<TabId, iced::Rectangle>>,
    /// Each strip's current horizontal scroll offset, mirrored from its
    /// scrollable's `on_scroll`. Zero until a strip first scrolls.
    strip_scroll: std::cell::RefCell<FxHashMap<GroupId, f32>>,
    /// Placeholder tabs awaiting materialization, keyed by the owning
    /// session and the pane's durable descriptor. A `PaneOpened` matching
    /// an entry binds that tab in place (the placeholder's position wins
    /// over the script's placement request); the rest are reaped when the
    /// session's runtime reports ready. Pruned with their tabs.
    pending_panes: std::collections::HashMap<(SessionId, DescriptorKey), PendingPane>,
    /// Vacated session slots homed in this window (their main pane lived
    /// here): retained placeholder arrangements a later session open in
    /// this window adopts, exact profile first, then lowest ordinal.
    /// Runtime-only — snapshots never carry vacancies.
    vacancies: Vec<SessionVacancy>,
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

/// The pane_grid axis (and new-first flag) for a body-edge drop region.
/// `Center` never reaches a split; the arm exists for totality only.
fn split_axis(region: DropRegion) -> (pane_grid::Axis, bool) {
    match region {
        DropRegion::Left => (pane_grid::Axis::Vertical, true),
        DropRegion::Right | DropRegion::Center => (pane_grid::Axis::Vertical, false),
        DropRegion::Top => (pane_grid::Axis::Horizontal, true),
        DropRegion::Bottom => (pane_grid::Axis::Horizontal, false),
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

/// `event::listen_with` filter for keyboard tab cycling: an uncaptured
/// `Ctrl+Tab` (forward) / `Ctrl+Shift+Tab` (backward) selects the next tab
/// in the focus group.
fn cycle_tab_pressed(
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
        ) if modifiers.control() => Some(Message::CycleTab {
            window_id,
            backwards: modifiers.shift(),
        }),
        _ => None,
    }
}

/// QA-only (debug builds, `SMUDGY_TAB_GROUPS_DEV=1`): whether the QA
/// affordance for forming multi-tab groups without a drag is enabled.
/// Compiled out of release builds — the drag gesture is the product surface.
#[cfg(debug_assertions)]
fn tab_groups_dev_enabled() -> bool {
    static ENABLED: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| std::env::var("SMUDGY_TAB_GROUPS_DEV").is_ok_and(|v| v == "1"));
    *ENABLED
}

/// QA-only: `event::listen_with` filter for the dev-gated group merge
/// (`Ctrl+Shift+G`).
#[cfg(debug_assertions)]
fn dev_merge_pressed(
    event: IcedEvent,
    status: iced::event::Status,
    window_id: window::Id,
) -> Option<Message> {
    match (event, status) {
        (
            IcedEvent::Keyboard(keyboard::Event::KeyPressed {
                key: keyboard::Key::Character(c),
                modifiers,
                ..
            }),
            iced::event::Status::Ignored,
        ) if modifiers.control() && modifiers.shift() && c.as_str().eq_ignore_ascii_case("g") => {
            Some(Message::DevMergeGroup(window_id))
        }
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

/// The overlay rectangles for a classified drop target: the highlight the
/// drop will occupy, plus the insertion caret for header targets. The
/// geometry comes verbatim from the classification the drop applies, so the
/// feedback can never disagree with the committed operation; the theme
/// resolves each role's paint at draw time.
fn target_overlay_rects(target: Option<&ClassifiedTarget>) -> Vec<drag_overlay::OverlayRect> {
    let Some(target) = target else {
        return Vec::new();
    };
    let mut rects = vec![drag_overlay::OverlayRect {
        bounds: target.highlight,
        role: drag_overlay::OverlayRole::Target,
    }];
    if let Some(caret) = target.caret {
        rects.push(drag_overlay::OverlayRect {
            bounds: caret,
            role: drag_overlay::OverlayRole::Caret,
        });
    }
    rects
}

impl SmudgyWindow {
    pub fn new(window_id: window::Id, cloud: CloudHandles) -> Self {
        Self {
            window_id,
            cloud,
            toolbar_expanded: true,
            maximized: false,
            modal: None,
            pending_layout_prompt: None,
            grid_area: std::cell::Cell::new(Size::ZERO),
            layout: GroupLayout::new(),
            bindings: FxHashMap::default(),
            grid: None,
            grid_dirty: false,
            workspace_dirty: false,
            split_targets: std::collections::BTreeMap::new(),
            hidden_panes: std::collections::HashSet::new(),
            focus_group: None,
            strip_ids: std::cell::RefCell::new(FxHashMap::default()),
            tab_anchors: std::cell::RefCell::new(FxHashMap::default()),
            group_sizes: std::cell::RefCell::new(FxHashMap::default()),
            active_session_id: None,
            previous_active_session_id: None,
            strip_bands: std::cell::RefCell::new(FxHashMap::default()),
            tab_spans: std::cell::RefCell::new(FxHashMap::default()),
            strip_scroll: std::cell::RefCell::new(FxHashMap::default()),
            pending_panes: std::collections::HashMap::new(),
            vacancies: Vec::new(),
        }
    }

    /// Whether every group renders regardless of the hidden set. Toolbar
    /// expanded is rearrange mode: hidden panes stay in the build (marked by
    /// their veil) so they can be re-shown and rearranged. Collapsed, hidden
    /// panes drop out — unless every pane is hidden, in which case the hidden
    /// state is ignored (an all-hidden window would otherwise render the
    /// empty connect state over live sessions).
    fn show_all(&self) -> bool {
        self.toolbar_expanded
            || self
                .bindings
                .keys()
                .all(|slot| self.hidden_panes.contains(slot))
    }

    /// The pane a group currently renders: its effective selection under the
    /// window's visibility mode. `None` for an unknown group, a fully hidden
    /// group, or a selection resolving to an unbound placeholder tab.
    fn rendered_slot_with(&self, group: GroupId, show_all: bool) -> Option<PaneRef> {
        let tab = self.layout.effective_selected(group, |tab| {
            tab.binding()
                .is_some_and(|slot| show_all || !self.hidden_panes.contains(slot))
        })?;
        self.layout.tab(tab)?.binding().copied()
    }

    /// [`Self::rendered_slot_with`] under the current visibility mode.
    /// Public because it answers the drop question for a body-center swap:
    /// what the user sees in a group is what swaps (rendered, not
    /// durably-selected).
    pub fn rendered_slot(&self, group: GroupId) -> Option<PaneRef> {
        self.rendered_slot_with(group, self.show_all())
    }

    /// The layout model, read-only — what the workspace snapshot walks
    /// (worth-persisting derives from `layout().is_empty()`, never grid
    /// presence).
    pub fn layout(&self) -> &GroupLayout<PaneRef> {
        &self.layout
    }

    /// The window's active session, if any — persisted explicitly per
    /// window so restore never depends on repair preferences.
    #[must_use]
    pub fn active_session_id(&self) -> Option<SessionId> {
        self.active_session_id
    }

    /// Whether the window is currently maximized (as mirrored from resize
    /// events for the frameless chrome).
    #[must_use]
    pub fn is_maximized(&self) -> bool {
        self.maximized
    }

    // ------------------------------------------------------------------
    // Workspace restoration and vacancy maintenance
    // ------------------------------------------------------------------

    /// Bind a freshly opened pane onto its waiting placeholder, if this
    /// window staged one for `(session, descriptor)`. The placeholder's
    /// position and sizing stand — the slot's stored geometry wins over the
    /// script's placement request. Returns the stored eyeball-hidden
    /// preference to replay; `None` means no placeholder matched here and
    /// normal placement should proceed.
    pub fn claim_pending_pane(
        &mut self,
        session_id: SessionId,
        descriptor: &DescriptorKey,
        key: PaneKey,
    ) -> Option<bool> {
        let pending = self.pending_panes.get(&(session_id, descriptor.clone()))?;
        let tab = pending.tab;
        let hidden = pending.hidden;
        let slot = PaneRef { session_id, key };
        if !self.layout.set_binding(tab, Some(slot)) {
            // The tab is gone (or the binding is somehow already hosted):
            // drop the stale entry and let normal placement handle the pane.
            self.pending_panes.remove(&(session_id, descriptor.clone()));
            return None;
        }
        self.pending_panes.remove(&(session_id, descriptor.clone()));
        self.bindings.insert(slot, tab);
        if hidden {
            self.hidden_panes.insert(slot);
        }
        self.mark_grid_dirty();
        Some(hidden)
    }

    /// The pending record standing behind an unbound placeholder tab, if
    /// this window staged one: the owning session, the durable descriptor
    /// the pane will bind by, and the stored eyeball-hidden preference —
    /// everything a snapshot needs to describe the placeholder while its
    /// pane is still materializing. Placeholders retained for a *vacancy*
    /// (no live session) have no record here and stay undescribable, so
    /// closed stays closed by omission.
    #[must_use]
    pub fn pending_pane_for_tab(&self, tab: TabId) -> Option<(SessionId, &DescriptorKey, bool)> {
        self.pending_panes
            .iter()
            .find(|(_, pending)| pending.tab == tab)
            .map(|((session, key), pending)| (*session, key, pending.hidden))
    }

    /// What the tab strip shows for an unbound tab: the durable record
    /// standing behind a *pending* placeholder — a live session's
    /// not-yet-materialized pane, which is visible content. `None` for
    /// everything else, vacancy tabs above all: a vacancy is invisible
    /// bookkeeping (retained geometry a later open may adopt), never
    /// user-visible in any mode, so the strip omits it exactly as snapshots
    /// do.
    fn placeholder_descriptor(&self, tab: TabId) -> Option<PlaceholderDescriptor> {
        let (_, key, hidden) = self.pending_pane_for_tab(tab)?;
        Some(PlaceholderDescriptor {
            label: descriptor_pane_name(key).to_string(),
            hidden,
        })
    }

    /// Whether `tab` is listed in its group's strip under the given
    /// visibility mode ([`Self::show_all`]). Rearrange mode lists every
    /// content tab — a bound pane, or a pending placeholder a live
    /// session still owes — with hidden ones dimmed behind their eyes for
    /// recovery. Otherwise the strip mirrors the grid: eyeball-hidden
    /// entries (bound panes and pre-hidden placeholders alike) drop out,
    /// so a collapsed toolbar never advertises a tab whose pane is not
    /// rendering. Vacancy tabs and recordless strays are invisible
    /// bookkeeping in every mode.
    fn tab_in_strip(&self, tab: &Tab<PaneRef>, show_all: bool) -> bool {
        match tab.binding() {
            Some(slot) => show_all || !self.hidden_panes.contains(slot),
            None => self
                .pending_pane_for_tab(tab.id())
                .is_some_and(|(_, _, hidden)| show_all || !hidden),
        }
    }

    /// Whether a group emits its header band (the tab strip), given how
    /// many tabs its strip lists ([`Self::tab_in_strip`]). A strip
    /// listing more than one tab always shows — tab selection must not
    /// depend on header preferences — and the count is of listed tabs,
    /// not members, so a group whose other members are all eyeball-hidden
    /// under a collapsed toolbar renders exactly like a true singleton
    /// under the §2.11 header-visibility rule: body-only when the global
    /// hide setting is on, the toolbar is collapsed, and the rendered
    /// pane's policy does not pin its bar. Two overrides remain: a live
    /// drag mounts a temporary band on every eligible target (the merge
    /// surface must exist to be droppable — safe, because pane_grid keeps
    /// the body subtree at a fixed child slot, so its state survives),
    /// and a group rendering no bound pane keeps its strip (the
    /// placeholder descriptors are the only cue for what the
    /// otherwise-empty body is holding space for).
    fn strip_emitted(
        listed_tabs: usize,
        renders_bound_pane: bool,
        policy_pins_bar: bool,
        toolbar_expanded: bool,
        hide_headers: bool,
        drag_live: bool,
    ) -> bool {
        listed_tabs > 1
            || !renders_bound_pane
            || policy_pins_bar
            || toolbar_expanded
            || !hide_headers
            || drag_live
    }

    /// Whether nothing in this window is visible content — the layout is
    /// empty or holds only vacancy tabs. Such a window renders the
    /// no-active-sessions view, and the emptied-window rule treats it as
    /// emptied: a secondary all-vacancy window closes (dropping its
    /// vacancies — adoption is window-local, so records in a closing window
    /// could never be adopted again anyway), while the last window stays
    /// open as the keep-alive connect surface with its vacancies retained
    /// invisibly for later opens to adopt.
    #[must_use]
    pub fn is_visually_empty(&self) -> bool {
        self.layout
            .panes()
            .iter()
            .all(|tab| tab.binding().is_none() && self.pending_pane_for_tab(tab.id()).is_none())
    }

    /// Drop every placeholder still waiting on `session_id` — the panes
    /// that never materialized (missing, renamed, or no longer authorized).
    /// Their leaves collapse exactly like closed panes; the rest of the
    /// layout is untouched. Returns whether the reap emptied this window.
    pub fn reap_session_placeholders(&mut self, session_id: SessionId) -> bool {
        let victims: Vec<TabId> = self
            .pending_panes
            .iter()
            .filter(|((owner, _), _)| *owner == session_id)
            .map(|(_, pending)| pending.tab)
            .collect();
        if victims.is_empty() {
            return false;
        }
        self.pending_panes
            .retain(|(owner, _), _| *owner != session_id);
        for tab in victims {
            let _reaped = self.layout.remove_tab(tab);
        }
        self.mark_grid_dirty();
        self.is_visually_empty()
    }

    /// Vacate a closing session's slot: its tabs stay in place as unbound
    /// vacancy tabs (geometry retained — the mix-and-match contract), and
    /// the window hosting its main pane records the vacancy a later open
    /// here can adopt. Vacancy tabs are invisible bookkeeping: they neither
    /// list in strips nor render as bodies. `descriptors` names the
    /// session's script panes (captured before the store entry was
    /// removed); a pane without one cannot be re-matched and is removed
    /// instead of stranded. Runtime state only — the next snapshot simply
    /// omits the unbound tabs.
    ///
    /// Returns whether the vacate left this window visually empty (no
    /// bound or pending tab remains — only invisible vacancies, if
    /// anything); the caller applies the emptied-window rule, under which
    /// the last window stays open showing the connect view with its
    /// vacancies intact.
    pub fn vacate_session(
        &mut self,
        session_id: SessionId,
        server: &str,
        profile: &str,
        descriptors: &std::collections::HashMap<PaneKey, DescriptorKey>,
        ordinal: u64,
    ) -> bool {
        let slots: Vec<PaneRef> = self
            .bindings
            .keys()
            .filter(|slot| slot.session_id == session_id)
            .copied()
            .collect();
        if slots.is_empty() {
            return false;
        }
        let mut main_tab = None;
        let mut panes = Vec::new();
        let mut doomed = Vec::new();
        for slot in slots {
            let Some(tab) = self.bindings.remove(&slot) else {
                continue;
            };
            let hidden = self.hidden_panes.remove(&slot);
            if slot.key == MAIN_PANE_KEY {
                self.layout.set_binding(tab, None);
                main_tab = Some(tab);
            } else if let Some(key) = descriptors.get(&slot.key) {
                self.layout.set_binding(tab, None);
                panes.push(VacantPane {
                    key: key.clone(),
                    tab,
                    hidden,
                });
            } else {
                doomed.push(tab);
            }
        }
        if let Some(main_tab) = main_tab {
            self.vacancies.push(SessionVacancy {
                server: server.to_string(),
                profile: profile.to_string(),
                ordinal,
                main_tab,
                panes,
            });
        } else {
            // The session's main lives elsewhere: script tabs here have no
            // adoptable home, so they close rather than strand.
            doomed.extend(panes.into_iter().map(|pane| pane.tab));
        }
        for tab in doomed {
            let _closed = self.layout.remove_tab(tab);
        }
        self.mark_grid_dirty();
        // The session is gone from the store; repair quietly (there is no
        // input to focus).
        self.repair_active_session_without_focus();
        self.is_visually_empty()
    }

    /// Give a session opening in this window the best vacant slot, if one
    /// of its server exists: exact server+profile first, then same-server
    /// lowest ordinal — the binding engine's rules over this window's
    /// vacancies only (opens-where-you-asked). Adoption rebinds the
    /// retained main tab in place and stages the vacancy's script panes as
    /// this session's placeholders. Returns whether a slot was adopted
    /// (otherwise the caller uses normal placement).
    fn adopt_vacancy(&mut self, session_id: SessionId, server: &str, profile: &str) -> bool {
        if self.vacancies.is_empty() {
            return false;
        }
        let mut order: Vec<usize> = (0..self.vacancies.len()).collect();
        order.sort_by_key(|&index| self.vacancies[index].ordinal);
        let slots: Vec<binding::SlotDescriptor<'_>> = order
            .iter()
            .map(|&index| binding::SlotDescriptor {
                server: &self.vacancies[index].server,
                profile: &self.vacancies[index].profile,
            })
            .collect();
        let live = [binding::LiveSession {
            id: session_id,
            server,
            profile,
        }];
        let bound = binding::bind(&slots, &live);
        let Some(position) = bound.iter().position(Option::is_some) else {
            return false;
        };
        let vacancy = self.vacancies.remove(order[position]);
        let main = PaneRef {
            session_id,
            key: MAIN_PANE_KEY,
        };
        if !self.layout.set_binding(vacancy.main_tab, Some(main)) {
            // The retained main tab vanished under the vacancy: nothing to
            // adopt. Its sibling placeholders close with it.
            for pane in vacancy.panes {
                let _closed = self.layout.remove_tab(pane.tab);
            }
            self.mark_grid_dirty();
            return false;
        }
        self.bindings.insert(main, vacancy.main_tab);
        // The adopted main becomes its group's selection — the fresh
        // session should be visible where its slot was.
        self.layout.select(vacancy.main_tab);
        for pane in vacancy.panes {
            self.pending_panes.insert(
                (session_id, pane.key),
                PendingPane {
                    tab: pane.tab,
                    hidden: pane.hidden,
                },
            );
        }
        self.mark_grid_dirty();
        true
    }

    /// Release every vacancy this window retains for `server`/`profile`:
    /// the records drop and their retained placeholder tabs close, so a
    /// later open falls through to normal placement instead of adopting
    /// stale geometry. Half of the Reset action (the other half re-places
    /// the live session's panes from current definitions). Returns whether
    /// the release emptied this window.
    pub fn release_vacancies(&mut self, server: &str, profile: &str) -> bool {
        let mut doomed: Vec<TabId> = Vec::new();
        self.vacancies.retain(|vacancy| {
            if vacancy.server == server && vacancy.profile == profile {
                doomed.push(vacancy.main_tab);
                doomed.extend(vacancy.panes.iter().map(|pane| pane.tab));
                false
            } else {
                true
            }
        });
        if doomed.is_empty() {
            return false;
        }
        for tab in doomed {
            let _closed = self.layout.remove_tab(tab);
        }
        self.mark_grid_dirty();
        self.is_visually_empty()
    }

    /// Replace this window's entire arrangement with a realized layout-
    /// apply plan: the new layout (bound panes, pending placeholders, and
    /// vacancy tabs already in their final positions), the placeholder
    /// registry, the vacancy records, and the eyeball-hidden set — each
    /// swapped wholesale, since every tab of the previous arrangement
    /// retires with it. The window may be mid-life: the rebuild re-mints
    /// every grid id, which is what cancels any stale interaction identity
    /// referencing the old model.
    ///
    /// `active` is the plan's explicit choice (a user restore applies the
    /// template's active slot); `None` preserves the current active session
    /// when the new arrangement still hosts it and repairs quietly when it
    /// does not.
    pub fn install_applied_layout(
        &mut self,
        layout: GroupLayout<PaneRef>,
        pending: Vec<(SessionId, DescriptorKey, PendingPane)>,
        vacancies: Vec<SessionVacancy>,
        hidden: Vec<PaneRef>,
        active: Option<SessionId>,
    ) {
        self.bindings = layout
            .panes()
            .iter()
            .filter_map(|tab| tab.binding().map(|slot| (*slot, tab.id())))
            .collect();
        self.layout = layout;
        self.hidden_panes = hidden.into_iter().collect();
        self.pending_panes = pending
            .into_iter()
            .map(|(session, key, pane)| ((session, key), pane))
            .collect();
        self.vacancies = vacancies;
        self.focus_group = None;
        match active {
            Some(session) => {
                if self.active_session_id != Some(session) {
                    self.previous_active_session_id = self.active_session_id;
                    self.active_session_id = Some(session);
                }
            }
            None => {
                if self
                    .active_session_id
                    .is_none_or(|current| !self.hosts_session(current))
                {
                    self.repair_active_session_without_focus();
                }
            }
        }
        self.mark_grid_dirty();
    }

    /// Whether the drag's validation snapshot `(tab, slot, group)` still
    /// resolves in this window: the pane is hosted here, bound to exactly
    /// that tab, and the tab still sits in its pick-time group. The
    /// terminal re-resolution every drop runs before mutating — a tab
    /// re-bound, re-hosted, or re-grouped mid-drag is stale identity.
    pub fn drag_tab_resolves(&self, tab: TabId, slot: PaneRef, group: GroupId) -> bool {
        self.tab_of(slot) == Some(tab) && self.layout.group_of(tab) == Some(group)
    }

    /// One-line structural description for drag diagnostics: groups in
    /// model order, member panes in strip order, `*` marking each group's
    /// durable selection. Drag-terminal logging only — never a hot path.
    pub fn describe_layout(&self) -> String {
        use std::fmt::Write as _;
        let mut out = String::new();
        for group in self.layout.groups_depth_first() {
            let selected = self.layout.selected(group);
            out.push('[');
            if let Some(tabs) = self.layout.tabs(group) {
                for (index, tab) in tabs.iter().enumerate() {
                    if index > 0 {
                        out.push(' ');
                    }
                    match tab.binding() {
                        Some(slot) => {
                            let _ = write!(out, "{}/{}", slot.session_id, slot.key);
                        }
                        None => out.push_str("placeholder"),
                    }
                    if selected == Some(tab.id()) {
                        out.push('*');
                    }
                }
            }
            out.push(']');
        }
        out
    }

    /// The tab hosting `slot` in this window.
    fn tab_of(&self, slot: PaneRef) -> Option<TabId> {
        self.bindings.get(&slot).copied()
    }

    /// The group hosting `slot` in this window.
    fn group_of_slot(&self, slot: PaneRef) -> Option<GroupId> {
        self.layout.group_of(self.tab_of(slot)?)
    }

    /// The stable scroll-anchor id of a group's tab strip.
    fn strip_id(&self, group: GroupId) -> iced::advanced::widget::Id {
        self.strip_ids
            .borrow_mut()
            .entry(group)
            .or_insert_with(|| {
                iced::advanced::widget::Id::from(format!("pane-tab-strip-{}", group.as_u64()))
            })
            .clone()
    }

    /// The mirrored horizontal scroll offset of one group's strip — zero for
    /// a strip that has never scrolled. This is the number that grounds
    /// strip-content-space points into window space
    /// ([`tab_strip::ground_to_window`]) and shifts tab spans for drag
    /// classification.
    fn strip_scroll_offset(&self, group: GroupId) -> f32 {
        self.strip_scroll
            .borrow()
            .get(&group)
            .copied()
            .unwrap_or(0.0)
    }

    /// The stable strip-anchor id of one tab (what the reveal task finds).
    fn tab_anchor(&self, tab: TabId) -> iced::advanced::widget::Id {
        self.tab_anchors
            .borrow_mut()
            .entry(tab)
            .or_insert_with(|| {
                iced::advanced::widget::Id::from(format!("pane-tab-{}", tab.as_u64()))
            })
            .clone()
    }

    /// Record that the layout model (or the effective pane visibility) has
    /// diverged from the rendered grid. The rebuild itself is deferred to
    /// [`Self::flush_grid_rebuild`] so every mutation an update cycle lands
    /// coalesces into one re-derivation. Everything that reshapes the grid
    /// also changes what the workspace mirror would persist, so the
    /// workspace mark rides along here.
    fn mark_grid_dirty(&mut self) {
        self.grid_dirty = true;
        self.workspace_dirty = true;
    }

    /// Record a persisted-state change that does not reshape the grid
    /// (selection, eyeball-hidden bookkeeping, the active session).
    fn mark_workspace_dirty(&mut self) {
        self.workspace_dirty = true;
    }

    /// Collect and clear this window's workspace-dirty mark. The daemon
    /// sweeps every window once per update cycle and folds the answers into
    /// the autosave schedule — the O(1)-per-mutation half of the
    /// persistence contract lives in the two boolean stores above.
    pub fn take_workspace_dirty(&mut self) -> bool {
        std::mem::take(&mut self.workspace_dirty)
    }

    /// Re-derive the grid if any mutation marked it dirty — the once-per-
    /// update-cycle rebuild point (the daemon flushes every window at the
    /// end of each update, and grid-reading paths inside a cycle flush
    /// before reading). Coalescing cannot disturb pane_grid's positional
    /// state re-pairing: intermediate grids were never rendered — `view`
    /// runs only after the cycle — so the order the next view re-pairs
    /// subtree state against is the final model's visible order either
    /// way.
    pub fn flush_grid_rebuild(&mut self) {
        if self.grid_dirty {
            self.grid_dirty = false;
            self.rebuild_grid();
        }
    }

    /// Re-derive the rendered grid from the layout model. Pixel sizings
    /// resolve against the grid's current on-screen size; user-owned ratios
    /// carry verbatim. A rebuild mints fresh `Pane`/`Split` ids and
    /// re-derives the divider→edge map, so no caller may hold grid ids or
    /// edge targets across it. Reached only through
    /// [`Self::flush_grid_rebuild`].
    fn rebuild_grid(&mut self) {
        // Structural mutations retire groups and tabs; their cached widget
        // ids and the keyboard focus anchor retire with them.
        self.strip_ids
            .borrow_mut()
            .retain(|group, _| self.layout.contains_group(*group));
        self.tab_anchors
            .borrow_mut()
            .retain(|tab, _| self.layout.contains_tab(*tab));
        // Groups filtered out of the grid keep their last measured size (the
        // stale-until-rendered contract needs it); only retired groups drop.
        self.group_sizes
            .borrow_mut()
            .retain(|group, _| self.layout.contains_group(*group));
        // Drag-geometry mirrors retire with their group/tab. Values for
        // still-live entries stay (a frame stale at worst; repainted before
        // the next hit-test).
        self.strip_bands
            .borrow_mut()
            .retain(|group, _| self.layout.contains_group(*group));
        self.strip_scroll
            .borrow_mut()
            .retain(|group, _| self.layout.contains_group(*group));
        self.tab_spans
            .borrow_mut()
            .retain(|tab, _| self.layout.contains_tab(*tab));
        // Restoration bookkeeping retires with its tabs: a placeholder or
        // vacancy whose tab left the model can never be claimed again.
        self.pending_panes
            .retain(|_, pending| self.layout.contains_tab(pending.tab));
        self.vacancies
            .retain(|vacancy| self.layout.contains_tab(vacancy.main_tab));
        for vacancy in &mut self.vacancies {
            vacancy
                .panes
                .retain(|pane| self.layout.contains_tab(pane.tab));
        }
        #[cfg(debug_assertions)]
        self.debug_validate_bindings();
        if let Some(group) = self.focus_group
            && !self.layout.contains_group(group)
        {
            self.focus_group = None;
        }
        // Vacancy tabs are invisible bookkeeping in EVERY mode: no build
        // ever admits an unbound tab without a pending record, so a group
        // (or window) of nothing but vacancies drops out of the grid — an
        // all-vacancy window therefore has no grid and renders the
        // no-active-sessions view. Pending placeholders are visible content
        // and render whenever the mode admits unbound tabs at all.
        let hidden = &self.hidden_panes;
        let pending = &self.pending_panes;
        let is_content = |tab: &Tab<PaneRef>| {
            tab.binding().is_some() || pending.values().any(|record| record.tab == tab.id())
        };
        let built = if self.show_all() {
            self.layout.build_filtered(
                self.grid_area.get(),
                GRID_SPACING,
                GRID_MIN_SIZE,
                is_content,
            )
        } else {
            self.layout
                .build_filtered(self.grid_area.get(), GRID_SPACING, GRID_MIN_SIZE, |tab| {
                    tab.binding().is_some_and(|slot| !hidden.contains(slot))
                })
        };
        match built {
            Some((config, mirror)) => {
                let state = pane_grid::State::with_configuration(config);
                self.split_targets = pane_groups::split_targets(state.layout(), &mirror);
                self.grid = Some(state);
            }
            None => {
                self.grid = None;
                self.split_targets.clear();
            }
        }
        // Diagnostic: a rebuild re-mints pane ids, so pane_grid re-pairs
        // every pane subtree fresh — any in-flight press in this window
        // dies here. Rebuilds are structural-mutation-only by design; one
        // appearing mid-gesture in the log is the smoking gun.
        log::info!("[pane-drag] grid rebuilt in {:?}", self.window_id);
    }

    /// Debug-only invariant check: `bindings` and the layout model agree —
    /// every bound tab's pane maps back to exactly that tab, and no binding
    /// outlives its tab. One pass over the strips; compiled out of release
    /// builds.
    #[cfg(debug_assertions)]
    fn debug_validate_bindings(&self) {
        let mut bound = 0usize;
        for group in self.layout.groups_depth_first() {
            for tab in self.layout.tabs(group).unwrap_or(&[]) {
                if let Some(slot) = tab.binding() {
                    bound += 1;
                    debug_assert_eq!(
                        self.bindings.get(slot),
                        Some(&tab.id()),
                        "binding for {slot:?} does not point at the tab carrying it"
                    );
                }
            }
        }
        debug_assert_eq!(
            bound,
            self.bindings.len(),
            "bindings map holds entries for panes the model does not carry"
        );
    }

    // Hosting queries read the layout model, not the rendered grid: the grid
    // omits hidden panes while the toolbar is collapsed, but a hidden pane is
    // still hosted here (it must still re-home, close with its session, and
    // count toward the empty-window rule).

    /// Every hosted pane in deterministic model order (cluster by cluster,
    /// depth-first, strip order within each group). Placeholder tabs carry no
    /// pane and are skipped. Allocates — for the rare daemon-driven walks,
    /// never the per-frame view path.
    fn slots_in_order(&self) -> Vec<PaneRef> {
        self.layout
            .groups_depth_first()
            .into_iter()
            .filter_map(|group| self.layout.tabs(group))
            .flatten()
            .filter_map(|tab| tab.binding().copied())
            .collect()
    }

    /// Whether this window's layout holds a pane of `session_id`.
    fn hosts_session(&self, session_id: SessionId) -> bool {
        self.bindings
            .keys()
            .any(|slot| slot.session_id == session_id)
    }

    /// Whether this window's layout holds the pane `(session_id, key)`.
    pub fn hosts_pane(&self, session_id: SessionId, key: PaneKey) -> bool {
        self.bindings.contains_key(&PaneRef { session_id, key })
    }

    /// The sessions whose MAIN pane lives in this window. Closing the window
    /// closes exactly these sessions; other sessions' script panes hosted
    /// here re-home next to their main pane instead.
    pub fn hosted_main_sessions(&self) -> Vec<SessionId> {
        let mut ids: Vec<SessionId> = self
            .slots_in_order()
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
        self.slots_in_order()
    }

    /// The sessions this window hosts any pane for.
    pub fn hosted_sessions(&self) -> Vec<SessionId> {
        let mut ids: Vec<SessionId> = self
            .slots_in_order()
            .into_iter()
            .map(|slot| slot.session_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }

    /// Remove one pane's tab from the model, settling the bindings map. The
    /// removed tab — the only remaining handle on its identity — is returned
    /// to the caller: hand it to another hosting operation, or drop it when
    /// the pane genuinely leaves this window (the daemon re-hosts through its
    /// `PaneRef` interfaces, minting a fresh tab in the destination).
    fn take_slot(&mut self, slot: PaneRef) -> Option<Tab<PaneRef>> {
        let tab = self.bindings.remove(&slot)?;
        let taken = self.layout.remove_tab(tab);
        debug_assert!(taken.is_some(), "bindings map desynced from the model");
        taken
    }

    /// Host `tab` as a new singleton top-level cluster, recording its
    /// binding. A rejected tab is already hosted here (its identity is
    /// already in the model), so dropping the duplicate handle loses nothing.
    fn host_as_cluster(&mut self, tab: Tab<PaneRef>) -> Option<GroupId> {
        let binding = tab.binding().copied();
        let id = tab.id();
        match self.layout.push_cluster(tab) {
            Ok(group) => {
                if let Some(slot) = binding {
                    self.bindings.insert(slot, id);
                }
                Some(group)
            }
            Err(rejected) => {
                debug_assert!(false, "hosting a fresh tab was rejected");
                let _duplicate_handle = rejected;
                None
            }
        }
    }

    /// Drop every slot belonging to `session_id` from the layout model (and
    /// the derived grid). Returns `true` when the removal emptied the
    /// window — the caller applies the empty-window rule.
    fn remove_session_slots(&mut self, session_id: SessionId) -> bool {
        let victims: Vec<PaneRef> = self
            .slots_in_order()
            .into_iter()
            .filter(|slot| slot.session_id == session_id)
            .collect();
        if victims.is_empty() {
            return false;
        }
        for slot in victims {
            // The pane is leaving with its session; its tab identity retires.
            let _retired = self.take_slot(slot);
        }
        self.hidden_panes
            .retain(|slot| slot.session_id != session_id);
        self.mark_grid_dirty();
        self.is_visually_empty()
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
        // Placeholders still waiting on the dead session can never
        // materialize; they close with it.
        let emptied = self.reap_session_placeholders(session_id) || emptied;

        if self.previous_active_session_id == Some(session_id) {
            self.previous_active_session_id = None;
        }

        (self.repair_active_session(sessions), emptied)
    }

    /// Whether `session_id`'s main pane is a tab this window currently
    /// renders: hosted here, and the effective selection of its group under
    /// the window's visibility mode. The "user can see it" test the hosting
    /// repair prefers — hosting queries deliberately stay broader (they
    /// include inactive tabs and hidden panes).
    fn renders_main_pane(&self, session_id: SessionId) -> bool {
        let main = PaneRef {
            session_id,
            key: MAIN_PANE_KEY,
        };
        self.group_of_slot(main)
            .and_then(|group| self.rendered_slot(group))
            == Some(main)
    }

    /// The session activation falls back to when the active session no
    /// longer hosts a pane here: the previously active session first (the
    /// press on a pane's close button also activates that pane — pane_grid
    /// publishes `on_click` for every press — so the closed session is
    /// usually active by the time the close lands even when the user was
    /// working elsewhere), then the hosted sessions by ascending id
    /// (deterministic where the grid's map order is not). The whole
    /// candidate set prefers a session whose main pane is a rendered tab:
    /// activation should land where the user can see it, not on a main
    /// input buried behind another tab or hidden under a collapsed toolbar.
    /// A window whose every candidate is obscured still repairs to the
    /// first candidate — hosting alone suffices when nothing renders a
    /// main pane. (In an all-hidden window the hidden filter is inert, so
    /// every main renders and the preference changes nothing.)
    fn repair_fallback(&self) -> Option<SessionId> {
        let previous = self
            .previous_active_session_id
            .filter(|id| self.hosts_session(*id));
        let hosted = self.hosted_sessions();
        let ordered = || previous.iter().copied().chain(hosted.iter().copied());
        ordered()
            .find(|&id| self.renders_main_pane(id))
            .or_else(|| ordered().next())
    }

    /// Re-point this window's active session when it no longer hosts a pane
    /// of the current one (its panes were removed or transplanted away),
    /// falling back per [`Self::repair_fallback`].
    pub fn repair_active_session(&mut self, sessions: &SessionStore) -> Task<Message> {
        if let Some(active) = self.active_session_id {
            if self.hosts_session(active) {
                return Task::none();
            }
            self.active_session_id = None;
            self.mark_workspace_dirty();
        }
        match self.repair_fallback() {
            Some(fallback) => self.set_active_session(fallback, sessions),
            None => Task::none(),
        }
    }

    /// The script-swap sibling of [`Self::repair_active_session`]: keep the
    /// toolbar's active session valid after a pane payload exchange without
    /// requesting keyboard focus. A layout-only script operation must not
    /// pull focus into a main input merely because that session became the
    /// deterministic fallback in this window.
    pub fn repair_active_session_without_focus(&mut self) {
        if self
            .active_session_id
            .is_some_and(|active| self.hosts_session(active))
        {
            return;
        }
        let repaired = self.repair_fallback();
        if repaired != self.active_session_id {
            self.active_session_id = repaired;
            self.mark_workspace_dirty();
        }
    }

    /// Record which pane each involved group renders before a pane payload
    /// exchange touching `first` and `second` mutates this window's model.
    /// The groups a swap can change are exactly those hosting the swapped
    /// panes; a pane hosted elsewhere contributes nothing here.
    pub fn pane_swap_render_probe(&self, first: PaneRef, second: PaneRef) -> PaneSwapRenderProbe {
        let mut probe = PaneSwapRenderProbe::default();
        for slot in [first, second] {
            if let Some(group) = self.group_of_slot(slot)
                && !probe.groups.iter().any(|&(seen, _)| seen == group)
            {
                probe.groups.push((group, self.rendered_slot(group)));
            }
        }
        probe
    }

    /// Reconcile this window's active-session identity after a pane payload
    /// swap, without issuing a focus operation: compare each involved
    /// group's rendered pane against the pre-swap probe and let activation
    /// follow the rendered position (see [`active_after_pane_swap`]). Ends
    /// with the hosting repair, so an active session whose last pane left
    /// this window still falls back deterministically.
    pub fn settle_active_session_after_pane_swap(&mut self, probe: PaneSwapRenderProbe) {
        let changes: smallvec::SmallVec<[RenderedSlotChange; 2]> = probe
            .groups
            .iter()
            .map(|&(group, before)| RenderedSlotChange {
                before,
                after: self.rendered_slot(group),
            })
            .collect();
        let next = active_after_pane_swap(self.active_session_id, &changes);
        if next != self.active_session_id {
            self.previous_active_session_id = self.active_session_id;
            self.active_session_id = next;
            self.mark_workspace_dirty();
        }
        self.repair_active_session_without_focus();
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
            // sessions, Tab could reach a session input behind it. Grouped panes
            // widen that gap: inactive tabs' mounted subtrees contribute obscured
            // focusables the traversal can land on as well.
            subscriptions.push(iced::event::listen_with(tab_pressed));
        }

        // Keyboard tab cycling within the focus group. (The handler no-ops
        // while a modal is open, where Tab belongs to form traversal.)
        subscriptions.push(iced::event::listen_with(cycle_tab_pressed));

        #[cfg(debug_assertions)]
        if tab_groups_dev_enabled() {
            // QA-only: the dev-gated group-merge keybinding.
            subscriptions.push(iced::event::listen_with(dev_merge_pressed));
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
            self.mark_grid_dirty();
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
            // The active session is persisted per window.
            self.mark_workspace_dirty();
        }
        self.active_session_id = Some(session_id);

        // Focus the session's input only when its main pane is in this
        // window *and* is the tab its group currently renders: focusing an
        // input obscured behind another tab would type into an invisible
        // widget. (Activation never propagates across windows.)
        let main = PaneRef {
            session_id,
            key: MAIN_PANE_KEY,
        };
        let rendered = self
            .group_of_slot(main)
            .and_then(|group| self.rendered_slot(group))
            == Some(main);
        if !rendered {
            return Task::none();
        }
        if let Some(session) = sessions.get(session_id) {
            let input_id = session.input.input_id();
            operation::focus(input_id)
        } else {
            Task::none()
        }
    }

    /// Make `tab` its group's durable selection and move activation with it
    /// (also the landing step of every drop that moves a tab — attention
    /// moves with the pane):
    /// the tab's session becomes this window's active session, the main
    /// input is focused only when the newly rendered pane is that session's
    /// main pane, the newly rendered pane's size is reported synchronously
    /// (so a script relayout cannot paint one frame at stale dimensions),
    /// and the strip scrolls the tab into view. Selection changes group
    /// content only — the grid configuration carries group ids and is not
    /// rebuilt.
    pub fn select_tab(&mut self, tab: TabId, sessions: &mut SessionStore) -> Task<Message> {
        self.select_tab_with_focus(tab, sessions, true)
    }

    /// Script-facing selection: update durable selection and activation but
    /// never pull keyboard focus into the newly shown pane.
    pub fn select_pane_without_focus(
        &mut self,
        slot: PaneRef,
        sessions: &mut SessionStore,
    ) -> Task<Message> {
        let Some(tab) = self.tab_of(slot) else {
            return Task::none();
        };
        self.select_tab_with_focus(tab, sessions, false)
    }

    fn select_tab_with_focus(
        &mut self,
        tab: TabId,
        sessions: &mut SessionStore,
        request_focus: bool,
    ) -> Task<Message> {
        // Selection is the landing step of drops and adoptions that just
        // mutated the model: settle the pending rebuild so the synchronous
        // size report below measures the grid the pane will paint into.
        self.flush_grid_rebuild();
        let Some(group) = self.layout.group_of(tab) else {
            return Task::none();
        };
        let slot = self.layout.tab(tab).and_then(|t| t.binding().copied());
        let previously_focused = self.focus_group.and_then(|group| self.rendered_slot(group));
        // The slot rendered before this selection — the pane the switch is
        // about to obscure.
        let previously_rendered = self.rendered_slot(group);
        let selection_changed = self.layout.selected(group) != Some(tab);
        if !self.layout.select(tab) {
            return Task::none();
        }
        if selection_changed {
            self.mark_workspace_dirty();
        }
        self.focus_group = Some(group);
        let newly_rendered = self.rendered_slot(group);

        // The obscured pane's subtree stays mounted behind the new tab, and
        // the focus transfer below blurs its input — a blur the widget
        // publishes only once its tab is re-selected (an obscured subtree
        // receives no events). Mark the input so that deferred `FocusLost`
        // reads as an obscure, not the user abandoning the line: the
        // clear-on-blur behavior must not consume in-progress text across a
        // tab switch.
        if let Some(previous) = previously_rendered
            && newly_rendered != Some(previous)
            && let Some(session) = sessions.get_mut(previous.session_id)
        {
            session.note_pane_input_obscured(previous.key);
        }

        let mut focus_task = Task::none();
        if let Some(slot) = slot {
            if self.active_session_id != Some(slot.session_id) {
                self.previous_active_session_id = self.active_session_id;
                self.active_session_id = Some(slot.session_id);
                self.mark_workspace_dirty();
            }
            let rendered = newly_rendered;
            if request_focus
                && rendered == Some(slot)
                && slot.key == MAIN_PANE_KEY
                && let Some(session) = sessions.get(slot.session_id)
            {
                // The caret-friendly transfer: focuses the input (releasing
                // every other holder) without re-focusing it when it already
                // holds focus — the stock focus operation would move its
                // caret to the end.
                focus_task = components::session_input::focus_target(session.input.input_id());
            } else {
                // Release only the input this selection displaced. Iced runs
                // widget operations through every application window, so the
                // stock blanket `unfocus()` would let a script selecting a tab
                // in a background window clear (and potentially erase) the
                // user's foreground input.
                //
                // A changed tab displaces that group's previously rendered
                // pane. An ordinary re-selection of an already-selected
                // non-main tab instead displaces the prior focus group's
                // input; retaining this second case prevents keystrokes from
                // staying attached to another session after activation moves.
                let displaced = displaced_input_for_selection(
                    request_focus,
                    rendered,
                    previously_rendered,
                    previously_focused,
                );
                if let Some(input_id) = displaced.and_then(|pane| {
                    sessions
                        .get(pane.session_id)
                        .and_then(|session| session.pane_input_id(pane.key))
                }) {
                    focus_task = components::session_input::unfocus_target(input_id);
                }
            }
            // The newly rendered pane occupies the group's existing region;
            // report that size before its first paint.
            if let Some(rendered) = rendered
                && let Some(size) = self.pane_size(rendered)
                && let Some(session) = sessions.get_mut(rendered.session_id)
                && session.pane_size_interest()
            {
                session.report_pane_size(rendered.key, size.width, size.height);
                session.flush_pane_sizes();
            }
        }
        Task::batch([
            focus_task,
            // The reveal reports the offset it scrolled to; routing it
            // through the strip's own Scrolled arm keeps the scroll mirror
            // true (a widget-operation scroll never fires `on_scroll`, and a
            // stale mirror would displace every later header-drop
            // classification by the un-mirrored delta).
            tab_strip::reveal(self.strip_id(group), self.tab_anchor(tab))
                .map(move |offset| Message::TabStrip(group, tab_strip::Event::Scrolled(offset))),
        ])
    }

    /// The group keyboard tab-cycling operates on: the focus group when it
    /// still exists, else the active session's main-pane group.
    fn cycle_group(&self) -> Option<GroupId> {
        self.focus_group
            .filter(|group| self.layout.contains_group(*group))
            .or_else(|| {
                let session_id = self.active_session_id?;
                self.group_of_slot(PaneRef {
                    session_id,
                    key: MAIN_PANE_KEY,
                })
            })
    }

    /// Host `tab` as a singleton group split off the WHOLE group hosting
    /// `reference` — a reference that is one tab of a multi-tab group splits
    /// beside its group, never inside it (placement never implies grouping;
    /// only an explicit grouping gesture or API request stacks tabs).
    /// Falls back to the session's main pane's group, then a fresh top-level
    /// cluster — the split placement chain. The tab value threads through
    /// each rejected attempt so the pane can never be lost between them.
    /// Callers rebuild.
    fn host_tab_beside(
        &mut self,
        mut tab: Tab<PaneRef>,
        reference: PaneRef,
        axis: pane_grid::Axis,
        new_first: bool,
        sizing: SplitSizing,
    ) {
        let Some(slot) = tab.binding().copied() else {
            // A placeholder tab has no session to anchor against; an even
            // top-level share is the only placement it can take.
            self.host_as_cluster(tab);
            return;
        };
        let id = tab.id();
        let main = PaneRef {
            session_id: slot.session_id,
            key: MAIN_PANE_KEY,
        };
        for anchor in [reference, main] {
            let Some(group) = self.group_of_slot(anchor) else {
                continue;
            };
            match self.layout.split_group(group, axis, new_first, sizing, tab) {
                Ok(_) => {
                    self.bindings.insert(slot, id);
                    return;
                }
                Err(rejected) => tab = rejected,
            }
        }
        // Neither the reference nor the session's main pane is here (the
        // window vanished mid-flight): take an even top-level share.
        self.host_as_cluster(tab);
    }

    /// Place a freshly opened script pane: split off the reference pane
    /// (falling back to the session's main pane, then a fresh top-level
    /// cluster) toward the requested direction. The pixel size request is
    /// stored on the model and resolved against the reference region's
    /// extent at every rebuild — the piece that makes placement independent
    /// of session/script creation order. The split lands in this window
    /// because the caller (the daemon) picked the window hosting the
    /// reference pane.
    pub fn place_session_pane(
        &mut self,
        session_id: SessionId,
        key: PaneKey,
        placement: PanePlacement,
    ) {
        let slot = PaneRef { session_id, key };
        if self.bindings.contains_key(&slot) {
            return;
        }
        match placement {
            PanePlacement::Split {
                reference,
                direction,
                size_px,
            } => {
                let (axis, new_first) = direction_axis(direction);
                let sizing = size_px.map_or(SplitSizing::Ratio(0.5), |px| SplitSizing::Px {
                    px,
                    sized_first: new_first,
                });
                let reference = PaneRef {
                    session_id,
                    key: reference,
                };
                self.host_tab_beside(Tab::bound(slot), reference, axis, new_first, sizing);
            }
            PanePlacement::Tab {
                reference,
                position,
                ..
            } => {
                let reference = PaneRef {
                    session_id,
                    key: reference,
                };
                let mut tab = Tab::bound(slot);
                if let Some((group, insertion_slot)) = self.tab_merge_target(reference, position) {
                    let id = tab.id();
                    match self.layout.insert_tab(group, insertion_slot, tab) {
                        Ok(()) => {
                            self.bindings.insert(slot, id);
                            self.mark_grid_dirty();
                            return;
                        }
                        Err(rejected) => tab = rejected,
                    }
                }
                self.host_as_cluster(tab);
            }
        }
        self.mark_grid_dirty();
    }

    /// Resolve a member-counted insertion slot in the reference's current
    /// group, matching `GroupLayout::merge_tab`.
    pub fn tab_merge_target(
        &self,
        reference: PaneRef,
        position: TabPosition,
    ) -> Option<(GroupId, usize)> {
        let tab = self.tab_of(reference)?;
        let group = self.layout.group_of(tab)?;
        let tabs = self.layout.tabs(group)?;
        let reference_index = tabs.iter().position(|member| member.id() == tab)?;
        let insertion_slot = match position {
            TabPosition::Before => reference_index,
            TabPosition::After => reference_index + 1,
            TabPosition::End => tabs.len(),
        };
        Some((group, insertion_slot))
    }

    /// Move or reorder one hosted pane into a reference's group without
    /// changing selection. Returns false if either side went stale.
    pub fn group_pane_with(
        &mut self,
        slot: PaneRef,
        reference: PaneRef,
        position: TabPosition,
    ) -> bool {
        let Some(tab) = self.tab_of(slot) else {
            return false;
        };
        let Some((target_group, insertion_slot)) = self.tab_merge_target(reference, position) else {
            return false;
        };
        let source_group = self.layout.group_of(tab);
        if !self.layout.merge_tab(tab, target_group, insertion_slot) {
            return false;
        }
        if source_group == Some(target_group) {
            self.mark_workspace_dirty();
        } else {
            self.mark_grid_dirty();
        }
        true
    }

    /// Drop one pane's slot from this window's layout. Returns `true` when
    /// the removal emptied the window — the caller applies the empty-window
    /// rule.
    pub fn remove_pane_slot(&mut self, session_id: SessionId, key: PaneKey) -> bool {
        let slot = PaneRef { session_id, key };
        // The pane is leaving this window; the daemon re-hosts it (if
        // anywhere) through its own placement paths, so the tab retires.
        if self.take_slot(slot).is_none() {
            return false;
        }
        self.hidden_panes.remove(&slot);
        self.mark_grid_dirty();
        self.is_visually_empty()
    }

    /// Sync one slot's hidden state from its pane's def (the daemon calls
    /// this on `PaneUpdated`, on a pre-hidden `PaneOpened`, and after a
    /// cross-window move re-homes the slot here). Idempotent for the window
    /// whose own eyeball click originated the change.
    pub fn set_pane_hidden(&mut self, slot: PaneRef, hidden: bool) {
        let changed = if hidden {
            self.hidden_panes.insert(slot)
        } else {
            self.hidden_panes.remove(&slot)
        };
        if changed {
            // The user-set eyeball state persists whatever the toolbar
            // mode; only the grid rebuild is gated on the collapsed filter.
            self.mark_workspace_dirty();
        }
        if changed && !self.toolbar_expanded {
            self.mark_grid_dirty();
        }
    }

    /// Whether this window's layout currently marks `slot` hidden.
    pub fn pane_hidden(&self, slot: PaneRef) -> bool {
        self.hidden_panes.contains(&slot)
    }

    /// The same-window shape of a pane swap (script `pane.swap` and drag
    /// center drops share one semantics — see `swap_script_panes`): exchange
    /// two panes' exact tab slots without changing the split tree. The two
    /// TabIds travel with their panes, so the keyed body host re-pairs each
    /// subtree with its moved tab and widget state follows the pane; hidden
    /// state belongs to the pane identity and needs no move while both
    /// panes remain in this window. Same-group pairs and inactive tabs
    /// exchange the same way — the model swaps strip slots and lets
    /// selection follow the slot.
    pub fn swap_pane_slots(&mut self, x: PaneRef, y: PaneRef) {
        let (Some(a), Some(b)) = (self.tab_of(x), self.tab_of(y)) else {
            return;
        };
        self.layout.swap_tabs(a, b);
        self.mark_grid_dirty();
    }

    /// One half of the cross-window shape of a pane swap (see
    /// `swap_script_panes` for the unified semantics): rebind a hosted tab
    /// to a different pane in place, preserving destination geometry, strip
    /// position, and selection — the tab stays in this window and only its
    /// pane payload trades places. Hidden membership for `old` is removed;
    /// the daemon seeds `new` from its source window after both halves of a
    /// cross-window swap land.
    pub fn replace_pane_slot(&mut self, old: PaneRef, new: PaneRef) -> bool {
        let Some(tab) = self.tab_of(old) else {
            return false;
        };
        if !self.layout.set_binding(tab, Some(new)) {
            return false;
        }
        self.bindings.remove(&old);
        self.bindings.insert(new, tab);
        self.hidden_panes.remove(&old);
        // The tab now fronts a different pane: drop its span mirror so the
        // next paint re-announces the bounds under the new binding.
        self.tab_spans.borrow_mut().remove(&tab);
        self.mark_grid_dirty();
        true
    }

    // ------------------------------------------------------------------
    // Drag targeting and drop application
    // ------------------------------------------------------------------

    /// Classify a drag hover/release point against this window's live
    /// geometry: grid regions from the current layout, header bands and tab
    /// spans from the draw-time mirrors. `window_point` is window-local
    /// logical; `window_size` is the daemon-tracked size (the grid fills the
    /// window below the toolbar/banner chrome). `None` means no drop surface
    /// is under the point — a release there is a no-op re-dock.
    pub fn classify_drag_target(
        &self,
        window_point: Point,
        window_size: Size,
        drag: &TabDrag,
    ) -> Option<ClassifiedTarget> {
        // Classification precedes every mutation in its cycle, so the grid
        // is never mid-cycle stale here.
        debug_assert!(!self.grid_dirty, "drag classification read a stale grid");
        if self.is_visually_empty() {
            // A window with no visible content (empty, or holding only
            // invisible vacancy tabs) renders the connect view and is one
            // whole drop surface: adopt as a fresh cluster. The highlight
            // covers the window's content region (the overlay clips
            // nothing; overdraw past the bottom is harmless).
            return Some(ClassifiedTarget {
                action: pane_drag::DragAction::Vacant,
                highlight: iced::Rectangle::with_size(window_size),
                caret: None,
            });
        }
        let grid = self.grid.as_ref()?;
        let area = self.grid_area.get();
        if area.width <= 0.0 || area.height <= 0.0 {
            return None;
        }
        let offset_y = (window_size.height - area.height).max(0.0);
        let point = Point::new(window_point.x, window_point.y - offset_y);

        let regions = grid
            .layout()
            .pane_regions(GRID_SPACING, GRID_MIN_SIZE, area);
        let bands = self.strip_bands.borrow();
        let spans = self.tab_spans.borrow();
        let scroll = self.strip_scroll.borrow();
        let show_all = self.show_all();
        let mut geoms: Vec<pane_drag::PaneTargetGeom> = Vec::with_capacity(regions.len());
        for (pane, group) in grid.panes.iter() {
            let Some(region) = regions.get(pane) else {
                continue;
            };
            let band_height = bands
                .get(group)
                .map_or(pane_drag::DEFAULT_HEADER_BAND, |band| {
                    band.height + TITLE_BAR_PADDING
                });
            let offset = scroll.get(group).copied().unwrap_or(0.0);
            // Only listed tabs contribute bands ([`Self::tab_in_strip`], the
            // strip's own filter): spans persist stale-until-rendered, so a
            // tab the strip is not showing — eyeball-hidden under a
            // collapsed toolbar — still has a span on file, and counting it
            // would aim carets and insertion slots at strip positions that
            // do not exist. Each band carries its member-vec index so the
            // drop slot stays in `merge_tab`'s member-counted space even
            // when the listing skips members.
            let tabs: smallvec::SmallVec<[pane_drag::TabBand; 8]> = self
                .layout
                .tabs(*group)
                .into_iter()
                .flatten()
                .enumerate()
                .filter(|(_, tab)| self.tab_in_strip(tab, show_all))
                .filter_map(|(slot, tab)| {
                    spans.get(&tab.id()).map(|span| pane_drag::TabBand {
                        start: span.x - offset,
                        end: span.x + span.width - offset,
                        slot,
                    })
                })
                .collect();
            geoms.push(pane_drag::PaneTargetGeom {
                group: *group,
                bounds: *region,
                band_height,
                tabs,
            });
        }

        let source_group = self.layout.group_of(drag.tab);
        let source_solo = source_group
            .and_then(|group| self.layout.tabs(group))
            .is_some_and(|tabs| tabs.len() == 1);
        pane_drag::classify_target(area, point, &geoms, source_group, source_solo)
    }

    /// Apply a header drop within this window: move `tab` into `group`'s
    /// strip at `slot` — a reorder when `group` is the tab's own. A reorder
    /// leaves selection, activation, and the grid untouched; a move into
    /// another group selects the moved tab there and activates its session.
    /// `None` when a participant no longer resolves.
    pub fn apply_drag_merge(
        &mut self,
        tab: TabId,
        group: GroupId,
        slot: usize,
        sessions: &mut SessionStore,
    ) -> Option<Task<Message>> {
        let owner = self.layout.group_of(tab)?;
        if !self.layout.merge_tab(tab, group, slot) {
            return None;
        }
        if owner == group {
            // Strip order is group content: the grid configuration carries
            // group ids and needs no rebuild — the keyed body host re-pairs
            // each subtree with its moved tab on the next view. The order is
            // persisted state all the same (`docs/panes.md` §17 stores group
            // membership and order), so the workspace mark is owed even
            // though the grid mark is not.
            self.mark_workspace_dirty();
            self.focus_group = Some(group);
            return Some(Task::none());
        }
        self.mark_grid_dirty();
        Some(self.select_tab(tab, sessions))
    }

    /// Apply a body-edge drop within this window: detach `tab` into a new
    /// singleton group split beside the whole `group` (against its own
    /// group, the ungroup gesture), select it, and move activation with it.
    /// `None` when a participant no longer resolves or there is nothing to
    /// ungroup.
    pub fn apply_drag_split(
        &mut self,
        tab: TabId,
        group: GroupId,
        region: DropRegion,
        sessions: &mut SessionStore,
    ) -> Option<Task<Message>> {
        let (axis, new_first) = split_axis(region);
        self.layout
            .split_tab_as_singleton(tab, group, axis, new_first, SplitSizing::Ratio(0.5))?;
        self.mark_grid_dirty();
        Some(self.select_tab(tab, sessions))
    }

    /// Apply a whole-grid edge drop within this window: detach `tab` into a
    /// new singleton top-level placement (Left: leading cluster, Right:
    /// trailing cluster, Top/Bottom: wrap the whole layout), select it, and
    /// move activation with it.
    pub fn apply_drag_grid_edge(
        &mut self,
        tab: TabId,
        side: GridEdgeSide,
        sessions: &mut SessionStore,
    ) -> Option<Task<Message>> {
        let moved = self.layout.remove_tab(tab)?;
        // The binding entry survives the round trip: the tab keeps its id
        // and its pane, only its leaf moves.
        let result = match side {
            GridEdgeSide::Left => self.layout.insert_cluster_front(moved),
            GridEdgeSide::Right => self.layout.push_cluster(moved),
            GridEdgeSide::Top => self
                .layout
                .wrap_all(pane_grid::Axis::Horizontal, true, moved),
            GridEdgeSide::Bottom => self
                .layout
                .wrap_all(pane_grid::Axis::Horizontal, false, moved),
        };
        if let Err(rejected) = result {
            // Unreachable (the tab was just removed, so it is admissible);
            // re-host rather than lose it.
            debug_assert!(false, "grid-edge re-hosting rejected a detached tab");
            self.host_as_cluster(rejected);
        }
        self.mark_grid_dirty();
        Some(self.select_tab(tab, sessions))
    }

    /// Detach one tab for a cross-window move, returning the tab value (its
    /// stable id and pane binding travel with it), its hidden state, and
    /// whether the removal emptied this window. The destination adopts the
    /// tab in the same update; a downstream rejection re-hosts it there —
    /// a detached tab is never stranded.
    pub fn extract_drag_tab(&mut self, tab: TabId) -> Option<(Tab<PaneRef>, bool, bool)> {
        let slot = self.layout.tab(tab)?.binding().copied();
        let taken = self.layout.remove_tab(tab)?;
        if let Some(slot) = slot {
            self.bindings.remove(&slot);
        }
        let hidden = slot.is_some_and(|slot| self.hidden_panes.remove(&slot));
        self.mark_grid_dirty();
        Some((taken, hidden, self.is_visually_empty()))
    }

    /// [`Self::extract_drag_tab`] addressed by pane reference — the daemon's
    /// entry for script relocate/tear-out, which detach one tab (preserving
    /// the remaining group and the tab's stable identity) for re-hosting
    /// elsewhere in the same update.
    pub fn extract_pane_tab(&mut self, slot: PaneRef) -> Option<(Tab<PaneRef>, bool, bool)> {
        let tab = self.tab_of(slot)?;
        self.extract_drag_tab(tab)
    }

    /// Adopt a cross-window tab into `group`'s strip at `slot`. Falls back
    /// to a fresh top-level cluster when the group vanished mid-update.
    pub fn adopt_drag_tab_merge(&mut self, tab: Tab<PaneRef>, group: GroupId, slot: usize) {
        let binding = tab.binding().copied();
        let id = tab.id();
        match self.layout.insert_tab(group, slot, tab) {
            Ok(()) => {
                if let Some(slot) = binding {
                    self.bindings.insert(slot, id);
                }
                self.mark_grid_dirty();
            }
            Err(rejected) => {
                self.host_as_cluster(rejected);
                self.mark_grid_dirty();
            }
        }
    }

    /// Adopt a cross-window tab as a new singleton group split beside the
    /// whole `group`. Falls back to a fresh top-level cluster when the
    /// group vanished mid-update.
    pub fn adopt_drag_tab_split(&mut self, tab: Tab<PaneRef>, group: GroupId, region: DropRegion) {
        let binding = tab.binding().copied();
        let id = tab.id();
        let (axis, new_first) = split_axis(region);
        match self
            .layout
            .split_group(group, axis, new_first, SplitSizing::Ratio(0.5), tab)
        {
            Ok(_) => {
                if let Some(slot) = binding {
                    self.bindings.insert(slot, id);
                }
                self.mark_grid_dirty();
            }
            Err(rejected) => {
                self.host_as_cluster(rejected);
                self.mark_grid_dirty();
            }
        }
    }

    /// Adopt a cross-window tab at a whole-grid edge placement.
    pub fn adopt_drag_tab_edge(&mut self, tab: Tab<PaneRef>, side: GridEdgeSide) {
        let binding = tab.binding().copied();
        let id = tab.id();
        let result = match side {
            GridEdgeSide::Left => self.layout.insert_cluster_front(tab),
            GridEdgeSide::Right => self.layout.push_cluster(tab),
            GridEdgeSide::Top => self.layout.wrap_all(pane_grid::Axis::Horizontal, true, tab),
            GridEdgeSide::Bottom => self
                .layout
                .wrap_all(pane_grid::Axis::Horizontal, false, tab),
        };
        match result {
            Ok(_) => {
                if let Some(slot) = binding {
                    self.bindings.insert(slot, id);
                }
            }
            Err(rejected) => {
                self.host_as_cluster(rejected);
            }
        }
        self.mark_grid_dirty();
    }

    /// Adopt a cross-window tab as this window's first cluster (a drop into
    /// an empty window, or a tear-out window's seed).
    pub fn adopt_drag_tab_cluster(&mut self, tab: Tab<PaneRef>) {
        self.host_as_cluster(tab);
        self.mark_grid_dirty();
    }

    /// Apply a script `pane.resize` (panes.md placement commands): write the
    /// nearest in-cluster ancestor edge per given axis back to a script-owned
    /// px sizing. Best-effort per axis (a full-extent axis no-ops); rebuilds
    /// only when an edge actually changed.
    pub fn resize_pane_slot(&mut self, slot: PaneRef, width: Option<f32>, height: Option<f32>) {
        let Some(group) = self.group_of_slot(slot) else {
            return;
        };
        let mut changed = false;
        if let Some(px) = width {
            changed |= self
                .layout
                .set_group_px(group, pane_grid::Axis::Vertical, px);
        }
        if let Some(px) = height {
            changed |= self
                .layout
                .set_group_px(group, pane_grid::Axis::Horizontal, px);
        }
        if changed {
            self.mark_grid_dirty();
        }
    }

    /// The re-attach half of a script `pane.relocate`: host the detached
    /// tab — stable id and pane binding intact, so keyed widget state
    /// survives a same-window relocation — as a singleton group split
    /// beside `reference`'s whole group, with a split-style optional px
    /// extent. The caller already detached the tab from whichever window
    /// held it. A reference that vanished mid-flight falls back to the
    /// session's main pane, then a fresh top-level cluster — the split
    /// placement chain.
    pub fn adopt_tab_beside(
        &mut self,
        tab: Tab<PaneRef>,
        reference: PaneRef,
        direction: SplitDirection,
        size_px: Option<f32>,
    ) {
        let (axis, new_first) = direction_axis(direction);
        let sizing = size_px.map_or(SplitSizing::Ratio(0.5), |px| SplitSizing::Px {
            px,
            sized_first: new_first,
        });
        self.host_tab_beside(tab, reference, axis, new_first, sizing);
        self.mark_grid_dirty();
    }

    /// The measured sizes of every rendered pane in this window's grid, for
    /// the pane-size mirror feed. Empty before the first layout pass. A pane
    /// not currently rendered — hidden and dropped from the
    /// collapsed-toolbar grid, or an unselected tab — simply doesn't appear;
    /// its mirror keeps the last laid-out size.
    pub fn pane_sizes(&self) -> Vec<(PaneRef, Size)> {
        // The measurement feed must never publish a grid the model has
        // outrun; callers flush the pending rebuild first.
        debug_assert!(!self.grid_dirty, "pane_sizes read a stale grid");
        let Some(grid) = self.grid.as_ref() else {
            return Vec::new();
        };
        let area = self.grid_area.get();
        if area.width <= 0.0 || area.height <= 0.0 {
            return Vec::new();
        }
        let show_all = self.show_all();
        let regions = grid
            .layout()
            .pane_regions(GRID_SPACING, GRID_MIN_SIZE, area);
        let mut group_sizes = self.group_sizes.borrow_mut();
        grid.panes
            .iter()
            .filter_map(|(pane, group)| {
                let region = regions.get(pane)?;
                // Every measured group refreshes its last-rendered size,
                // whether or not it currently resolves to a reportable pane.
                group_sizes.insert(*group, region.size());
                let slot = self.rendered_slot_with(*group, show_all)?;
                Some((slot, region.size()))
            })
            .collect()
    }

    /// The pane rendered by the grid slot behind one of this grid's internal
    /// pane ids (pane ids are State-internal and never reused within a
    /// State, so a stale id is a clean miss). Grid-derived: read before a
    /// cycle's first mutation, never between a mutation and the deferred
    /// rebuild.
    pub fn pane_slot(&self, pane: pane_grid::Pane) -> Option<PaneRef> {
        // Grid ids are only meaningful against the grid that minted them;
        // callers resolve them before any mutation dirties it.
        debug_assert!(!self.grid_dirty, "pane_slot read a stale grid");
        let group = self.grid.as_ref().and_then(|grid| grid.panes.get(&pane))?;
        self.rendered_slot(*group)
    }

    /// The on-screen size (logical) of the grid slot hosting `slot`'s group,
    /// for sizing a torn-out window after it. A group absent from the
    /// current grid (hidden under a collapsed toolbar) reports its last
    /// measured size — a pane's size is stale until it renders again, never
    /// absent. `None` only for a group never measured (a window that has
    /// not completed a layout pass since the group arrived). Grid-derived:
    /// within an update cycle this is read before the cycle's first
    /// mutation (or behind a flush), never between a mutation and the
    /// deferred rebuild.
    pub fn pane_size(&self, slot: PaneRef) -> Option<Size> {
        // Sizes derived from a grid the model has outrun would be lies;
        // callers read before mutating or flush the rebuild first.
        debug_assert!(!self.grid_dirty, "pane_size read a stale grid");
        let group = self.group_of_slot(slot)?;
        let live = self.grid.as_ref().and_then(|grid| {
            let area = self.grid_area.get();
            if area.width <= 0.0 || area.height <= 0.0 {
                return None;
            }
            let pane = grid
                .panes
                .iter()
                .find_map(|(pane, g)| (*g == group).then_some(*pane))?;
            grid.layout()
                .pane_regions(GRID_SPACING, GRID_MIN_SIZE, area)
                .get(&pane)
                .map(|region| region.size())
        });
        match live {
            Some(size) => {
                self.group_sizes.borrow_mut().insert(group, size);
                Some(size)
            }
            None => self.group_sizes.borrow().get(&group).copied(),
        }
    }

    /// Detach every tab of `group` from the model, in strip order, together
    /// with the group's durably selected tab id. The group's leaf collapses
    /// with the last removal; the caller re-hosts the returned tabs (their
    /// window-level bindings are unaffected by the round trip). Compiled
    /// with its only caller, the debug-build dev-merge QA hook.
    #[cfg(debug_assertions)]
    fn extract_group_tabs(
        &mut self,
        group: GroupId,
    ) -> (smallvec::SmallVec<[Tab<PaneRef>; 4]>, Option<TabId>) {
        let selected = self.layout.selected(group);
        let ids: smallvec::SmallVec<[TabId; 4]> = self
            .layout
            .tabs(group)
            .map(|tabs| tabs.iter().map(Tab::id).collect())
            .unwrap_or_default();
        let mut tabs = smallvec::SmallVec::new();
        for id in ids {
            if let Some(tab) = self.layout.remove_tab(id) {
                tabs.push(tab);
            }
        }
        (tabs, selected)
    }

    /// Seed a freshly opened tear-out window (drag or script) with its
    /// single transplanted pane: the extracted tab value moves in whole,
    /// keeping its stable id across the window change. The toolbar starts
    /// collapsed — the window exists to show the pane, not the first-run
    /// connect flow.
    pub fn adopt_torn_out_tab(&mut self, tab: Tab<PaneRef>) {
        debug_assert!(
            self.layout.is_empty(),
            "tear-out windows start with an empty layout"
        );
        self.adopt_drag_tab_cluster(tab);
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
        let session_id =
            sessions.open_session(server_name.clone(), profile_name.clone(), auto_connect);

        // A vacant slot of this server in THIS window (the modal's window —
        // opens-where-you-asked) is adopted with its retained geometry;
        // otherwise the new session's main pane becomes a new singleton
        // group in a new top-level cluster, dividing the window evenly
        // against the existing session clusters — deterministic regardless
        // of whether other sessions' scripts have created their panes yet.
        if !self.adopt_vacancy(session_id, &server_name, &profile_name) {
            self.host_as_cluster(Tab::bound(PaneRef {
                session_id,
                key: MAIN_PANE_KEY,
            }));
        }
        self.mark_grid_dirty();

        // Set this as the active session (will deactivate others)
        let focus_task = self.set_active_session(session_id, sessions);

        self.set_toolbar_expanded(false);
        self.modal = None;
        self.deliver_pending_layout_prompt();

        focus_task
    }

    /// QA hook (debug builds only): opens an offline session in this window
    /// without the connect modal — the `SMUDGY_SPIKE_AUTOSESSION` startup
    /// path the scripted drag matrix drives (`bin/drag-matrix.ps1`).
    #[cfg(debug_assertions)]
    pub fn autosession_open_offline_session(
        &mut self,
        server_name: String,
        profile_name: String,
        sessions: &mut SessionStore,
    ) -> Task<Message> {
        let task = self.open_session(server_name, profile_name, false, sessions);
        // Keep the toolbar expanded so pane headers (the tabs the scripted
        // matrix drags) render even when the user's hide_pane_headers
        // preference would hide them.
        self.set_toolbar_expanded(true);
        task
    }

    /// Deliver a parked keep-or-close prompt if the modal surface allows it:
    /// no modal at all, or the Layouts menu sitting in its browse stage
    /// (which holds no user-entered state worth preserving). Called wherever
    /// the modal closes or settles, so a parked prompt surfaces at the first
    /// safe moment.
    fn deliver_pending_layout_prompt(&mut self) {
        if self.pending_layout_prompt.is_none() {
            return;
        }
        let deliverable = match &self.modal {
            None => true,
            Some(modal::Modal::Layouts(state)) => state.is_browsing(),
            Some(_) => false,
        };
        if !deliverable {
            log::info!("[layouts] keep-or-close prompt parked behind an open modal");
            return;
        }
        let Some(PendingLayoutPrompt {
            server,
            source,
            rows,
        }) = self.pending_layout_prompt.take()
        else {
            return;
        };
        // Reuse an open Layouts menu only when it is scoped to the prompt's
        // server — the answers' apply resolves against the menu's server.
        let reusable = matches!(&self.modal, Some(modal::Modal::Layouts(state))
            if state.server() == server);
        if !reusable {
            self.modal = Some(modal::Modal::Layouts(modal::layouts::State::opening(
                server,
            )));
        }
        if let Some(modal::Modal::Layouts(state)) = &mut self.modal {
            state.prompt_keep_or_close(source, rows);
        }
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
                toolbar::Message::LayoutsPressed => {
                    // Scoped to the acting session's server: the menu lists
                    // that store and nothing else.
                    if let Some(server) = self
                        .active_session_id
                        .and_then(|active| sessions.get(active))
                        .map(|session| session.server_name.clone())
                    {
                        self.modal = Some(modal::Modal::Layouts(modal::layouts::State::opening(
                            server,
                        )));
                        // A parked keep-or-close prompt outranks the fresh
                        // browse stage.
                        self.deliver_pending_layout_prompt();
                    } else {
                        log::info!("LayoutsPressed ignored - no active session");
                    }
                    Update::none()
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
                    // A stage retreat (Back to browsing) can free the modal
                    // surface for a parked keep-or-close prompt.
                    self.deliver_pending_layout_prompt();
                    Update::with_task(task.map(Message::ModalMessage))
                } else {
                    Update::none()
                }
            }
            Message::ModalEvent(event) => match event {
                modal::Event::Connect(connect_event) => match connect_event {
                    modal::ConnectEvent::CloseModalRequested => {
                        self.modal = None;
                        self.deliver_pending_layout_prompt();
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
                    modal::ConnectEvent::RestoreLastSession(server_name) => {
                        log::info!("Last-session restore requested for {server_name}");
                        // Restoring is a one-shot stamp, exactly like a
                        // named-layout apply: the modal closes; any
                        // keep-or-close questions reopen as a prompt.
                        self.modal = None;
                        self.deliver_pending_layout_prompt();
                        Update::with_event(Event::RestoreLastSession {
                            server: server_name,
                        })
                    }
                },
                modal::Event::Layouts(layouts_event) => {
                    let server = match &self.modal {
                        Some(modal::Modal::Layouts(state)) => state.server().to_string(),
                        _ => return Update::none(),
                    };
                    match layouts_event {
                        modal::layouts::Event::Apply { name } => {
                            // Applying is a one-shot stamp: the menu closes;
                            // any keep-or-close questions reopen it.
                            self.modal = None;
                            self.deliver_pending_layout_prompt();
                            Update::with_event(Event::ApplyLayout { server, name })
                        }
                        modal::layouts::Event::ApplyWithAnswers {
                            source,
                            close,
                            keep,
                        } => {
                            self.modal = None;
                            self.deliver_pending_layout_prompt();
                            Update::with_event(Event::ApplyLayoutWithAnswers {
                                server,
                                source,
                                close,
                                keep,
                            })
                        }
                        modal::layouts::Event::Save { name } => {
                            // The menu stays open: the save outcome (and any
                            // partial-capture annotation) lands back in it.
                            Update::with_event(Event::SaveLayout { server, name })
                        }
                        modal::layouts::Event::Reset => {
                            self.modal = None;
                            self.deliver_pending_layout_prompt();
                            match self.active_session_id {
                                Some(session) => {
                                    Update::with_event(Event::ResetSessionLayout(session))
                                }
                                None => Update::none(),
                            }
                        }
                    }
                }
            },
            Message::PromptLayoutAnswers {
                server,
                source,
                rows,
            } => {
                // Route the questions into the Layouts modal, reopening it
                // if the user already dismissed it — nothing mutates until
                // these are answered, and closing must stay explicit. An
                // occupied modal surface (a Connect form mid-use, or the
                // Layouts menu deep in a stage) is never clobbered: the
                // prompt parks and delivers when the surface frees up.
                self.pending_layout_prompt = Some(PendingLayoutPrompt {
                    server,
                    source,
                    rows,
                });
                self.deliver_pending_layout_prompt();
                Update::none()
            }
            Message::LayoutSaveOutcome(outcome) => {
                if let Some(modal::Modal::Layouts(state)) = &mut self.modal {
                    state.record_save_outcome(outcome);
                } else {
                    log::info!("[layouts] save outcome arrived after the menu closed: {outcome:?}");
                }
                Update::none()
            }
            Message::CloseModal => {
                self.modal = None;
                self.deliver_pending_layout_prompt();
                Update::none()
            }
            Message::EscapePressed(window_id) => {
                if window_id == self.window_id {
                    self.modal = None;
                    self.deliver_pending_layout_prompt();
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
                // The clicked pane's group becomes the keyboard focus group.
                if let Some(&group) = self.grid.as_ref().and_then(|grid| grid.panes.get(&pane)) {
                    self.focus_group = Some(group);
                }
                let clicked_session = self.pane_slot(pane).map(|slot| slot.session_id);
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
                    // Divider positions persist; the mark is two boolean
                    // stores, so the per-frame resize stream stays within
                    // the mutation budget (the trailing debounce coalesces
                    // the whole drag into one write).
                    self.mark_workspace_dirty();
                }
                Update::none()
            }
            Message::TogglePaneVisibility(slot) => {
                let hidden = if self.hidden_panes.remove(&slot) {
                    false
                } else {
                    self.hidden_panes.insert(slot);
                    true
                };
                // The eyeball state persists (replayed at restore through
                // this same user-toggle path).
                self.mark_workspace_dirty();
                // With the toolbar expanded the toggle only changes the veil
                // (the grid renders everything); collapsed — reachable when
                // the pane's header is pinned or the global hide setting is
                // off — the pane leaves or rejoins the grid immediately.
                if !self.toolbar_expanded {
                    self.mark_grid_dirty();
                }
                // The flip above is optimistic display state; the def lives
                // on the pane's session runtime, so the daemon reports it.
                Update::with_event(Event::PaneVisibilityToggled { slot, hidden })
            }
            Message::TabStrip(group, event) => match event {
                tab_strip::Event::Select(tab) => {
                    // A sub-deadband release: a click both selects and
                    // activates. Logged for the scripted drag matrix.
                    log::info!("[pane-drag] click-select tab {tab:?}");
                    Update::with_task(self.select_tab(tab, sessions))
                }
                tab_strip::Event::Drag(tab, press) => match press {
                    tab_press::Event::Pressed { point } => {
                        let (Some(slot), Some(owner)) = (
                            self.layout.tab(tab).and_then(|t| t.binding().copied()),
                            self.layout.group_of(tab),
                        ) else {
                            return Update::none();
                        };
                        // The press surface reports strip-content-space
                        // points (its host scrollable hands children a
                        // scroll-translated cursor); the daemon measures its
                        // deadband against raw window-space cursor samples,
                        // so the point is grounded here, where the group's
                        // scroll-offset mirror lives.
                        let point =
                            tab_strip::ground_to_window(point, self.strip_scroll_offset(group));
                        Update::with_event(Event::TabDragPressed {
                            tab,
                            slot,
                            group: owner,
                            point,
                        })
                    }
                    tab_press::Event::ModifiedPress => {
                        // Reserved control behavior: never a drag, and
                        // never a selection through the drag machinery.
                        log::info!("[pane-drag] modified press on tab {tab:?} — reserved, no-op");
                        Update::none()
                    }
                    tab_press::Event::DragStarted { press, point } => {
                        let (Some(slot), Some(owner)) = (
                            self.layout.tab(tab).and_then(|t| t.binding().copied()),
                            self.layout.group_of(tab),
                        ) else {
                            return Update::none();
                        };
                        // Grounded like the press: the daemon stores `press`
                        // for release diagnostics and classifies `point`
                        // against window-space geometry.
                        let offset = self.strip_scroll_offset(group);
                        Update::with_event(Event::TabDragStarted {
                            tab,
                            slot,
                            group: owner,
                            press: tab_strip::ground_to_window(press, offset),
                            point: tab_strip::ground_to_window(point, offset),
                        })
                    }
                    tab_press::Event::DragReleased { point } => {
                        Update::with_event(Event::TabDragReleased { point })
                    }
                    tab_press::Event::CaptureLost { dragging } => {
                        if dragging {
                            Update::with_event(Event::TabDragCanceled {
                                reason: "capture-loss",
                            })
                        } else {
                            Update::none()
                        }
                    }
                    // `Click` is mapped to `Event::Select` inside the strip.
                    tab_press::Event::Click => Update::none(),
                },
                tab_strip::Event::Scrolled(offset) => {
                    self.strip_scroll.borrow_mut().insert(group, offset);
                    Update::none()
                }
                tab_strip::Event::Connect(tab) | tab_strip::Event::Disconnect(tab) => {
                    let Some(slot) = self.layout.tab(tab).and_then(|t| t.binding().copied()) else {
                        return Update::none();
                    };
                    let msg = match event {
                        tab_strip::Event::Connect(_) => session_store::Message::Reconnect,
                        _ => session_store::Message::Disconnect,
                    };
                    self.update(
                        Message::SessionPaneUserAction {
                            session_id: slot.session_id,
                            msg,
                        },
                        sessions,
                    )
                }
                tab_strip::Event::CloseSession(tab) => {
                    match self.layout.tab(tab).and_then(|t| t.binding().copied()) {
                        Some(slot) => Update::with_event(Event::CloseSession(slot.session_id)),
                        None => Update::none(),
                    }
                }
                tab_strip::Event::ToggleVisibility(tab) => {
                    match self.layout.tab(tab).and_then(|t| t.binding().copied()) {
                        Some(slot) => self.update(Message::TogglePaneVisibility(slot), sessions),
                        None => Update::none(),
                    }
                }
            },
            Message::CycleTab {
                window_id,
                backwards,
            } => {
                // While a modal is open Tab belongs to its form traversal.
                if window_id != self.window_id || self.modal.is_some() {
                    return Update::none();
                }
                let Some(group) = self.cycle_group() else {
                    return Update::none();
                };
                let Some(tabs) = self.layout.tabs(group) else {
                    return Update::none();
                };
                if tabs.len() < 2 {
                    return Update::none();
                }
                let current = self
                    .layout
                    .selected(group)
                    .and_then(|selected| tabs.iter().position(|t| t.id() == selected))
                    .unwrap_or(0);
                // Keyboard cycling reaches only tabs that would render:
                // hidden panes dropped under a collapsed toolbar and unbound
                // placeholders are skipped (a hidden tab stays reachable by
                // clicking its dimmed strip entry — an explicit act). Walking
                // at most one full wrap keeps the direction's order; no
                // renderable destination means no-op.
                let show_all = self.show_all();
                let renderable = |tab: &Tab<PaneRef>| {
                    tab.binding()
                        .is_some_and(|slot| show_all || !self.hidden_panes.contains(slot))
                };
                let mut index = current;
                let mut target = None;
                for _ in 1..tabs.len() {
                    index = cycle_index(index, tabs.len(), backwards);
                    if renderable(&tabs[index]) {
                        target = Some(tabs[index].id());
                        break;
                    }
                }
                let Some(target) = target else {
                    return Update::none();
                };
                Update::with_task(self.select_tab(target, sessions))
            }
            #[cfg(debug_assertions)]
            Message::DevMergeGroup(window_id) => {
                // QA-only (`SMUDGY_TAB_GROUPS_DEV=1`, debug builds): merge
                // the focus group's tabs into its neighbor group so
                // multi-tab groups can be exercised without the merge drag.
                if window_id != self.window_id || !tab_groups_dev_enabled() {
                    return Update::none();
                }
                let groups = self.layout.groups_depth_first();
                if groups.len() < 2 {
                    return Update::none();
                }
                let Some(source) = self.cycle_group() else {
                    return Update::none();
                };
                let Some(position) = groups.iter().position(|g| *g == source) else {
                    return Update::none();
                };
                let target = if position > 0 {
                    groups[position - 1]
                } else {
                    groups[position + 1]
                };
                log::info!("[tab-groups-dev] merging {source:?} into {target:?}");
                let (tabs, selected) = self.extract_group_tabs(source);
                for tab in tabs {
                    if let Err(rejected) = self.layout.append_tab(target, tab) {
                        self.host_as_cluster(rejected);
                    }
                }
                if let Some(selected) = selected {
                    self.layout.select(selected);
                }
                self.focus_group = Some(target);
                self.mark_grid_dirty();
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

    /// `drag` is this window's view of the daemon-owned drag state: whether
    /// a drag is live (temporary header bands, press-surface resets), the
    /// window-level modifier state for the tab press surfaces, and — when
    /// this window is the hovered one — the classified target its overlay
    /// renders.
    pub fn view<'a>(
        &'a self,
        sessions: &'a SessionStore,
        drag: DragViewContext<'a>,
    ) -> ThemedElement<'a, Message> {
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
                let show_all = self.show_all();
                let panes = PaneGrid::new(grid, |_pane, group, _is_maximized| {
                    let group = *group;
                    // The payload is the stable group id; the strip renders
                    // every member tab and the body hosts every member's
                    // subtree, with the effective selection on top.
                    let Some(tabs) = self.layout.tabs(group) else {
                        debug_assert!(false, "grid slot references unknown group {group:?}");
                        return pane_grid::Content::new(iced::widget::Space::new());
                    };
                    let selected_tab = self.layout.selected(group);
                    let rendered_tab = self.layout.effective_selected(group, |tab| {
                        tab.binding()
                            .is_some_and(|slot| show_all || !self.hidden_panes.contains(slot))
                    });
                    // A group whose every member is a pending placeholder —
                    // a restored or layout-applied window whose pane
                    // definitions have not arrived — resolves to no bound
                    // pane. Such a group renders its placeholder bodies
                    // (neutral empty surfaces holding the persisted
                    // geometry) beneath a strip listing every pending tab;
                    // it is a first-class state, never a panic, in any
                    // build profile. (Vacancy tabs never get this far: the
                    // grid build drops groups holding nothing else, and
                    // mixed groups render only their content tabs.)
                    let rendered_slot = rendered_tab
                        .and_then(|id| self.layout.tab(id))
                        .and_then(|tab| tab.binding().copied());
                    let rendered_session = rendered_slot.and_then(|slot| {
                        let session = sessions.get(slot.session_id);
                        // A bound slot must reference a live session; render
                        // the desync like a placeholder rather than panicking
                        // mid-frame.
                        debug_assert!(
                            session.is_some(),
                            "grid slot references unknown session {}",
                            slot.session_id
                        );
                        session
                    });
                    let rendered = rendered_slot.zip(rendered_session);
                    let is_active = rendered
                        .is_some_and(|(slot, _)| self.active_session_id == Some(slot.session_id));
                    let is_hidden =
                        rendered.is_some_and(|(slot, _)| self.hidden_panes.contains(&slot));

                    // Every member subtree stays mounted, keyed by stable tab
                    // id; only the rendered one is laid out fresh, drawn, and
                    // given events.
                    let mut keys = Vec::with_capacity(tabs.len());
                    let mut children: Vec<ThemedElement<'_, Message>> =
                        Vec::with_capacity(tabs.len());
                    let mut rendered_index = 0;
                    for (index, tab) in tabs.iter().enumerate() {
                        if rendered_tab == Some(tab.id()) {
                            rendered_index = index;
                        }
                        keys.push(tab.id());
                        children.push(match tab.binding() {
                            Some(slot) => match sessions.get(slot.session_id) {
                                Some(session) => {
                                    let session_id = slot.session_id;
                                    let wrap = move |msg| Message::SessionPaneUserAction {
                                        session_id,
                                        msg,
                                    };
                                    if slot.key == MAIN_PANE_KEY {
                                        session.pane_body().map(wrap)
                                    } else {
                                        session.script_pane_body(slot.key).map(wrap)
                                    }
                                }
                                None => iced::widget::Space::new().into(),
                            },
                            // A placeholder tab awaiting materialization has
                            // no body yet.
                            None => iced::widget::Space::new().into(),
                        });
                    }
                    let body: ThemedElement<'_, Message> =
                        widgets::tab_host::TabHost::new(keys, children, rendered_index).into();

                    // A hidden pane that still renders (toolbar expanded, or
                    // every pane hidden) is marked, not removed: a veil of
                    // the window background with a red ✕, showing what
                    // collapsing the toolbar will drop. The stack is emitted
                    // in BOTH states, the veil layer an inert placeholder
                    // while the pane is visible: chrome around a stateful
                    // subtree must keep one element shape (the rule the
                    // grid-level drag overlay follows) — swapping the bare
                    // body for a stack across the toggle would change the
                    // subtree's element type, making iced rebuild it and
                    // erase every mounted tab's widget state on each
                    // hide/unhide. The veil layer itself is stateless, so
                    // its own container↔placeholder swap rebuilds nothing
                    // that matters.
                    let veil: ThemedElement<'_, Message> = if is_hidden {
                        container(
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
                        .style(theme::builtins::container::pane_hidden_overlay)
                        .into()
                    } else {
                        iced::widget::Space::new().into()
                    };
                    let body: ThemedElement<'_, Message> = stack(vec![body, veil]).into();
                    let content = pane_grid::Content::new(body);

                    // Strip emission ([`Self::strip_emitted`]): the count is
                    // of listed tabs, not members, so a group whose other
                    // members are all eyeball-hidden under a collapsed
                    // toolbar renders as a plain pane, indistinguishable
                    // from a true singleton. (Invisible vacancy tabs never
                    // list: a bound singleton sharing its group with vacancy
                    // bookkeeping is still a singleton to the user.)
                    let listed_tabs = tabs
                        .iter()
                        .filter(|tab| self.tab_in_strip(tab, show_all))
                        .count();
                    let show_header = Self::strip_emitted(
                        listed_tabs,
                        rendered.is_some(),
                        rendered.is_some_and(|(slot, session)| {
                            session.title_bar_policy(slot.key) == TitleBarPolicy::AlwaysShow
                        }),
                        self.toolbar_expanded,
                        hide_headers,
                        drag.live,
                    );
                    if !show_header {
                        return content;
                    }

                    // The strip renders from plain descriptors: one per
                    // listed tab ([`Self::tab_in_strip`]), in strip order —
                    // the same filter the emission count and the drag
                    // classifier's tab bands apply, so the strip never
                    // disagrees with either. An unbound tab renders from the
                    // durable record standing behind it (a pending pane), so
                    // a placeholder holds its strip position exactly as its
                    // body holds its geometry. The placeholder descriptors
                    // are inert beyond selection: every press, connect,
                    // close, and visibility handler requires a binding.
                    let descriptors: smallvec::SmallVec<[components::tab_strip::TabDescriptor; 8]> =
                        tabs.iter()
                            .filter(|tab| self.tab_in_strip(tab, show_all))
                            .filter_map(|tab| match tab.binding() {
                                Some(slot) => {
                                    let session = sessions.get(slot.session_id)?;
                                    Some(components::tab_strip::TabDescriptor {
                                        id: tab.id(),
                                        label: session.pane_label(slot.key),
                                        main: slot.key == MAIN_PANE_KEY,
                                        selected: selected_tab == Some(tab.id()),
                                        rendered: rendered_tab == Some(tab.id()),
                                        hidden: self.hidden_panes.contains(slot),
                                        active_session: self.active_session_id
                                            == Some(slot.session_id),
                                        connected: session.is_connected(),
                                        ever_connected: session.ever_connected(),
                                    })
                                }
                                None => {
                                    let placeholder = self.placeholder_descriptor(tab.id())?;
                                    Some(components::tab_strip::TabDescriptor {
                                        id: tab.id(),
                                        label: placeholder.label,
                                        // Never `main`: the main-tab controls
                                        // (connect, close) act through a
                                        // binding no placeholder has.
                                        main: false,
                                        selected: selected_tab == Some(tab.id()),
                                        rendered: false,
                                        hidden: placeholder.hidden,
                                        active_session: false,
                                        connected: false,
                                        ever_connected: false,
                                    })
                                }
                            })
                            .collect();
                    let strip = components::tab_strip::view(
                        self.strip_id(group),
                        descriptors,
                        |tab| self.tab_anchor(tab),
                        components::tab_strip::StripContext {
                            drag_live: drag.live,
                            modifiers: drag.modifiers,
                            visibility_eyes: show_all,
                            on_strip_bounds: Box::new(move |bounds| {
                                self.strip_bands.borrow_mut().insert(group, bounds);
                            }),
                            on_tab_bounds: std::rc::Rc::new(|tab, bounds| {
                                let mut spans = self.tab_spans.borrow_mut();
                                if spans.get(&tab) != Some(&bounds) {
                                    // The announcement exists solely for the
                                    // scripted drag matrix, which aims real
                                    // input at tab bounds without probing.
                                    // Harness runs only (debug builds under
                                    // the autosession hook): even
                                    // change-gated, a divider drag
                                    // re-measures every tab per layout pass,
                                    // and that stream must never reach a
                                    // normal-play log. The mirror write is
                                    // the real product.
                                    #[cfg(debug_assertions)]
                                    if crate::spike_forensics_enabled()
                                        && let Some(slot) = self
                                            .layout
                                            .tab(tab)
                                            .and_then(|t| t.binding().copied())
                                    {
                                        log::info!(
                                            "[pane-drag] tab {tab:?} ({}/{}) bounds window={:?}: ({:.1}, {:.1}) {:.1}x{:.1}",
                                            slot.session_id,
                                            slot.key,
                                            self.window_id,
                                            bounds.x,
                                            bounds.y,
                                            bounds.width,
                                            bounds.height,
                                        );
                                    }
                                    spans.insert(tab, bounds);
                                }
                            }),
                        },
                        move |event| Message::TabStrip(group, event),
                    );

                    // The active session's panes tint their whole header band
                    // (the pre-pane UI carried this on the session tab).
                    let bar_style = move |theme: &crate::Theme| {
                        if is_active {
                            theme::builtins::container::pane_title_bar_active(theme)
                        } else {
                            theme::builtins::container::pane_title_bar(theme)
                        }
                    };

                    // The title bar carries only the strip: each tab is its
                    // own drag handle (the press surface inside it), and the
                    // embedded session/pane controls live inside their tabs.
                    let title_bar = pane_grid::TitleBar::new(strip).padding(2).style(bar_style);

                    content.title_bar(title_bar)
                })
                .width(Length::Fill)
                .height(Length::Fill)
                .spacing(GRID_SPACING)
                .on_click(Message::PaneClicked)
                .on_resize(8, Message::PaneResized);

                let grid_element: ThemedElement<'_, Message> = panes.into();

                // The drop-target overlay renders the daemon's classified
                // target — the exact geometry the drop will use.
                //
                // The overlay layer is ALWAYS present (empty and inert when
                // no drag hovers): swapping the element between `grid` and
                // `stack(grid, overlay)` changes the widget type at this
                // tree position, and iced's diff then rebuilds the whole
                // subtree with fresh state — which erases in-flight press
                // state the instant a drag goes live (measured: the
                // mid-drag release then arrived while the surface was
                // idle and was silently discarded). A shape-stable tree
                // keeps widget state across hover transitions.
                stack(vec![
                    grid_element,
                    drag_overlay::TargetOverlay::new(target_overlay_rects(drag.target)).into(),
                ])
                .into()
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

            let empty_state: ThemedElement<'_, Message> = container(
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
            .into();
            // An empty window is a whole-window drop surface (adopt as the
            // first cluster); its overlay stays mounted for the same
            // shape-stability reason as the grid's.
            stack(vec![
                empty_state,
                drag_overlay::TargetOverlay::new(target_overlay_rects(drag.target)).into(),
            ])
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

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_core::session::runtime::pane::{
        DefStateSpec, PaneKind, PaneNamespace, PaneRegistry,
    };

    fn main_pane(session: u32) -> PaneRef {
        PaneRef {
            session_id: SessionId::from(session),
            key: MAIN_PANE_KEY,
        }
    }

    /// A real non-main pane reference, keyed through the registry (the only
    /// minting authority for pane keys).
    fn script_pane(session: u32) -> PaneRef {
        let key = PaneRegistry::new()
            .split(
                &PaneNamespace::User,
                "notes",
                PaneKind::Terminal,
                DefStateSpec {
                    title_bar: None,
                    hidden: None,
                    font_size: None,
                },
                None,
            )
            .expect("a fresh registry accepts a plain split")
            .def
            .key;
        PaneRef {
            session_id: SessionId::from(session),
            key,
        }
    }

    fn changed(before: PaneRef, after: PaneRef) -> RenderedSlotChange {
        RenderedSlotChange {
            before: Some(before),
            after: Some(after),
        }
    }

    fn unchanged(rendered: PaneRef) -> RenderedSlotChange {
        RenderedSlotChange {
            before: Some(rendered),
            after: Some(rendered),
        }
    }

    #[test]
    fn both_selected_swap_flips_the_active_session_identity() {
        let a = main_pane(10);
        let b = main_pane(20);
        // Both panes rendered at their groups' selected slots: each side's
        // rendered pane changed to the other.
        let changes = [changed(a, b), changed(b, a)];

        assert_eq!(
            active_after_pane_swap(Some(a.session_id), &changes),
            Some(b.session_id)
        );
        assert_eq!(
            active_after_pane_swap(Some(b.session_id), &changes),
            Some(a.session_id)
        );
    }

    #[test]
    fn both_unselected_swap_moves_no_attention() {
        let a = main_pane(10);
        let b = main_pane(20);
        // Neither swapped pane was on screen: each involved group keeps
        // rendering whatever its selected slot already showed.
        let changes = [unchanged(main_pane(30)), unchanged(main_pane(40))];

        assert_eq!(
            active_after_pane_swap(Some(a.session_id), &changes),
            Some(a.session_id)
        );
        assert_eq!(
            active_after_pane_swap(Some(b.session_id), &changes),
            Some(b.session_id)
        );
    }

    #[test]
    fn mixed_swap_with_the_active_session_on_the_rendered_side_transfers() {
        let a = main_pane(10);
        let b = main_pane(20);
        // `a` rendered at its group's selected slot and was swapped with an
        // off-screen `b`: only `a`'s side changed on screen, and it now
        // renders `b`.
        let changes = [changed(a, b), unchanged(main_pane(30))];

        assert_eq!(
            active_after_pane_swap(Some(a.session_id), &changes),
            Some(b.session_id)
        );
    }

    #[test]
    fn mixed_swap_with_the_active_session_on_the_unrendered_side_stays() {
        let a = main_pane(10);
        let b = main_pane(20);
        // The active session's pane was the off-screen half: nothing it was
        // showing changed, so activation stays where it is.
        let changes = [changed(a, b), unchanged(main_pane(30))];

        assert_eq!(
            active_after_pane_swap(Some(b.session_id), &changes),
            Some(b.session_id)
        );
    }

    #[test]
    fn newly_rendered_script_pane_carries_activation_to_its_session() {
        let a = main_pane(10);
        let b = script_pane(20);
        // The pane arriving at the rendered slot is a script pane: active
        // follows its session, exactly as selecting that tab would.
        let changes = [changed(a, b), unchanged(main_pane(30))];

        assert_eq!(
            active_after_pane_swap(Some(a.session_id), &changes),
            Some(b.session_id)
        );
    }

    #[test]
    fn uninvolved_active_session_is_untouched() {
        let a = main_pane(10);
        let b = main_pane(20);
        let other = SessionId::from(30);
        let changes = [changed(a, b), changed(b, a)];

        assert_eq!(active_after_pane_swap(Some(other), &changes), Some(other));
        assert_eq!(active_after_pane_swap(None, &changes), None);
    }

    #[test]
    fn slot_left_rendering_nothing_keeps_the_active_session() {
        let a = main_pane(10);
        // The incoming pane is hidden in the destination window: the slot
        // renders nothing, so there is no session to follow.
        let changes = [RenderedSlotChange {
            before: Some(a),
            after: None,
        }];

        assert_eq!(
            active_after_pane_swap(Some(a.session_id), &changes),
            Some(a.session_id)
        );
    }

    #[test]
    fn tab_cycling_wraps_in_both_directions() {
        assert_eq!(cycle_index(0, 3, false), 1);
        assert_eq!(cycle_index(2, 3, false), 0);
        assert_eq!(cycle_index(0, 3, true), 2);
        assert_eq!(cycle_index(1, 3, true), 0);
        assert_eq!(cycle_index(0, 1, false), 0);
        assert_eq!(cycle_index(0, 1, true), 0);
    }

    // ------------------------------------------------------------------
    // Lifecycle matrix: window-level group awareness of the daemon-driven
    // pane operations (placement, relocate, tear-out, swap halves, session
    // purge, visibility, repair). Session runtimes are not involved — these
    // exercise the layout/binding/attention layer only.
    // ------------------------------------------------------------------

    fn test_window() -> SmudgyWindow {
        SmudgyWindow::new(window::Id::unique(), crate::cloud_account::test_handles())
    }

    /// Distinct non-main pane references for one session, keyed through a
    /// single registry (two panes of one session must not collide).
    fn script_panes_for(session: u32, count: usize) -> Vec<PaneRef> {
        let mut registry = PaneRegistry::new();
        (0..count)
            .map(|index| {
                let key = registry
                    .split(
                        &PaneNamespace::User,
                        &format!("notes{index}"),
                        PaneKind::Terminal,
                        DefStateSpec {
                            title_bar: None,
                            hidden: None,
                            font_size: None,
                        },
                        None,
                    )
                    .expect("a fresh registry accepts plain splits")
                    .def
                    .key;
                PaneRef {
                    session_id: SessionId::from(session),
                    key,
                }
            })
            .collect()
    }

    /// Stack `slot`'s tab into `target`'s group at `index` — the test-side
    /// stand-in for a header-drop grouping gesture.
    fn merge_into_group(window: &mut SmudgyWindow, slot: PaneRef, target: PaneRef, index: usize) {
        let group = window.group_of_slot(target).expect("target hosted");
        let (tab, _, _) = window.extract_pane_tab(slot).expect("slot hosted");
        window.adopt_drag_tab_merge(tab, group, index);
    }

    fn host_cluster(window: &mut SmudgyWindow, slot: PaneRef) {
        window.host_as_cluster(Tab::bound(slot));
        window.mark_grid_dirty();
        window.flush_grid_rebuild();
    }

    fn place_beside(window: &mut SmudgyWindow, slot: PaneRef, reference: PaneKey) {
        window.place_session_pane(
            slot.session_id,
            slot.key,
            PanePlacement::Split {
                reference,
                direction: SplitDirection::Right,
                size_px: None,
            },
        );
    }

    #[test]
    fn pane_opened_reference_inside_a_group_splits_beside_the_whole_group() {
        let mut window = test_window();
        let main = main_pane(1);
        let scripts = script_panes_for(1, 2);
        host_cluster(&mut window, main);
        place_beside(&mut window, scripts[0], MAIN_PANE_KEY);
        merge_into_group(&mut window, scripts[0], main, 1);
        let group = window.group_of_slot(main).expect("main hosted");
        assert_eq!(window.layout.tabs(group).map(<[_]>::len), Some(2));

        // The new pane's reference is a tab inside the two-tab group: the
        // split lands beside the whole group, never inside it.
        place_beside(&mut window, scripts[1], scripts[0].key);
        let new_group = window.group_of_slot(scripts[1]).expect("pane placed");
        assert_ne!(new_group, group);
        assert_eq!(
            window.layout.tabs(group).map(<[_]>::len),
            Some(2),
            "the reference group must not gain a tab from placement"
        );
        assert_eq!(window.layout.tabs(new_group).map(<[_]>::len), Some(1));
        assert_eq!(window.layout.groups_depth_first().len(), 2);
    }

    #[test]
    fn same_group_reorder_marks_the_workspace_dirty_without_a_grid_rebuild() {
        let mut window = test_window();
        let mut sessions = SessionStore::new(crate::cloud_account::test_handles());
        let main = main_pane(1);
        let scripts = script_panes_for(1, 1);
        host_cluster(&mut window, main);
        place_beside(&mut window, scripts[0], MAIN_PANE_KEY);
        merge_into_group(&mut window, scripts[0], main, 1);
        window.flush_grid_rebuild();
        let _ = window.take_workspace_dirty(); // drain the setup's marks

        let group = window.group_of_slot(main).expect("main hosted");
        let tab = window.tab_of(scripts[0]).expect("script hosted");
        let applied = window.apply_drag_merge(tab, group, 0, &mut sessions);
        assert!(applied.is_some(), "a same-group reorder applies");
        assert_eq!(
            window.layout.tabs(group).map(|tabs| tabs[0].id()),
            Some(tab),
            "the reorder moved the tab to slot 0"
        );
        // Strip order is persisted state: the reorder owes the workspace
        // mark even though the grid configuration needs no rebuild.
        assert!(window.take_workspace_dirty());
        assert!(!window.grid_dirty, "a reorder never re-derives the grid");
    }

    #[test]
    fn scripted_tab_creation_inserts_after_reference_without_selecting() {
        let mut window = test_window();
        let main = main_pane(1);
        let script = script_panes_for(1, 1)[0];
        host_cluster(&mut window, main);

        window.place_session_pane(
            script.session_id,
            script.key,
            PanePlacement::Tab {
                reference: MAIN_PANE_KEY,
                position: TabPosition::After,
                selected: false,
            },
        );

        let group = window.group_of_slot(main).expect("main hosted");
        let tabs = window.layout.tabs(group).expect("group exists");
        assert_eq!(tabs.len(), 2);
        assert_eq!(tabs[0].binding(), Some(&main));
        assert_eq!(tabs[1].binding(), Some(&script));
        assert_eq!(window.layout.selected(group), Some(tabs[0].id()));
    }

    #[test]
    fn scripted_grouping_reorders_main_panes() {
        let mut window = test_window();
        let first = main_pane(1);
        let second = main_pane(2);
        host_cluster(&mut window, first);
        host_cluster(&mut window, second);

        assert!(window.group_pane_with(first, second, TabPosition::After));
        let group = window.group_of_slot(second).expect("target hosted");
        let tabs = window.layout.tabs(group).expect("group exists");
        assert_eq!(
            tabs.iter()
                .filter_map(|tab| tab.binding().copied())
                .collect::<Vec<_>>(),
            vec![second, first]
        );
        assert_eq!(window.layout.groups_depth_first().len(), 1);

        assert!(window.group_pane_with(first, second, TabPosition::Before));
        let tabs = window.layout.tabs(group).expect("group exists");
        assert_eq!(
            tabs.iter()
                .filter_map(|tab| tab.binding().copied())
                .collect::<Vec<_>>(),
            vec![first, second]
        );
    }

    #[test]
    fn script_select_keeps_hidden_pane_hidden_but_activates_its_session() {
        let mut window = test_window();
        let mut sessions = SessionStore::new(crate::cloud_account::test_handles());
        let main = main_pane(1);
        let hidden = script_panes_for(2, 1)[0];
        host_cluster(&mut window, main);
        host_cluster(&mut window, hidden);
        merge_into_group(&mut window, hidden, main, 1);
        window.set_toolbar_expanded(false);
        window.set_pane_hidden(hidden, true);

        let group = window.group_of_slot(main).expect("group exists");
        let hidden_tab = window.tab_of(hidden).expect("hidden pane hosted");
        let _ = window.select_pane_without_focus(hidden, &mut sessions);

        assert_eq!(window.layout.selected(group), Some(hidden_tab));
        assert_eq!(window.rendered_slot(group), Some(main));
        assert!(window.pane_hidden(hidden));
        assert_eq!(window.active_session_id(), Some(hidden.session_id));
    }

    #[test]
    fn selection_blurs_only_the_input_it_displaced() {
        let old_tab = main_pane(1);
        let new_tab = main_pane(2);
        let other_group = main_pane(3);

        assert_eq!(
            displaced_input_for_selection(false, Some(new_tab), Some(old_tab), Some(other_group),),
            Some(old_tab),
            "script selection blurs the pane it actually obscured"
        );
        assert_eq!(
            displaced_input_for_selection(false, Some(new_tab), Some(new_tab), Some(other_group),),
            None,
            "an unchanged background tab must not disturb another focus group/window"
        );
        assert_eq!(
            displaced_input_for_selection(true, Some(new_tab), Some(new_tab), Some(other_group),),
            Some(other_group),
            "ordinary re-selection still releases the previous focus group"
        );
    }

    #[test]
    fn relocation_detaches_one_tab_and_splits_beside_the_reference_group() {
        let mut window = test_window();
        let (main1, main2) = (main_pane(1), main_pane(2));
        let scripts = script_panes_for(1, 1);
        host_cluster(&mut window, main1);
        host_cluster(&mut window, main2);
        place_beside(&mut window, scripts[0], MAIN_PANE_KEY);
        merge_into_group(&mut window, scripts[0], main1, 1);

        let group1 = window.group_of_slot(main1).expect("main1 hosted");
        let tab_before = window.tab_of(scripts[0]).expect("script hosted");
        let (tab, hidden, emptied) = window
            .extract_pane_tab(scripts[0])
            .expect("hosted tab extracts");
        assert!(!hidden);
        assert!(!emptied);
        assert_eq!(
            window.layout.tabs(group1).map(<[_]>::len),
            Some(1),
            "detaching one tab preserves the remaining group"
        );

        window.adopt_tab_beside(tab, main2, SplitDirection::Bottom, None);
        assert_eq!(
            window.tab_of(scripts[0]),
            Some(tab_before),
            "a relocation carries the tab's stable id"
        );
        let landed = window.group_of_slot(scripts[0]).expect("re-hosted");
        let group2 = window.group_of_slot(main2).expect("main2 hosted");
        assert_ne!(landed, group2, "a relocation splits beside, never inside");
        assert_eq!(window.layout.tabs(group2).map(<[_]>::len), Some(1));
    }

    #[test]
    fn relocation_reference_fallback_lands_beside_main_then_a_fresh_cluster() {
        // Reference vanished: the split placement chain falls back to the
        // session's main pane's group.
        let mut window = test_window();
        let main = main_pane(1);
        let scripts = script_panes_for(1, 2);
        host_cluster(&mut window, main);
        window.adopt_tab_beside(
            Tab::bound(scripts[0]),
            scripts[1], // never hosted
            SplitDirection::Right,
            None,
        );
        assert!(window.hosts_pane(scripts[0].session_id, scripts[0].key));
        assert_eq!(window.layout.groups_depth_first().len(), 2);

        // Neither reference nor main hosted here: an even top-level share.
        let mut other = test_window();
        host_cluster(&mut other, main_pane(2));
        other.adopt_tab_beside(Tab::bound(scripts[1]), main, SplitDirection::Right, None);
        assert!(other.hosts_pane(scripts[1].session_id, scripts[1].key));
        assert_eq!(other.layout.groups_depth_first().len(), 2);
    }

    #[test]
    fn tear_out_detach_preserves_the_group_and_the_tab_identity() {
        let mut window = test_window();
        let main = main_pane(1);
        let scripts = script_panes_for(1, 1);
        host_cluster(&mut window, main);
        place_beside(&mut window, scripts[0], MAIN_PANE_KEY);
        merge_into_group(&mut window, scripts[0], main, 1);

        let (tab, _, emptied) = window
            .extract_pane_tab(scripts[0])
            .expect("hosted tab extracts");
        assert!(!emptied, "the remaining group keeps the window alive");
        assert!(window.hosts_pane(main.session_id, main.key));

        let id = tab.id();
        let mut torn_out = test_window();
        torn_out.adopt_torn_out_tab(tab);
        assert!(torn_out.hosts_pane(scripts[0].session_id, scripts[0].key));
        assert_eq!(
            torn_out.tab_of(scripts[0]),
            Some(id),
            "the tab's stable id crosses the window change"
        );
        assert!(!torn_out.toolbar_expanded);

        // Extracting the window's final pane reports the emptied state the
        // empty-window rule consumes.
        let (_, _, emptied) = window.extract_pane_tab(main).expect("main extracts");
        assert!(emptied);
    }

    #[test]
    fn swap_between_two_inactive_tabs_changes_nothing_on_screen() {
        let mut window = test_window();
        let (main1, main2) = (main_pane(1), main_pane(2));
        let s1 = script_panes_for(1, 1)[0];
        let s2 = script_panes_for(2, 1)[0];
        host_cluster(&mut window, main1);
        host_cluster(&mut window, main2);
        place_beside(&mut window, s1, MAIN_PANE_KEY);
        place_beside(&mut window, s2, MAIN_PANE_KEY);
        merge_into_group(&mut window, s1, main1, 1);
        merge_into_group(&mut window, s2, main2, 1);
        let group1 = window.group_of_slot(main1).expect("hosted");
        let group2 = window.group_of_slot(main2).expect("hosted");
        window.active_session_id = Some(main1.session_id);

        // Both swapped panes sit behind their groups' selected mains.
        let probe = window.pane_swap_render_probe(s1, s2);
        window.swap_pane_slots(s1, s2);
        window.settle_active_session_after_pane_swap(probe);

        assert_eq!(window.group_of_slot(s2), Some(group1));
        assert_eq!(window.group_of_slot(s1), Some(group2));
        assert_eq!(window.rendered_slot(group1), Some(main1));
        assert_eq!(window.rendered_slot(group2), Some(main2));
        assert_eq!(window.active_session_id, Some(main1.session_id));
    }

    #[test]
    fn swap_with_a_rendered_side_moves_attention_with_the_rendered_slot() {
        let mut window = test_window();
        let (main1, main2) = (main_pane(1), main_pane(2));
        host_cluster(&mut window, main1);
        host_cluster(&mut window, main2);
        let group1 = window.group_of_slot(main1).expect("hosted");
        let group2 = window.group_of_slot(main2).expect("hosted");
        window.active_session_id = Some(main1.session_id);

        let probe = window.pane_swap_render_probe(main1, main2);
        window.swap_pane_slots(main1, main2);
        window.settle_active_session_after_pane_swap(probe);

        // Selection followed the slot on both sides, so each group renders
        // the incoming pane; activation followed the rendered position.
        assert_eq!(window.rendered_slot(group1), Some(main2));
        assert_eq!(window.rendered_slot(group2), Some(main1));
        assert_eq!(window.active_session_id, Some(main2.session_id));
    }

    #[test]
    fn same_group_swap_exchanges_strip_slots_and_selection_follows_the_slot() {
        let mut window = test_window();
        let main = main_pane(1);
        let script = script_panes_for(1, 1)[0];
        host_cluster(&mut window, main);
        place_beside(&mut window, script, MAIN_PANE_KEY);
        merge_into_group(&mut window, script, main, 1);
        let group = window.group_of_slot(main).expect("hosted");
        assert_eq!(window.rendered_slot(group), Some(main));
        window.active_session_id = Some(main.session_id);

        let probe = window.pane_swap_render_probe(main, script);
        window.swap_pane_slots(main, script);
        window.settle_active_session_after_pane_swap(probe);

        // The selected strip position now holds the script pane.
        assert_eq!(window.rendered_slot(group), Some(script));
        let tabs = window.layout.tabs(group).expect("group lives");
        assert_eq!(tabs[0].binding(), Some(&script));
        assert_eq!(tabs[1].binding(), Some(&main));
        assert_eq!(window.active_session_id, Some(script.session_id));
    }

    #[test]
    fn cross_window_swap_exchanges_bindings_and_tabs_stay_in_their_windows() {
        let mut first = test_window();
        let mut second = test_window();
        let (main1, main2) = (main_pane(1), main_pane(2));
        let script = script_panes_for(1, 1)[0];
        host_cluster(&mut first, main1);
        place_beside(&mut first, script, MAIN_PANE_KEY);
        merge_into_group(&mut first, script, main1, 1);
        host_cluster(&mut second, main2);
        first.active_session_id = Some(main1.session_id);
        second.active_session_id = Some(main2.session_id);

        // The inactive grouped script pane swaps with the other window's
        // rendered main — the two replace halves of `swap_script_panes`.
        let tab_first = first.tab_of(script).expect("hosted");
        let tab_second = second.tab_of(main2).expect("hosted");
        let probe_first = first.pane_swap_render_probe(script, main2);
        let probe_second = second.pane_swap_render_probe(script, main2);
        assert!(first.replace_pane_slot(script, main2));
        assert!(second.replace_pane_slot(main2, script));
        first.settle_active_session_after_pane_swap(probe_first);
        second.settle_active_session_after_pane_swap(probe_second);

        assert_eq!(
            first.tab_of(main2),
            Some(tab_first),
            "the tab stays in its window; only the pane binding moved"
        );
        assert_eq!(second.tab_of(script), Some(tab_second));
        // The inactive slot stayed inactive: the source group still renders
        // its main, so attention is untouched there.
        let group1 = first.group_of_slot(main1).expect("hosted");
        assert_eq!(first.rendered_slot(group1), Some(main1));
        assert_eq!(first.active_session_id, Some(main1.session_id));
        // The destination slot was rendered: it now shows the incoming pane
        // and attention followed it.
        let group2 = second.group_of_slot(script).expect("hosted");
        assert_eq!(second.rendered_slot(group2), Some(script));
        assert_eq!(second.active_session_id, Some(script.session_id));
    }

    #[test]
    fn session_close_purges_its_tabs_from_groups_with_collapse_semantics() {
        let mut window = test_window();
        let (main1, main2) = (main_pane(1), main_pane(2));
        let s2 = script_panes_for(2, 1)[0];
        host_cluster(&mut window, main1);
        host_cluster(&mut window, main2);
        place_beside(&mut window, s2, MAIN_PANE_KEY);
        merge_into_group(&mut window, main2, main1, 1);
        window.set_pane_hidden(s2, true);
        window.active_session_id = Some(main2.session_id);
        window.previous_active_session_id = None;

        let emptied = window.remove_session_slots(SessionId::from(2));
        assert!(!emptied);
        let group1 = window.group_of_slot(main1).expect("survivor hosted");
        assert_eq!(
            window.layout.tabs(group1).map(<[_]>::len),
            Some(1),
            "the dead session's tab left its group; the group survives"
        );
        assert!(!window.hosts_pane(s2.session_id, s2.key));
        assert!(
            !window.pane_hidden(s2),
            "hidden membership retires with the pane"
        );
        window.repair_active_session_without_focus();
        assert_eq!(window.active_session_id, Some(main1.session_id));

        assert!(window.remove_session_slots(SessionId::from(1)));
        assert!(window.layout.is_empty());
    }

    #[test]
    fn repair_prefers_a_session_whose_main_is_rendered() {
        let mut window = test_window();
        let (main10, main20) = (main_pane(10), main_pane(20));
        host_cluster(&mut window, main20);
        host_cluster(&mut window, main10);
        merge_into_group(&mut window, main10, main20, 1);
        // Group renders main20; main10's main is buried behind it.
        window.active_session_id = None;
        window.previous_active_session_id = None;
        window.repair_active_session_without_focus();
        assert_eq!(
            window.active_session_id,
            Some(SessionId::from(20)),
            "the rendered main outranks the lower session id"
        );
    }

    #[test]
    fn repair_falls_back_to_candidate_order_when_every_main_is_buried() {
        let mut window = test_window();
        let (main10, main20) = (main_pane(10), main_pane(20));
        let s10 = script_panes_for(10, 1)[0];
        let s20 = script_panes_for(20, 1)[0];
        host_cluster(&mut window, main10);
        host_cluster(&mut window, main20);
        place_beside(&mut window, s10, MAIN_PANE_KEY);
        place_beside(&mut window, s20, MAIN_PANE_KEY);
        merge_into_group(&mut window, s10, main10, 1);
        merge_into_group(&mut window, s20, main20, 1);
        // Select the script tabs so both mains are obscured.
        let tab10 = window.tab_of(s10).expect("hosted");
        let tab20 = window.tab_of(s20).expect("hosted");
        assert!(window.layout.select(tab10));
        assert!(window.layout.select(tab20));

        window.active_session_id = None;
        window.previous_active_session_id = None;
        window.repair_active_session_without_focus();
        assert_eq!(window.active_session_id, Some(SessionId::from(10)));
    }

    #[test]
    fn repair_previous_active_yields_to_a_rendered_main_when_buried() {
        let mut window = test_window();
        let (main10, main20, main30) = (main_pane(10), main_pane(20), main_pane(30));
        host_cluster(&mut window, main10);
        host_cluster(&mut window, main20);
        host_cluster(&mut window, main30);
        merge_into_group(&mut window, main20, main30, 1);
        // main30 renders; main20 (the previously active session) is buried.
        window.active_session_id = None;
        window.previous_active_session_id = Some(SessionId::from(20));
        window.repair_active_session_without_focus();
        assert_eq!(
            window.active_session_id,
            Some(SessionId::from(10)),
            "a buried previous session yields to a rendered main"
        );

        // With its main rendered, the previous session wins outright.
        let tab20 = window.tab_of(main20).expect("hosted");
        assert!(window.layout.select(tab20));
        window.active_session_id = None;
        window.previous_active_session_id = Some(SessionId::from(20));
        window.repair_active_session_without_focus();
        assert_eq!(window.active_session_id, Some(SessionId::from(20)));
    }

    #[test]
    fn repair_in_an_all_hidden_window_behaves_as_if_visible() {
        let mut window = test_window();
        let (main10, main20) = (main_pane(10), main_pane(20));
        host_cluster(&mut window, main10);
        host_cluster(&mut window, main20);
        window.set_toolbar_expanded(false);
        window.set_pane_hidden(main10, true);
        window.set_pane_hidden(main20, true);
        // Every pane hidden: the hidden filter is inert, so both mains
        // render and the preference changes nothing.
        window.active_session_id = None;
        window.previous_active_session_id = None;
        window.repair_active_session_without_focus();
        assert_eq!(window.active_session_id, Some(SessionId::from(10)));

        // One pane un-hidden re-arms the filter: the still-hidden main is
        // no longer a preferred candidate.
        window.set_pane_hidden(main20, false);
        window.active_session_id = None;
        window.repair_active_session_without_focus();
        assert_eq!(window.active_session_id, Some(SessionId::from(20)));
    }

    #[test]
    fn hiding_a_grouped_rendered_tab_falls_back_to_a_visible_member() {
        let mut window = test_window();
        let (main10, main20) = (main_pane(10), main_pane(20));
        host_cluster(&mut window, main10);
        host_cluster(&mut window, main20);
        merge_into_group(&mut window, main20, main10, 1);
        window.set_toolbar_expanded(false);
        let group = window.group_of_slot(main10).expect("hosted");
        assert_eq!(window.rendered_slot(group), Some(main10));

        window.set_pane_hidden(main10, true);
        assert_eq!(
            window.rendered_slot(group),
            Some(main20),
            "the hidden selection derives a visible stand-in"
        );
        // Hosting queries still include the hidden tab.
        assert!(window.hosts_pane(main10.session_id, main10.key));

        window.set_pane_hidden(main10, false);
        assert_eq!(
            window.rendered_slot(group),
            Some(main10),
            "the durable selection returns the moment it is visible"
        );
    }

    /// A window hosting one two-tab group (main + one script pane merged
    /// beside it), the script pane eyeball-hidden.
    fn window_with_hidden_second_tab() -> (SmudgyWindow, PaneRef, PaneRef) {
        let mut window = test_window();
        let main = main_pane(1);
        let script = script_panes_for(1, 1)[0];
        host_cluster(&mut window, main);
        place_beside(&mut window, script, MAIN_PANE_KEY);
        merge_into_group(&mut window, script, main, 1);
        window.set_pane_hidden(script, true);
        window.flush_grid_rebuild();
        (window, main, script)
    }

    /// Which of `group`'s member tabs the strip lists under the window's
    /// current visibility mode, in strip order.
    fn strip_listing(window: &SmudgyWindow, group: GroupId) -> Vec<bool> {
        let show_all = window.show_all();
        window
            .layout
            .tabs(group)
            .expect("group exists")
            .iter()
            .map(|tab| window.tab_in_strip(tab, show_all))
            .collect()
    }

    #[test]
    fn strip_lists_hidden_tabs_only_in_rearrange_mode() {
        let (mut window, main, _) = window_with_hidden_second_tab();
        let group = window.group_of_slot(main).expect("main hosted");

        // Expanded (rearrange mode): the hidden tab stays listed, dimmed
        // behind its eye.
        assert_eq!(strip_listing(&window, group), vec![true, true]);
        // Collapsed: the hidden tab drops out of the strip with its pane.
        window.set_toolbar_expanded(false);
        window.flush_grid_rebuild();
        assert_eq!(strip_listing(&window, group), vec![true, false]);
        // All panes hidden: the inert-filter exception lists everything
        // again (the panes render, so their tabs must too).
        window.set_pane_hidden(main, true);
        window.flush_grid_rebuild();
        assert_eq!(strip_listing(&window, group), vec![true, true]);
    }

    #[test]
    fn a_group_collapsed_to_one_listed_tab_emits_no_strip() {
        let (mut window, main, _) = window_with_hidden_second_tab();
        let group = window.group_of_slot(main).expect("main hosted");
        window.set_toolbar_expanded(false);
        window.flush_grid_rebuild();

        let listed = |window: &SmudgyWindow| {
            strip_listing(window, group)
                .into_iter()
                .filter(|listed| *listed)
                .count()
        };
        // One listed tab under a collapsed toolbar, global hide on,
        // policy unpinned, no drag: a plain pane, exactly like a true
        // singleton.
        assert_eq!(listed(&window), 1);
        assert!(!SmudgyWindow::strip_emitted(
            listed(&window),
            true,
            false,
            false,
            true,
            false
        ));
        // A live drag still mounts the temporary header band — the merge
        // surface a drop needs.
        assert!(SmudgyWindow::strip_emitted(
            listed(&window),
            true,
            false,
            false,
            true,
            true
        ));
        // Expanded, the group is multi-tab again and the strip returns
        // regardless of the other clauses.
        window.set_toolbar_expanded(true);
        window.flush_grid_rebuild();
        assert_eq!(listed(&window), 2);
        assert!(SmudgyWindow::strip_emitted(
            listed(&window),
            true,
            false,
            true,
            true,
            false
        ));
    }

    #[test]
    fn drag_bands_skip_tabs_the_collapsed_strip_does_not_list() {
        let (mut window, main, script) = window_with_hidden_second_tab();
        window.set_toolbar_expanded(false);
        window.grid_area.set(Size::new(800.0, 600.0));
        window.mark_grid_dirty();
        window.flush_grid_rebuild();

        // Draw-time mirrors as a paint would leave them — including a
        // span for the hidden tab, on file from before the hide (spans
        // persist stale-until-rendered).
        let main_tab = window.tab_of(main).expect("main hosted");
        let script_tab = window.tab_of(script).expect("script hosted");
        window.tab_spans.borrow_mut().insert(
            main_tab,
            iced::Rectangle::new(Point::new(2.0, 0.0), Size::new(60.0, 22.0)),
        );
        window.tab_spans.borrow_mut().insert(
            script_tab,
            iced::Rectangle::new(Point::new(64.0, 0.0), Size::new(60.0, 22.0)),
        );

        let drag = TabDrag {
            source_window: window::Id::unique(),
            tab: main_tab,
            slot: main,
            source_group: window.group_of_slot(main).expect("main hosted"),
            press: Point::ORIGIN,
            hover: None,
        };
        // A header hit past the sole listed tab (y below the grid-edge
        // band, inside the default header band): the stale hidden span
        // must not classify as a between-tabs insertion — the drop
        // appends right behind the listed member (member space).
        let target = window
            .classify_drag_target(Point::new(100.0, 20.0), Size::new(800.0, 600.0), &drag)
            .expect("the header band classifies");
        let group = window.group_of_slot(main).expect("main hosted");
        assert_eq!(
            target.action,
            pane_drag::DragAction::Merge { group, slot: 1 }
        );
    }

    // ------------------------------------------------------------------
    // Workspace restoration and vacancy semantics
    // ------------------------------------------------------------------

    use crate::pane_groups::Blueprint;
    use crate::workspace::restore::{DescriptorKey, PendingPane};

    fn user_descriptor(name: &str) -> DescriptorKey {
        DescriptorKey::User {
            name: name.to_string(),
        }
    }

    /// A restored window: session 1's main bound, plus two placeholders
    /// (notes0 visible, notes1 eyeball-hidden) in a 300px split, notes1
    /// durably selected. Returns the window and the placeholder tab ids.
    fn restored_window() -> (SmudgyWindow, TabId, TabId) {
        let session = SessionId::from(1);
        let main_tab = Tab::bound(main_pane(1));
        let notes0 = Tab::placeholder();
        let notes1 = Tab::placeholder();
        let (id0, id1) = (notes0.id(), notes1.id());
        let layout = GroupLayout::from_blueprint(vec![(
            1.0,
            Blueprint::Split {
                axis: pane_grid::Axis::Vertical,
                sizing: SplitSizing::Px {
                    px: 300.0,
                    sized_first: false,
                },
                a: Box::new(Blueprint::Group {
                    tabs: vec![main_tab],
                    selected: 0,
                }),
                b: Box::new(Blueprint::Group {
                    tabs: vec![notes0, notes1],
                    selected: 1,
                }),
            },
        )]);
        let mut window = test_window();
        window.install_applied_layout(
            layout,
            vec![
                (
                    session,
                    user_descriptor("notes0"),
                    PendingPane {
                        tab: id0,
                        hidden: false,
                    },
                ),
                (
                    session,
                    user_descriptor("notes1"),
                    PendingPane {
                        tab: id1,
                        hidden: true,
                    },
                ),
            ],
            Vec::new(),
            Vec::new(),
            Some(session),
        );
        window.set_toolbar_expanded(false);
        window.flush_grid_rebuild();
        (window, id0, id1)
    }

    #[test]
    fn strip_lists_pre_hidden_placeholders_only_in_rearrange_mode() {
        let (mut window, notes0, notes1) = restored_window();
        let in_strip = |window: &SmudgyWindow, tab: TabId| {
            let show_all = window.show_all();
            let group = window.layout.group_of(tab).expect("tab hosted");
            window
                .layout
                .tabs(group)
                .expect("group exists")
                .iter()
                .find(|member| member.id() == tab)
                .map(|member| window.tab_in_strip(member, show_all))
                .expect("tab is a member")
        };
        // Collapsed (restored_window's mode): the visible placeholder
        // holds its strip position; the pre-hidden one is absent exactly
        // like the hidden pane it will bind into.
        assert!(in_strip(&window, notes0));
        assert!(!in_strip(&window, notes1));
        // Expanded, both list — the pre-hidden placeholder dims exactly
        // as the pane carrying its stored eyeball state will.
        window.set_toolbar_expanded(true);
        assert!(in_strip(&window, notes0));
        assert!(in_strip(&window, notes1));
    }

    #[test]
    fn delayed_out_of_order_materialization_reconstructs_the_same_model() {
        let session = SessionId::from(1);
        let scripts = script_panes_for(1, 2);

        let (mut ordered, ..) = restored_window();
        assert_eq!(
            ordered.claim_pending_pane(session, &user_descriptor("notes0"), scripts[0].key),
            Some(false)
        );
        assert_eq!(
            ordered.claim_pending_pane(session, &user_descriptor("notes1"), scripts[1].key),
            Some(true)
        );
        ordered.flush_grid_rebuild();

        let (mut reversed, ..) = restored_window();
        assert_eq!(
            reversed.claim_pending_pane(session, &user_descriptor("notes1"), scripts[1].key),
            Some(true)
        );
        assert_eq!(
            reversed.claim_pending_pane(session, &user_descriptor("notes0"), scripts[0].key),
            Some(false)
        );
        reversed.flush_grid_rebuild();

        // Arrival order changed nothing: same hosted panes, same strip
        // order, same durable selection, same split geometry.
        assert_eq!(ordered.describe_layout(), reversed.describe_layout());
        assert!(
            ordered.pane_hidden(scripts[1]),
            "stored eyeball state applied"
        );
        assert!(!ordered.pane_hidden(scripts[0]));
        assert_eq!(ordered.active_session_id(), Some(session));
        let mirror = ordered.layout().structure();
        assert!(matches!(
            mirror[0].1,
            pane_groups::StructureNode::Split {
                sizing: SplitSizing::Px { px, .. },
                ..
            } if (px - 300.0).abs() < f32::EPSILON
        ));
    }

    #[test]
    fn unmatched_placeholders_reap_without_disturbing_the_rest() {
        let session = SessionId::from(1);
        let scripts = script_panes_for(1, 1);

        let (mut window, _id0, id1) = restored_window();
        // notes0 materializes; notes1 never does (renamed/unauthorized).
        assert_eq!(
            window.claim_pending_pane(session, &user_descriptor("notes0"), scripts[0].key),
            Some(false)
        );
        assert!(
            !window.reap_session_placeholders(session),
            "window not emptied"
        );
        window.flush_grid_rebuild();

        assert!(
            !window.layout().contains_tab(id1),
            "the dead placeholder is gone"
        );
        assert!(window.hosts_pane(session, scripts[0].key));
        assert!(window.hosts_pane(session, MAIN_PANE_KEY));
        // Reaping is idempotent.
        assert!(!window.reap_session_placeholders(session));
    }

    #[test]
    fn a_pane_with_no_matching_placeholder_reports_unclaimed() {
        let (mut window, ..) = restored_window();
        let foreign = script_panes_for(1, 1);
        assert_eq!(
            window.claim_pending_pane(
                SessionId::from(1),
                &user_descriptor("someones-else"),
                foreign[0].key
            ),
            None,
            "unknown panes fall through to normal placement"
        );
    }

    #[test]
    fn vacate_retains_geometry_and_adoption_rebinds_in_place() {
        let mut window = test_window();
        let main1 = main_pane(1);
        let script1 = script_panes_for(1, 1)[0];
        host_cluster(&mut window, main1);
        window.place_session_pane(
            SessionId::from(1),
            script1.key,
            PanePlacement::Split {
                reference: MAIN_PANE_KEY,
                direction: SplitDirection::Right,
                size_px: Some(200.0),
            },
        );
        host_cluster(&mut window, main_pane(2));
        window.flush_grid_rebuild();
        let groups_before = window.layout().groups_depth_first().len();

        let descriptors =
            std::collections::HashMap::from([(script1.key, user_descriptor("notes0"))]);
        window.vacate_session(SessionId::from(1), "Arctic", "imm", &descriptors, 0);
        window.flush_grid_rebuild();

        // Geometry retained: every group leaf is still standing, only the
        // bindings are gone.
        assert_eq!(window.layout().groups_depth_first().len(), groups_before);
        assert!(!window.layout().contains_binding(main1));
        assert!(!window.layout().contains_binding(script1));
        assert!(window.hosts_pane(SessionId::from(2), MAIN_PANE_KEY));

        // The next snapshot omits the vacancy — only session 2's main
        // remains describable.
        let clusters = crate::workspace::snapshot::clusters(window.layout(), &mut |tab| {
            tab.binding()
                .map(|_| crate::workspace::snapshot::PaneRecord {
                    slot: 2,
                    identity: crate::workspace::dto::PaneIdentity::Main,
                    hidden: false,
                })
        });
        let described: usize = {
            fn count(node: &crate::workspace::dto::Node) -> usize {
                match node {
                    crate::workspace::dto::Node::Group(group) => group.tabs.len(),
                    crate::workspace::dto::Node::Split(split) => count(&split.a) + count(&split.b),
                }
            }
            clusters.iter().map(|cluster| count(&cluster.root)).sum()
        };
        assert_eq!(described, 1, "snapshots omit vacant slots by omission");

        // Adoption: a fresh session of the same server takes the slot in
        // place — main rebinds onto the retained tab, the script pane waits
        // as this session's placeholder.
        let adopted = SessionId::from(7);
        assert!(window.adopt_vacancy(adopted, "Arctic", "imm"));
        window.flush_grid_rebuild();
        assert_eq!(window.layout().groups_depth_first().len(), groups_before);
        assert!(window.hosts_pane(adopted, MAIN_PANE_KEY));
        let new_script = script_panes_for(7, 1)[0];
        assert_eq!(
            window.claim_pending_pane(adopted, &user_descriptor("notes0"), new_script.key),
            Some(false)
        );
        assert!(window.hosts_pane(adopted, new_script.key));
    }

    #[test]
    fn adoption_prefers_exact_profile_then_lowest_ordinal_and_never_crosses_servers() {
        let mut window = test_window();
        host_cluster(&mut window, main_pane(1));
        host_cluster(&mut window, main_pane(2));
        window.flush_grid_rebuild();
        let empty = std::collections::HashMap::new();
        window.vacate_session(SessionId::from(1), "Arctic", "alpha", &empty, 0);
        window.vacate_session(SessionId::from(2), "Arctic", "beta", &empty, 1);

        // Cross-server: never adopted, even with vacancies free.
        assert!(!window.adopt_vacancy(SessionId::from(10), "Elsewhere", "alpha"));

        // Exact profile beats the lower ordinal.
        assert!(window.adopt_vacancy(SessionId::from(11), "Arctic", "beta"));
        assert!(window.hosts_pane(SessionId::from(11), MAIN_PANE_KEY));

        // The remaining slot goes by ordinal to a same-server open with a
        // profile no vacancy names.
        assert!(window.adopt_vacancy(SessionId::from(12), "Arctic", "gamma"));
        assert!(window.hosts_pane(SessionId::from(12), MAIN_PANE_KEY));

        // Nothing vacant remains.
        assert!(!window.adopt_vacancy(SessionId::from(13), "Arctic", "alpha"));
    }

    #[test]
    fn vacated_scripts_without_a_local_main_close_instead_of_stranding() {
        // The session's main lives in another window; this window hosts
        // only its script pane. Vacating here must not leave an
        // unadoptable placeholder behind.
        let mut window = test_window();
        let script1 = script_panes_for(1, 1)[0];
        host_cluster(&mut window, main_pane(2));
        window.adopt_tab_beside(
            Tab::bound(script1),
            main_pane(2),
            SplitDirection::Right,
            None,
        );
        window.flush_grid_rebuild();

        let descriptors =
            std::collections::HashMap::from([(script1.key, user_descriptor("notes0"))]);
        assert!(
            !window.vacate_session(SessionId::from(1), "Arctic", "imm", &descriptors, 0),
            "another session's main keeps the window populated"
        );
        window.flush_grid_rebuild();
        assert!(!window.layout().contains_binding(script1));
        assert_eq!(
            window.layout().groups_depth_first().len(),
            1,
            "the orphan script leaf collapsed instead of lingering unbound"
        );
        assert!(!window.adopt_vacancy(SessionId::from(9), "Arctic", "imm"));
    }

    #[test]
    fn vacating_a_windows_only_cross_window_script_pane_empties_it() {
        // The window hosts nothing but the closing session's torn-out
        // script pane (its main lives in another window): the pane is
        // doomed, and the vacate must report the window emptied so the
        // daemon can close it instead of leaving a blank shell behind.
        let mut window = test_window();
        let script1 = script_panes_for(1, 1)[0];
        host_cluster(&mut window, script1);
        window.flush_grid_rebuild();

        let descriptors =
            std::collections::HashMap::from([(script1.key, user_descriptor("notes0"))]);
        assert!(window.vacate_session(SessionId::from(1), "Arctic", "imm", &descriptors, 0));
        window.flush_grid_rebuild();
        assert!(window.layout().is_empty());
        assert!(
            !window.adopt_vacancy(SessionId::from(9), "Arctic", "imm"),
            "no vacancy exists: the main was never here"
        );

        // A window hosting the session's main retains the vacancy record —
        // but vacancy tabs are invisible, so it too reports visually empty
        // (the emptied-window rule keeps the last window alive as the
        // connect surface, its vacancy waiting for a later open to adopt).
        let mut homed = test_window();
        host_cluster(&mut homed, main_pane(1));
        homed.flush_grid_rebuild();
        let empty = std::collections::HashMap::new();
        assert!(homed.vacate_session(SessionId::from(1), "Arctic", "imm", &empty, 0));
        homed.flush_grid_rebuild();
        assert!(
            !homed.layout().is_empty(),
            "the vacancy tab is retained in the model"
        );
        assert!(homed.is_visually_empty(), "and invisible to the user");
        assert!(homed.adopt_vacancy(SessionId::from(9), "Arctic", "imm"));
    }

    #[test]
    fn vacancy_placeholders_stay_undescribable_in_snapshots() {
        use crate::workspace::{restore, snapshot};

        // Vacated tabs are runtime-only: they carry no pending record, so
        // the snapshot's placeholder fallback cannot resurrect them —
        // closed stays closed by omission.
        let mut window = test_window();
        let script1 = script_panes_for(1, 1)[0];
        host_cluster(&mut window, main_pane(1));
        place_beside(&mut window, script1, MAIN_PANE_KEY);
        window.flush_grid_rebuild();
        let descriptors =
            std::collections::HashMap::from([(script1.key, user_descriptor("notes0"))]);
        assert!(
            window.vacate_session(SessionId::from(1), "Arctic", "imm", &descriptors, 0),
            "only invisible vacancy tabs remain"
        );
        window.flush_grid_rebuild();

        let mut describe = |tab: &Tab<PaneRef>| -> Option<snapshot::PaneRecord> {
            assert!(
                tab.binding().is_none(),
                "every tab is unbound after the vacate"
            );
            let (_session, key, hidden) = window.pending_pane_for_tab(tab.id())?;
            Some(snapshot::PaneRecord {
                slot: 1,
                identity: restore::identity_from_key(key),
                hidden,
            })
        };
        let clusters = snapshot::clusters(window.layout(), &mut describe);
        assert!(
            clusters.is_empty(),
            "a vacated slot's tabs must vanish from the snapshot"
        );
    }

    #[test]
    fn an_all_vacancy_window_renders_the_empty_view() {
        // Closing the last session leaves the window holding only vacancy
        // tabs: the model keeps them (adoption bookkeeping), but no grid
        // builds in ANY visibility mode, so the view falls through to the
        // no-active-sessions state — the same view a window that never
        // hosted a session shows.
        let mut window = test_window();
        let sessions = SessionStore::new(crate::cloud_account::test_handles());
        let script1 = script_panes_for(1, 1)[0];
        host_cluster(&mut window, main_pane(1));
        place_beside(&mut window, script1, MAIN_PANE_KEY);
        window.flush_grid_rebuild();

        let descriptors =
            std::collections::HashMap::from([(script1.key, user_descriptor("notes0"))]);
        assert!(window.vacate_session(SessionId::from(1), "Arctic", "imm", &descriptors, 0));

        window.set_toolbar_expanded(false);
        window.flush_grid_rebuild();
        assert!(
            !window.layout().is_empty(),
            "vacancy tabs stay in the model"
        );
        assert!(window.is_visually_empty());
        assert!(window.grid.is_none(), "collapsed: no grid builds");

        window.set_toolbar_expanded(true);
        window.flush_grid_rebuild();
        assert!(
            window.grid.is_none(),
            "rearrange mode admits no vacancy either"
        );

        // No vacancy tab offers a strip identity in any mode, and the
        // empty-view branch of the view builds cleanly over the model.
        for tab in window.layout().panes() {
            assert!(window.placeholder_descriptor(tab.id()).is_none());
        }
        let _ = window.view(&sessions, DragViewContext::default());
    }

    #[test]
    fn vacancy_tabs_are_invisible_beside_live_content() {
        // Session 2 stays live while session 1's slot vacates in the same
        // window: the vacancy group drops out of the grid in BOTH
        // visibility modes, its tabs list nowhere, and the invisible record
        // still adopts a later open in place.
        let mut window = test_window();
        host_cluster(&mut window, main_pane(1));
        host_cluster(&mut window, main_pane(2));
        window.flush_grid_rebuild();

        let vacated_main = window
            .tab_of(main_pane(1))
            .expect("session 1's main is hosted");
        let empty = std::collections::HashMap::new();
        assert!(
            !window.vacate_session(SessionId::from(1), "Arctic", "imm", &empty, 0),
            "session 2's main keeps the window populated"
        );

        let grid_leaves =
            |window: &SmudgyWindow| window.grid.as_ref().map_or(0, |grid| grid.panes.len());
        window.set_toolbar_expanded(false);
        window.flush_grid_rebuild();
        assert_eq!(grid_leaves(&window), 1, "collapsed: only the live main");
        window.set_toolbar_expanded(true);
        window.flush_grid_rebuild();
        assert_eq!(grid_leaves(&window), 1, "show-all admits no vacancy either");
        assert!(window.placeholder_descriptor(vacated_main).is_none());

        // Visibility mode never gates adoption: the invisible record still
        // adopts a later open in place.
        assert!(window.adopt_vacancy(SessionId::from(9), "Arctic", "imm"));
        window.flush_grid_rebuild();
        assert_eq!(
            window.tab_of(PaneRef {
                session_id: SessionId::from(9),
                key: MAIN_PANE_KEY
            }),
            Some(vacated_main),
            "the adopted main rebinds the retained tab in place"
        );
        assert_eq!(grid_leaves(&window), 2);
    }

    #[test]
    fn quick_quit_round_trips_script_placeholders_losslessly() {
        use crate::workspace::{dto, restore, snapshot};
        use std::collections::HashMap;

        // A workspace exactly as a prior run persisted it: two sessions,
        // two windows, script panes described by their durable identities
        // alone (a pane that has not materialized has no live definition to
        // read a display name from, so the fixture carries none either).
        let template: dto::Workspace = serde_json::from_str(
            r#"{
                "version": 1,
                "sessions": [
                    {"id": 1, "server": "ArcticMUD", "profile": "Kestrel", "connect": true},
                    {"id": 2, "server": "ArcticMUD", "profile": "Sable", "connect": false}
                ],
                "windows": [
                    {
                        "id": 1,
                        "geometry": {"x": 100.0, "y": 80.0, "width": 1600.0, "height": 900.0, "scale": 1.25},
                        "active_slot": 1,
                        "clusters": [{
                            "weight": 1.0,
                            "root": {"split": {
                                "axis": "vertical",
                                "sizing": {"px": {"px": 300.0, "sized_first": false}},
                                "a": {"group": {"tabs": [{"slot": 1, "id": "main"}], "selected": 0}},
                                "b": {"group": {"tabs": [
                                    {"slot": 1, "id": {"script": {"namespace": "user", "name": "chat"}}},
                                    {"slot": 1, "id": {"script": {"namespace": {"package": {"owner": "gtanger", "name": "mapper"}}, "name": "minimap"}}, "hidden": true}
                                ], "selected": 1}}
                            }}
                        }]
                    },
                    {
                        "id": 2,
                        "geometry": {"x": 1750.0, "y": 120.0, "width": 700.0, "height": 500.0, "scale": 1.0},
                        "maximized": true,
                        "active_slot": 2,
                        "clusters": [{
                            "weight": 1.0,
                            "root": {"split": {
                                "axis": "horizontal",
                                "sizing": {"ratio": 0.65},
                                "a": {"group": {"tabs": [{"slot": 2, "id": "main"}], "selected": 0}},
                                "b": {"group": {"tabs": [{"slot": 1, "id": {"script": {"namespace": "user", "name": "spells"}}}], "selected": 0}}
                            }}
                        }]
                    }
                ]
            }"#,
        )
        .expect("template parses");
        assert_eq!(
            template.clone().sanitized(),
            template,
            "the fixture must be clean, or the round trip proves nothing"
        );

        // Restore: one spawned session per slot, every window installed
        // with its mains bound and its script panes standing as pending
        // placeholders.
        let session_of: HashMap<u64, SessionId> = template
            .sessions
            .iter()
            .zip(0u32..)
            .map(|(slot, ordinal)| (slot.id, SessionId::from(ordinal)))
            .collect();
        let slot_of: HashMap<SessionId, u64> = session_of
            .iter()
            .map(|(&slot, &session)| (session, slot))
            .collect();
        let mut rebuilt = dto::Workspace {
            version: dto::SCHEMA_VERSION,
            sessions: template.sessions.clone(),
            windows: Vec::new(),
        };
        for window_template in &template.windows {
            let plan = restore::plan_window(window_template, &session_of);
            let mut window = test_window();
            window.install_applied_layout(
                GroupLayout::from_blueprint(plan.clusters),
                plan.pending,
                Vec::new(),
                plan.hidden_bound,
                plan.active,
            );
            window.set_toolbar_expanded(false);
            window.flush_grid_rebuild();

            // Quit before anything materialized: describe each tab the way
            // the snapshot builder does — bound tabs (only mains exist yet)
            // directly, placeholders through the pending registry.
            let mut describe = |tab: &Tab<PaneRef>| {
                let Some(slot_ref) = tab.binding().copied() else {
                    let (session, key, hidden) = window.pending_pane_for_tab(tab.id())?;
                    let slot = *slot_of.get(&session)?;
                    return Some(snapshot::PaneRecord {
                        slot,
                        identity: restore::identity_from_key(key),
                        hidden,
                    });
                };
                let slot = *slot_of.get(&slot_ref.session_id)?;
                assert_eq!(
                    slot_ref.key, MAIN_PANE_KEY,
                    "only mains bind before materialization"
                );
                Some(snapshot::PaneRecord {
                    slot,
                    identity: dto::PaneIdentity::Main,
                    hidden: window.pane_hidden(slot_ref),
                })
            };
            let clusters = snapshot::clusters(window.layout(), &mut describe);
            rebuilt.windows.push(dto::Window {
                id: window_template.id,
                geometry: window_template.geometry.clone(),
                maximized: window_template.maximized,
                active_slot: window
                    .active_session_id()
                    .and_then(|active| slot_of.get(&active).copied()),
                clusters,
            });
        }

        // Byte-for-byte: the quick quit writes back exactly what it loaded.
        assert_eq!(
            serde_json::to_string_pretty(&rebuilt).expect("serialize rebuilt"),
            serde_json::to_string_pretty(&template).expect("serialize template"),
            "a quit before materialization must round-trip the file losslessly"
        );
    }

    // ------------------------------------------------------------------
    // All-placeholder groups: a group whose effective selection resolves
    // to no bound pane is a first-class render state, never a panic.
    // ------------------------------------------------------------------

    /// A restored window holding ONLY another window's session's script
    /// pane (a torn-out pane window persisted while its session's main
    /// lived elsewhere — legal per the schema): every tab installs as an
    /// unbound placeholder, `show_all` is vacuously true with zero
    /// bindings, so the grid admits the group and its render resolution
    /// finds no bound pane. The view must produce placeholder bodies and
    /// strip descriptors.
    #[test]
    fn a_restored_script_only_window_renders_placeholders() {
        let mut window = test_window();
        let sessions = SessionStore::new(crate::cloud_account::test_handles());

        let session = SessionId::from(7);
        let tab = Tab::placeholder();
        let tab_id = tab.id();
        let pending = vec![(
            session,
            DescriptorKey::User {
                name: "chat".to_string(),
            },
            PendingPane {
                tab: tab_id,
                hidden: false,
            },
        )];
        let clusters = vec![(
            1.0,
            crate::pane_groups::Blueprint::Group {
                tabs: vec![tab],
                selected: 0,
            },
        )];
        window.install_applied_layout(
            GroupLayout::from_blueprint(clusters),
            pending,
            vec![],
            vec![],
            None,
        );
        window.set_toolbar_expanded(false);
        window.flush_grid_rebuild();

        let group = window.layout().groups_depth_first()[0];
        assert!(
            window.rendered_slot(group).is_none(),
            "the group resolves to no bound pane"
        );
        // The strip identifies the placeholder from its pending record.
        assert_eq!(
            window
                .placeholder_descriptor(tab_id)
                .map(|descriptor| descriptor.label),
            Some("chat".to_string())
        );
        // Building the view drives the grid closure for every slot — an
        // unbound group must resolve to a placeholder body, never panic.
        let _ = window.view(&sessions, DragViewContext::default());
    }

    /// A user-initiated layout apply that spawned a missing session
    /// realizes that session's not-yet-materialized panes as placeholder
    /// tabs. A group made only of them flows through the same placeholder
    /// rendering as a last-session restore.
    #[test]
    fn an_applied_all_placeholder_group_renders_placeholders() {
        use crate::workspace::apply::{
            PlanNode, PlannedTab, PlannedWindow, WindowTarget, realize_window,
        };
        use crate::workspace::dto;

        let session = SessionId::from(9);
        let planned = PlannedWindow {
            target: WindowTarget::New {
                geometry: dto::Geometry {
                    x: 0.0,
                    y: 0.0,
                    width: 800.0,
                    height: 600.0,
                    scale: 1.0,
                },
                maximized: false,
            },
            clusters: vec![(
                1.0,
                PlanNode::Group {
                    tabs: vec![PlannedTab::Pending {
                        session,
                        descriptor: DescriptorKey::Package {
                            owner: "local".to_string(),
                            package: "arctic-mapper".to_string(),
                            name: "map".to_string(),
                        },
                        hidden: false,
                    }],
                    selected: 0,
                },
            )],
            active: None,
        };
        let realized = realize_window(&planned, &[]);

        let mut window = test_window();
        let sessions = SessionStore::new(crate::cloud_account::test_handles());
        window.install_applied_layout(
            GroupLayout::from_blueprint(realized.clusters),
            realized.pending,
            vec![],
            realized.hidden,
            realized.active,
        );
        window.flush_grid_rebuild();

        let group = window.layout().groups_depth_first()[0];
        assert!(window.rendered_slot(group).is_none());
        let tab_id = window.layout().tabs(group).expect("group exists")[0].id();
        assert_eq!(
            window
                .placeholder_descriptor(tab_id)
                .map(|descriptor| descriptor.label),
            Some("map".to_string())
        );
        let _ = window.view(&sessions, DragViewContext::default());
    }
}
