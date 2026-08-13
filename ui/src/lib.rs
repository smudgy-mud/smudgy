#![allow(clippy::pedantic)]
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use crate::session_store::BindTarget;
use chrono::{DateTime, Utc};
use iced::widget::{center, text};
use iced::window;
use iced::window::settings::PlatformSpecific;
use iced::{Point, Rectangle, Size, Subscription, Task};
use smudgy_cloud::cloud_api::{AreaPref, CloudApiClient};
use smudgy_cloud::{AreaId, AtlasId, CloudError, MapStorage, Mapper};
use smudgy_core::models::map_scopes::{MapScopes, ScopeState};
use smudgy_core::models::settings::{MapAreaPref, Settings};
use smudgy_core::session::runtime::pane::{
    MAIN_PANE_KEY, PaneKey, PanePlacement, SplitDirection, TabPosition,
};
use smudgy_core::session::ui_command::{
    PaneCommand, UiCommand, UiCommandEnvelope, UiCommandReceiver,
};
use smudgy_core::session::{SessionEvent, SessionId, TaggedSessionEvent};

// Core session imports
use windows::automations_window::{AutomationsWindow, Event as AutomationsWindowEvent};
use windows::settings_window::{self, Event as SettingsWindowEvent, SettingsWindow};
use windows::smudgy_window::SmudgyWindow;

mod assets;
mod cloud_account;
mod discord_presence;
mod i18n;
mod images;
mod pane_drag;
mod pane_groups;
pub mod prefs;
mod session_store;
pub mod terminal_buffer;
mod update;
mod widgets;
mod win_rm;
pub mod workspace;

pub use smudgy_theme::{self as theme, Element, Theme};

mod components;

mod windows {
    pub mod automations_window;
    pub mod map_editor_window;
    pub mod settings_window;
    pub mod smudgy_window;
}

mod keymap;

use windows::smudgy_window::{Event as SmudgyWindowEvent, PaneRef};

/// Title for the main smudgy window, marked per build channel so a non-release
/// build is never mistaken for the published release. A dev/pre-release build
/// (which talks to the dev API) is tagged "DEV BUILD"; a release candidate —
/// which behaves like a release but ships ahead of it — is tagged with its
/// exact version so a tester can see which RC they are running. The channel
/// decision lives in `core` so the title and the API/data-dir defaults can't
/// drift. A clean release gets the bare title.
fn main_window_title() -> String {
    match smudgy_core::models::settings::build_channel() {
        smudgy_core::models::settings::BuildChannel::Dev => {
            i18n::t!("window-main-development")
        }
        smudgy_core::models::settings::BuildChannel::ReleaseCandidate => {
            i18n::t!(
                "window-main-release-candidate",
                "version" => env!("CARGO_PKG_VERSION")
            )
        }
        smudgy_core::models::settings::BuildChannel::Release => "smudgy".to_string(),
    }
}

use crate::cloud_account::CloudAccount;
use crate::discord_presence::DiscordPresence;
use crate::session_store::SessionStore;
use crate::windows::map_editor_window::{self, MapEditorWindow, SharedClipboard};

extern crate log;

pub type Renderer = iced::Renderer;

/// Where an out-of-date client sends the user to upgrade — opened in the system
/// browser when the user clicks the "out of date" banner link, and shown
/// verbatim in that banner's label (single-sourced here so the two stay in sync).
pub(crate) const DOWNLOAD_URL: &str = "https://www.smudgy.org/download";

// Main application state
struct Smudgy {
    account: CloudAccount,
    /// Discord Rich Presence ("Playing smudgy — on <server>"), mirrored from
    /// `settings.discord_rich_presence`. Re-derived from the session store on
    /// every connect/disconnect/close; the controller change-gates, so the
    /// frequent recomputes are free.
    discord: DiscordPresence,
    /// All live sessions, window-independent: windows' grids hold pane
    /// references into this store, and session events route here directly.
    sessions: SessionStore,
    /// Single consumer for imperative UI mutations issued by every script
    /// runtime. Its channel receive order is the daemon's canonical order.
    ui_commands: UiCommandReceiver,
    /// Commands whose pane dependencies are not hosted yet. A later Open
    /// retries them in bus order; retirement cancels them permanently.
    pending_pane_commands: VecDeque<PaneCommand>,
    retired_panes: HashSet<PaneRef>,
    /// Ordered close events that overtook their command on iced's independent
    /// subscriptions. The command completes display-state retirement.
    pending_ordered_pane_closes: HashSet<PaneRef>,
    last_ui_command_seq: HashMap<SessionId, u64>,
    smudgy_windows: BTreeMap<window::Id, SmudgyWindow>,
    automations_windows: BTreeMap<window::Id, AutomationsWindow>,
    map_editor_windows: BTreeMap<window::Id, MapEditorWindow>,
    settings_windows: BTreeMap<window::Id, SettingsWindow>,
    /// Areas the user excludes from room identification, mirrored from
    /// settings.json. The authoritative copy for fan-out to live mappers.
    /// This is the **derived** effective set — exactly the `disabled == true`
    /// entries of [`Self::area_prefs`] — kept in sync with it.
    disabled_map_areas: HashSet<AreaId>,
    /// Timestamped per-area enable/disable preferences: the authoritative
    /// local mirror of the cloud `/me/area-prefs` rows, and the offline
    /// cache + last-write-wins basis for cross-device sync. A present
    /// entry is an explicit preference; an absent area defaults to enabled.
    area_prefs: HashMap<AreaId, MapAreaPref>,
    /// Areas whose pref push came back [`CloudError::NotFoundOrNoAccess`] this
    /// launch: local-tier maps and lost grants, which the server will keep
    /// refusing. The reconcile skips re-pushing these — without the parking,
    /// the 90s tick re-attempted the same doomed PUTs for the life of the
    /// process (measured at 37% of prod API traffic). An explicit user toggle
    /// or a fresh sign-in clears an area's parking, so newly-granted access
    /// syncs without waiting for a relaunch.
    area_prefs_push_parked: HashSet<AreaId>,
    /// The authoritative per-user cloud-map scope associations (atlas/area →
    /// server entries). Owned here, persisted to `map-scopes.json`, and fanned
    /// out to every live session mapper and open map editor window whenever an
    /// association changes.
    map_scopes: MapScopes,
    /// One app-global clipboard shared by every map editor window, so the
    /// two-window merge workflow can copy/paste between them.
    map_editor_clipboard: SharedClipboard,
    /// Window origins/sizes/scales/cursors + focus MRU + keyboard modifier
    /// state, observed from the event stream. The drag layer reconstructs
    /// screen-space geometry from this (iced has no direct "window under
    /// this screen point" query).
    window_tracker: pane_drag::WindowTracker,
    /// The tab drag in flight, if any — the drag controller's single owner
    /// state. Recorded when a tab press crosses the deadband; consumed by
    /// exactly one terminal (release, Escape, capture loss) or an abort
    /// (pane/session/source-window death mid-drag). Windows derive their
    /// view of it per frame; none keeps a drag flag of its own.
    tab_drag: Option<pane_drag::TabDrag>,
    /// A tab press below the deadband, if any. The daemon carries the
    /// gesture from press to terminal: tracked motion past the deadband
    /// promotes this to [`Self::tab_drag`] even if the press surface's
    /// widget state was erased by an async subtree rebuild mid-gesture (the
    /// widget's own deadband crossing is the fast path). Cleared by the raw
    /// release (a plain click — selection is the widget's fast path) and by
    /// every cancel that clears the drag.
    tab_press: Option<pane_drag::PendingPress>,
    /// Smudgy windows we have asked to close but whose async `CloseWindow`
    /// event has not yet landed. They linger in `smudgy_windows` in the
    /// meantime, so the empty-window sweep must not count them as "remaining"
    /// — otherwise two windows emptied in separate updates can each close and
    /// leave zero windows, exiting the app against the keep-one-alive rule.
    closing_windows: HashSet<window::Id>,
    /// The live-workspace mirror's scheduling state and runtime↔durable
    /// identity maps (stable window/slot ids, polled geometry, snapshot
    /// generations). Windows and sessions raise cheap dirty flags; the
    /// daemon sweeps them here once per update and the debounce/checkpoint
    /// ticks turn the flags into the active server's `last-session.json`
    /// write.
    workspace: workspace::autosave::Mirror,
    /// Restoration bookkeeping: eyeball replays owed to runtimes that are
    /// not ready yet, per-session readiness, and the vacancy ordinal well.
    restore: workspace::restore::RestoreState,
    /// The background writer for script-initiated layout saves: capture and
    /// serialization stay on the update thread (same-cycle consistency),
    /// while the fsync-bearing write coalesces per layout name off-thread.
    /// Dropping it at exit flushes what is still pending, best-effort.
    layout_saver: workspace::layouts::DebouncedSaver,
}

#[derive(Debug, Clone)]
enum Message {
    CloseWindow(window::Id),
    Account(cloud_account::Message),
    /// ~24h cloud-session keep-alive: slide the session's idle deadline so a
    /// long-running, actively-used client is never logged out for inactivity
    /// (launch covers the session-start case).
    SessionRefreshTick,
    /// Long-interval re-check for a newer client version (launch covers the
    /// startup case). Gated on `auto_check_for_updates`; unauthenticated, so it
    /// runs signed in or out.
    UpdateCheckTick,
    SmudgyWindowMessage(window::Id, windows::smudgy_window::Message),
    /// An event from a session's runtime stream, routed straight to the
    /// session store (whatever window hosts the session's pane repaints from
    /// the shared state).
    SessionEvent(TaggedSessionEvent),
    UiCommand(UiCommandEnvelope),
    /// A session-level action carrying no window context: task continuations
    /// from store-routed updates and daemon fan-outs (settings changes,
    /// script reloads, widget wake-ups).
    SessionAction(SessionId, session_store::Message),
    NewSmudgyWindow(window::Id),
    /// The raw HWND of a freshly opened main window, delivered so the Restart
    /// Manager shutdown hook can be installed on it (Windows only; the hook is a
    /// no-op elsewhere).
    HookWindowForShutdown(u64),
    // Handled in `update()` (opens a window -> `NewSmudgyWindow`), mirroring
    // the other `Create*Window` variants; no sender currently emits it.
    #[allow(dead_code)]
    CreateSmudgyWindow,
    AutomationsWindowMessage(window::Id, windows::automations_window::Message),
    NewAutomationsWindow {
        id: window::Id,
        server_name: Arc<String>,
        session_id: smudgy_core::session::SessionId,
    },
    CreateAutomationsWindow {
        server_name: Arc<String>,
        session_id: smudgy_core::session::SessionId,
    },
    MapEditorWindowMessage(window::Id, windows::map_editor_window::Message),
    NewMapEditorWindow {
        id: window::Id,
        mapper: Mapper,
        server_name: Arc<String>,
    },
    CreateMapEditorWindow {
        mapper: Mapper,
        server_name: Arc<String>,
    },
    SettingsWindowMessage(window::Id, windows::settings_window::Message),
    NewSettingsWindow(window::Id),
    CreateSettingsWindow,
    SetMapperCurrentLocation(AreaId, Option<i32>),
    /// Periodic + login/startup trigger to pull `/me/area-prefs` and reconcile
    /// it against the local set (cross-device sync).
    AreaPrefsReconcileTick,
    /// `GET /me/area-prefs` landed: merge (last-write-wins) into the local set.
    AreaPrefsFetched(Result<Vec<AreaPref>, CloudError>),
    /// A `PUT /me/area-prefs/{id}` push completed; adopt the server-stamped
    /// `updated_at` (or, on a uniform 404 / error, leave the local pref as-is).
    AreaPrefPushed {
        area_id: AreaId,
        result: Result<AreaPref, CloudError>,
    },
    /// A window-geometry observation (moved/resized/rescaled/focused/cursor/
    /// modifiers) for the tracker feeding the drag layer.
    WindowTracking(window::Id, pane_drag::TrackEvent),
    /// A drag terminal from the drag-gated subscription: the raw left-button
    /// release (the authoritative terminal — the tab widget's own release is
    /// a fast path only) or Escape (cancel). Subscribed only while a drag is
    /// live; a stray arrival with no drag in flight is a no-op.
    PaneDragTerminal(window::Id, pane_drag::DragTerminal),
    /// The trailing debounce flush of one session's pane-size feed: send the
    /// settled pending sizes to the runtime (`docs/panes.md` placement
    /// read-back). Scheduled by [`report_pane_sizes`], at most one in flight
    /// per session.
    FlushPaneSizes(SessionId),
    /// One tick of the workspace autosave's trailing debounce (subscribed
    /// only while the mirror is dirty): a tick with no churn since the
    /// previous one snapshots the workspace.
    WorkspaceDebounceTick,
    /// One tick of the workspace autosave's max-interval checkpoint. Fires
    /// the unconditional geometry poll — the poll, not event tracking, is
    /// the geometry dirty signal — and bounds crash loss and write volume
    /// under sustained churn.
    WorkspaceCheckpointTick,
    /// One asynchronous geometry answer for the workspace mirror. `poll`
    /// ties the answer to the poll that asked (stale answers are ignored);
    /// `None` is the open-time seed.
    WorkspaceGeometry {
        poll: Option<u64>,
        window: window::Id,
        sample: workspace::autosave::GeometrySample,
    },
    /// The quit path's awaited write completed (or the writer is
    /// unavailable): the deferred `iced::exit()` may run.
    WorkspaceQuitFlushed,
}

/// The application id, matching the Linux desktop-entry / Flatpak app id
/// (`org.smudgy.Smudgy`). On Linux it must be set as each window's
/// `application_id` so the running window associates with
/// `org.smudgy.Smudgy.desktop` — iced maps it to both the Wayland `app_id` and
/// the X11 `WM_CLASS`, which is what a compositor/WM uses to pick the taskbar
/// icon. Without it the window shows a generic icon (the app-menu entry still
/// works from the .desktop file, but the live window would not).
#[cfg(target_os = "linux")]
pub(crate) const LINUX_APP_ID: &str = "org.smudgy.Smudgy";

/// Whether main windows paint their own rounded window frame (Linux only).
///
/// True on Wayland sessions, where client-side decoration is the platform
/// convention (GTK apps round their own corners) and an alpha surface is
/// composited correctly. X11 stays sharp: without a compositor the pixels
/// outside the corner arcs would render black, and sharp rectangles are what
/// tiling setups expect anyway. `SMUDGY_SQUARE_CORNERS=1` opts out (e.g. on
/// tiled Wayland compositors, where rounding fights the layout).
#[cfg(target_os = "linux")]
pub(crate) fn client_rounded_frame() -> bool {
    static ACTIVE: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
        // Mirror winit's backend choice exactly: it goes Wayland when either
        // variable is set non-empty (an empty `WAYLAND_DISPLAY=` still lands
        // on X11, which must keep the opaque sharp look).
        let non_empty = |name: &str| std::env::var_os(name).is_some_and(|v| !v.is_empty());
        // wgpu's GL backend advertises only Opaque composite alpha, so the
        // transparent corners would render black there. The automatic
        // GL fallback isn't visible from here, but an explicit override is.
        let forced_gl = std::env::var("WGPU_BACKEND")
            .is_ok_and(|v| matches!(v.to_ascii_lowercase().as_str(), "gl" | "gles" | "opengl"));
        (non_empty("WAYLAND_DISPLAY") || non_empty("WAYLAND_SOCKET"))
            && !forced_gl
            && !std::env::var_os("SMUDGY_SQUARE_CORNERS").is_some_and(|v| v == "1" || v == "true")
    });
    *ACTIVE
}

/// Windows gets its frame from DWM and macOS keeps its native frame, so only
/// Linux ever draws one client-side.
#[cfg(not(target_os = "linux"))]
pub(crate) fn client_rounded_frame() -> bool {
    false
}

/// Settings for main smudgy windows. On Windows and Linux the window is
/// borderless, with the toolbar acting as the titlebar (drag area + window
/// controls) and resize grips at the edges; Windows keeps DWM's rounded
/// corners and hairline border, Linux Wayland paints its own (see
/// [`client_rounded_frame`]). On macOS the window keeps its native frame —
/// title hidden, titlebar transparent, full-size content view — so the system
/// supplies the rounded corners, hairline border, edge resizing, and traffic
/// lights, and the toolbar draws in the titlebar region.
fn smudgy_window_settings() -> window::Settings {
    window::Settings {
        decorations: cfg!(target_os = "macos"),
        min_size: Some(Size::new(640.0, 400.0)),
        exit_on_close_request: true,
        // Alpha surface so the pixels outside the self-drawn frame's corner
        // arcs stay empty (Wayland only; the X11 surface stays opaque).
        #[cfg(target_os = "linux")]
        transparent: client_rounded_frame(),
        // Keep the OS drop shadow (and the window-frame feel it provides)
        // even without native decorations.
        #[cfg(target_os = "windows")]
        platform_specific: PlatformSpecific {
            undecorated_shadow: true,
            ..Default::default()
        },
        // Associate the window with org.smudgy.Smudgy.desktop (Wayland app_id /
        // X11 WM_CLASS) so the compositor shows the app icon.
        #[cfg(target_os = "linux")]
        platform_specific: PlatformSpecific {
            application_id: LINUX_APP_ID.to_string(),
            ..Default::default()
        },
        #[cfg(target_os = "macos")]
        platform_specific: PlatformSpecific {
            title_hidden: true,
            titlebar_transparent: true,
            fullsize_content_view: true,
        },
        ..Default::default()
    }
}

/// `window::Settings` for a secondary (tool) window with the given minimum size.
/// Carries the Linux `application_id` so every window — not just the main one —
/// groups under `org.smudgy.Smudgy.desktop`.
fn secondary_window_settings(min_size: Size) -> window::Settings {
    window::Settings {
        min_size: Some(min_size),
        #[cfg(target_os = "linux")]
        platform_specific: PlatformSpecific {
            application_id: LINUX_APP_ID.to_string(),
            ..Default::default()
        },
        ..Default::default()
    }
}

fn init() -> (Smudgy, Task<Message>) {
    // Seed the hot prefs snapshot before any window renders, and load the
    // per-area enable/disable preferences (migrating a legacy disabled-only
    // file) for fan-out to mappers and cross-device reconcile. `load_settings`
    // also folds in the installer's update-check seed, which overrides the
    // persisted auto-check value while present.
    let settings = smudgy_core::models::settings::load_settings();
    i18n::activate(&settings.locale);
    prefs::apply(&settings);
    // Startup image-cache housekeeping (plan D10): drop namespaces of servers that no
    // longer exist and trim the disk cache to `image_cache_max_mb` (LRU by fetch time).
    // Fire-and-forget on a plain thread — pure disk I/O, nothing awaits it.
    {
        let max_mb = settings.image_cache_max_mb;
        std::thread::spawn(move || match smudgy_core::models::server::list_servers() {
            Ok(list) => {
                let servers: Vec<String> = list.into_iter().map(|s| s.name).collect();
                images::startup_image_cache_sweep(&servers, max_mb);
            }
            // A keep-list we can't trust must not drive a destructive sweep — an
            // empty-on-error list would read as "no servers exist, drop every namespace".
            Err(err) => log::warn!("skipping image-cache sweep; could not list servers: {err}"),
        });
    }
    let area_prefs = load_area_prefs(&settings);
    let disabled_map_areas = disabled_set_from_prefs(&area_prefs);
    // Per-server cloud-map scope associations, applied to each session's mapper
    // as it opens and re-pushed here whenever the editor changes an association.
    let map_scopes = MapScopes::load();

    // Split a pre-0.4.1 global local-map store into the per-server stores and
    // delete it, before any session or map editor opens a LocalBackend.
    session_store::migrate_legacy_global_local_maps();

    let (account, account_task) = CloudAccount::new();
    // If we resumed a signed-in session, reconcile against the cloud at once.
    let reconcile_task = if account.snapshot().signed_in {
        reconcile_area_prefs_task(&account.handles().client)
    } else {
        Task::none()
    };

    // The launch-time update check. Unauthenticated, so it runs signed in or
    // out; the setting is the master switch, so a cloud-averse user who turned
    // it off makes no smudgy.org contact at all.
    let update_check_task = if settings.auto_check_for_updates {
        account.check_for_updates().map(Message::Account)
    } else {
        Task::none()
    };

    let (ui_command_bus, ui_commands) = smudgy_core::session::ui_command::channel();
    let sessions = SessionStore::with_ui_commands(account.handles(), ui_command_bus);
    let discord = DiscordPresence::new(settings.discord_rich_presence);

    // The workspace mirror is disabled entirely under the scripted-matrix
    // QA hook (debug builds only): the harness drives a synthetic
    // arrangement against the real data directory, which must neither be
    // restored from nor written to. Release builds have no hook — the
    // mirror is always on.
    #[cfg(debug_assertions)]
    let workspace_enabled = spike_autosession_count() == 0;
    #[cfg(not(debug_assertions))]
    let workspace_enabled = true;

    // The workspace writer (the single-writer worker plus the snapshot cell
    // the WM_ENDSESSION hook flushes from) exists for the whole run; it
    // drains to the per-server last-session files as snapshots settle.
    if workspace_enabled {
        workspace::writer::init_global();
    }

    // Startup is always the clean no-active-sessions view: nothing restores
    // at launch. Each server's last arrangement waits in its own
    // last-session snapshot, offered per server on the connect surface.
    let workspace_mirror = workspace::autosave::Mirror::default();
    let restore_state = workspace::restore::RestoreState::default();
    let smudgy_windows: BTreeMap<window::Id, SmudgyWindow> = BTreeMap::new();
    let mut open_tasks: Vec<Task<Message>> = Vec::new();
    let (_id, open) = window::open(smudgy_window_settings());
    open_tasks.push(open.map(Message::NewSmudgyWindow));

    (
        Smudgy {
            account,
            discord,
            sessions,
            ui_commands,
            pending_pane_commands: VecDeque::new(),
            retired_panes: HashSet::new(),
            pending_ordered_pane_closes: HashSet::new(),
            last_ui_command_seq: HashMap::new(),
            smudgy_windows,
            automations_windows: BTreeMap::new(),
            map_editor_windows: BTreeMap::new(),
            settings_windows: BTreeMap::new(),
            disabled_map_areas,
            area_prefs,
            area_prefs_push_parked: HashSet::new(),
            map_scopes,
            map_editor_clipboard: Arc::new(arc_swap::ArcSwap::from_pointee(
                map_editor_window::commands::EntityClipboard::default(),
            )),
            window_tracker: pane_drag::WindowTracker::default(),
            tab_drag: None,
            tab_press: None,
            closing_windows: HashSet::new(),
            workspace: workspace_mirror,
            restore: restore_state,
            layout_saver: workspace::layouts::DebouncedSaver::new(),
        },
        Task::batch([
            Task::batch(open_tasks),
            account_task.map(Message::Account),
            reconcile_task,
            update_check_task,
        ]),
    )
}

/// Applies the `--data-dir <path>` and `--keyring-user <name>` launch flags
/// (each accepts both `--flag value` and `--flag=value` forms) before any data
/// access. Together they let a second instance run side by side against a
/// different account: `--data-dir` isolates all on-disk state (accounts,
/// profiles, maps, settings, logs) while `--keyring-user` points the cloud
/// session token at a separate OS-keyring slot so the two logins don't collide.
///
/// Must run before `smudgy_core::init`, which opens the log file under the
/// (possibly overridden) home directory.
fn apply_launch_overrides() {
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if let Some(value) = flag_value("--data-dir", &arg, &mut args) {
            smudgy_core::set_smudgy_home(value);
        } else if let Some(value) = flag_value("--keyring-user", &arg, &mut args) {
            smudgy_core::models::auth::set_keyring_user(value);
        }
    }
}

/// Extracts the value for the flag `name` from `arg`: the inline `--name=value`
/// form, or the next argument for the `--name value` form (consumed from
/// `rest`). Returns `None` when `arg` is not this flag. Exits with a usage error
/// when the flag is given without a value.
fn flag_value(name: &str, arg: &str, rest: &mut impl Iterator<Item = String>) -> Option<String> {
    if let Some(value) = arg.strip_prefix(name).and_then(|s| s.strip_prefix('=')) {
        return Some(value.to_string());
    }
    if arg == name {
        if let Some(value) = rest.next() {
            return Some(value);
        }
        eprintln!("error: {name} requires a value");
        std::process::exit(2);
    }
    None
}

/// Runs the smudgy application: applies the launch-flag overrides, initializes
/// `smudgy_core` (logging, data dir), and drives the iced daemon until the last
/// window closes; joins the session and connection-worker threads before
/// returning. The `smudgy` binary's `main` is a thin wrapper around this.
pub fn run() -> anyhow::Result<()> {
    apply_launch_overrides();
    smudgy_core::init();
    // Resolve the persisted/system locale before configuring the daemon's
    // first window. `init` reloads the same settings for the live model.
    let startup_settings = smudgy_core::models::settings::load_settings();
    i18n::activate(&startup_settings.locale);

    iced::daemon(init, update, view)
        .theme(|smudgy: &Smudgy, window_id| {
            if smudgy.smudgy_windows.contains_key(&window_id) {
                // Palette-aware: re-evaluated per frame, so theme changes in
                // the Preferences tab apply live.
                prefs::app_theme()
            } else {
                smudgy_theme::secondary()
            }
        })
        .style(|_smudgy, theme| {
            let mut style = iced::theme::Base::base(theme);
            // The self-drawn rounded frame needs an alpha surface: the
            // runtime's clear color goes fully transparent and every window
            // paints its own background instead (main windows via the frame
            // container, secondary windows via an opaque wrapper — see
            // `view`), leaving the pixels outside the corner arcs empty.
            if client_rounded_frame() {
                style.background_color = iced::Color::TRANSPARENT;
            }
            style
        })
        .subscription(subscription)
        .font(assets::fonts::GEIST_VF_BYTES)
        .font(assets::fonts::GEIST_ITALIC_VF_BYTES)
        .font(assets::fonts::GEIST_MONO_VF_BYTES)
        .font(assets::fonts::GEIST_MONO_ITALIC_VF_BYTES)
        .font(assets::fonts::BOOTSTRAP_ICONS_BYTES)
        .font(assets::fonts::MONASPACE_ARGON_BYTES)
        .font(assets::fonts::MONASPACE_KRYPTON_BYTES)
        .font(assets::fonts::MONASPACE_NEON_BYTES)
        .font(assets::fonts::MONASPACE_RADON_BYTES)
        .font(assets::fonts::MONASPACE_XENON_BYTES)
        .font(assets::fonts::COURIER_PRIME_BYTES)
        .font(assets::fonts::COURIER_PRIME_BOLD_BYTES)
        .font(assets::fonts::COURIER_PRIME_ITALIC_BYTES)
        .font(assets::fonts::COURIER_PRIME_BOLD_ITALIC_BYTES)
        .font(assets::fonts::DEPARTURE_MONO_BYTES)
        .font(assets::fonts::FIRA_MONO_BYTES)
        .font(assets::fonts::FIRA_MONO_MEDIUM_BYTES)
        .font(assets::fonts::FIRA_MONO_BOLD_BYTES)
        .font(assets::fonts::LILEX_BYTES)
        .font(assets::fonts::VT323_BYTES)
        .font(assets::fonts::FIXEDSYS_EX_BYTES)
        .default_font(assets::fonts::GEIST_VF)
        .title(|smudgy: &Smudgy, window_id: window::Id| {
            if let Some(window) = smudgy.automations_windows.get(&window_id) {
                i18n::t!("window-automations", "server" => window.server_name())
            } else if let Some(window) = smudgy.map_editor_windows.get(&window_id) {
                window.title()
            } else {
                main_window_title()
            }
        })
        .run()?;

    log::info!("Application closing");

    smudgy_core::session::connection::shutdown_io_runtime();
    smudgy_core::session::runtime::join_runtime_threads();

    Ok(())
}

fn subscription(smudgy: &Smudgy) -> Subscription<Message> {
    let mut subs = vec![
        Subscription::run_with(smudgy.ui_commands.clone(), Clone::clone).map(Message::UiCommand),
        // Session runtimes: one event stream per live session, owned at the
        // daemon because sessions are window-independent.
        Subscription::batch(
            smudgy
                .sessions
                .iter()
                .map(|(_, session)| session.session_subscription()),
        )
        .map(Message::SessionEvent),
        // Script-widget wake-ups: a repaint poke whenever a session's widget
        // tree mutates off-thread.
        Subscription::batch(
            smudgy
                .sessions
                .iter()
                .map(|(_, session)| session.jsx_subscription()),
        )
        .map(|id| Message::SessionAction(id, session_store::Message::None)),
        Subscription::batch(
            smudgy
                .smudgy_windows
                .iter()
                .map(|(id, window)| window.subscription().with(*id)),
        )
        .map(|(id, msg)| Message::SmudgyWindowMessage(id, msg)),
        Subscription::batch(
            smudgy
                .map_editor_windows
                .iter()
                .map(|(id, window)| window.subscription().with(*id)),
        )
        .map(|(id, msg)| Message::MapEditorWindowMessage(id, msg)),
        Subscription::batch(
            smudgy
                .automations_windows
                .iter()
                .map(|(id, window)| window.subscription().with(*id)),
        )
        .map(|(id, msg)| Message::AutomationsWindowMessage(id, msg)),
        window::close_events().map(Message::CloseWindow),
        // Window geometry + cursor tracking for pane drags. `listen_with`
        // (not `listen`): captured events must still reach the tracker. The
        // full filter maps every window move and mouse motion to a message —
        // and every message rebuilds and repaints all windows — so it runs
        // only while a drag is in flight; idle windows use the rare-events
        // filter.
        if smudgy.tab_drag.is_some() || smudgy.tab_press.is_some() {
            iced::event::listen_with(window_tracking_event)
        } else {
            iced::event::listen_with(window_tracking_idle_event)
        },
    ];

    if smudgy.tab_drag.is_some() || smudgy.tab_press.is_some() {
        // Drag termination authority: while a drag is live, the raw
        // left-button release is the authoritative terminal and Escape is
        // the cancel — regardless of capture status, so no focused widget
        // can strand a drag. Below the deadband no drag record exists yet,
        // and Escape deliberately does nothing.
        subs.push(iced::event::listen_with(|event, _status, window_id| {
            pane_drag::drag_terminal_event(&event)
                .map(|terminal| Message::PaneDragTerminal(window_id, terminal))
        }));
    }

    // While signed in, poll /me/area-prefs periodically so cross-device
    // changes and prefs for newly-shared areas reconcile in (login covers the
    // session-start case; this covers "after a /sync row-set change").
    if smudgy.account.snapshot().signed_in {
        subs.push(
            iced::time::every(Duration::from_secs(90)).map(|_| Message::AreaPrefsReconcileTick),
        );
        // Keep the cloud session alive: slide its 365-day idle deadline roughly
        // once a day so a continuously-running client never lapses (the first
        // tick lands at +24h; launch already refreshed via `CloudAccount::new`).
        subs.push(
            iced::time::every(Duration::from_secs(86_400)).map(|_| Message::SessionRefreshTick),
        );
    }

    // Re-check for a newer client version every few hours so a long-running
    // client eventually notices a release (launch covers the startup case).
    // Master-switched on `auto_check_for_updates` and independent of sign-in.
    if smudgy.account.auto_check_for_updates() {
        subs.push(iced::time::every(Duration::from_secs(21_600)).map(|_| Message::UpdateCheckTick));
    }

    // Workspace autosave. The checkpoint runs whenever windows exist: its
    // unconditional geometry poll is the only thing that notices a window
    // move while idle. The debounce tick exists only while the mirror is
    // dirty, so an idle workspace costs no timer at all. Both stop once the
    // quit flush latches the schedule shut.
    if !smudgy.smudgy_windows.is_empty() && !smudgy.workspace.schedule.is_shutting_down() {
        subs.push(
            iced::time::every(workspace::autosave::CHECKPOINT_INTERVAL)
                .map(|_| Message::WorkspaceCheckpointTick),
        );
        if smudgy.workspace.schedule.is_dirty() {
            subs.push(
                iced::time::every(workspace::autosave::DEBOUNCE_TICK)
                    .map(|_| Message::WorkspaceDebounceTick),
            );
        }
    }

    Subscription::batch(subs)
}

/// `event::listen_with` filter feeding the window tracker while a pane drag
/// is in flight. Runs for every window (map editors and settings included —
/// drop-target membership is filtered where the tracker is read, not here).
fn window_tracking_event(
    event: iced::Event,
    status: iced::event::Status,
    window_id: window::Id,
) -> Option<Message> {
    spike_log_raw_event(&event, status, window_id);
    pane_drag::track_event(&event).map(|track| Message::WindowTracking(window_id, track))
}

/// The no-drag counterpart of [`window_tracking_event`]: tracks only the
/// rare geometry facts, so window moves and mouse motion cost nothing. No
/// forensics either — raw-event logging is gesture-scoped, and this filter
/// runs exactly while no gesture is armed.
fn window_tracking_idle_event(
    event: iced::Event,
    _status: iced::event::Status,
    window_id: window::Id,
) -> Option<Message> {
    pane_drag::track_event_idle(&event).map(|track| Message::WindowTracking(window_id, track))
}

/// QA forensics (debug builds only): logs the low-frequency input events
/// exactly as the daemon subscription sees them — every `MouseInput`
/// press/release plus the cursor enter/leave and focus transitions, tagged
/// with the window that winit surfaced them on. If a release reaches winit
/// for ANY window of this process, it appears here; if it never appears,
/// Windows never delivered the `WM_*BUTTONUP` to this thread at all. Called
/// only from the gesture-gated tracking filter, so idle play (clicks, focus
/// churn) logs nothing; the scripted drag matrix (`bin/drag-matrix.ps1`)
/// asserts against gesture-time lines only.
/// Release counterpart of the debug forensics logger: an empty inline body,
/// so the tracking filter keeps one shape in both profiles and the release
/// build carries no logging.
#[cfg(not(debug_assertions))]
fn spike_log_raw_event(_: &iced::Event, _: iced::event::Status, _: window::Id) {}

#[cfg(debug_assertions)]
fn spike_log_raw_event(event: &iced::Event, status: iced::event::Status, window_id: window::Id) {
    use iced::mouse;
    match event {
        iced::Event::Mouse(mouse::Event::ButtonPressed(button)) => {
            log::info!("[pane-drag] raw {window_id:?} ButtonPressed({button:?}) status={status:?}");
        }
        iced::Event::Mouse(mouse::Event::ButtonReleased(button)) => {
            log::info!(
                "[pane-drag] raw {window_id:?} ButtonReleased({button:?}) status={status:?}"
            );
        }
        iced::Event::Mouse(mouse::Event::CursorEntered) => {
            log::info!("[pane-drag] raw {window_id:?} CursorEntered");
        }
        iced::Event::Mouse(mouse::Event::CursorLeft) => {
            log::info!("[pane-drag] raw {window_id:?} CursorLeft");
        }
        iced::Event::Window(window::Event::Focused) => {
            log::info!("[pane-drag] raw {window_id:?} Focused");
        }
        iced::Event::Window(window::Event::Unfocused) => {
            log::info!("[pane-drag] raw {window_id:?} Unfocused");
        }
        _ => {}
    }
}

/// QA forensics (debug builds only): logs transitions of the Win32
/// mouse-capture owner. `GetCapture` reports the capture window of the
/// *calling* thread, and the daemon's `update` runs on the winit event-loop
/// thread that owns every smudgy window, so sampling here (per message,
/// change-gated) pinpoints when the OS capture was gained, released, or
/// stolen — the ground truth that window-event logs can only imply.
#[cfg(all(target_os = "windows", debug_assertions))]
fn spike_log_capture_owner() {
    use std::sync::atomic::{AtomicIsize, Ordering};
    #[link(name = "user32")]
    unsafe extern "system" {
        fn GetCapture() -> isize;
    }
    static LAST: AtomicIsize = AtomicIsize::new(0);
    let current = unsafe { GetCapture() };
    let last = LAST.swap(current, Ordering::Relaxed);
    if current != last {
        log::info!("[pane-drag] GetCapture changed: {last:#x} -> {current:#x}");
    }
}

#[cfg(all(not(target_os = "windows"), debug_assertions))]
fn spike_log_capture_owner() {}

/// Whether the scripted-matrix forensics that are too chatty for normal play
/// are enabled: tab-bounds announcements re-log per tab on every geometry
/// change (a stream during divider drags), and only the matrix consumes them
/// (it aims real input at tab bounds without probing). Keyed off the same
/// `SMUDGY_SPIKE_AUTOSESSION` hook that arranges the matrix's sessions, so a
/// harness run gets the lines and every other launch gets none.
#[cfg(debug_assertions)]
pub(crate) fn spike_forensics_enabled() -> bool {
    static ENABLED: std::sync::LazyLock<bool> =
        std::sync::LazyLock::new(|| spike_autosession_count() > 0);
    *ENABLED
}

/// QA hook (debug builds only): `SMUDGY_SPIKE_AUTOSESSION=<n>` (1 or 2)
/// makes the first smudgy window open that many offline sessions at startup
/// (no connect-modal driving needed) and opens a second, empty smudgy
/// window — the exact arrangement the scripted drag matrix requires.
#[cfg(debug_assertions)]
fn spike_autosession_count() -> usize {
    match std::env::var("SMUDGY_SPIKE_AUTOSESSION") {
        Ok(value) if value == "1" => 1,
        Ok(value) if value == "2" => 2,
        _ => 0,
    }
}

/// The autosession runs exactly once — the second window it opens re-enters
/// the `NewSmudgyWindow` arm.
#[cfg(debug_assertions)]
static SPIKE_AUTOSESSION_DONE: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The server/profile the autosession opens: `SMUDGY_SPIKE_SERVER` /
/// `SMUDGY_SPIKE_PROFILE` when set, else "localhost" when configured (a
/// server that connects nowhere), else the first server, with its first
/// profile alphabetically.
#[cfg(debug_assertions)]
fn spike_autosession_target() -> Option<(String, String)> {
    let server = std::env::var("SMUDGY_SPIKE_SERVER").ok().or_else(|| {
        let servers = smudgy_core::models::server::list_servers().ok()?;
        servers
            .iter()
            .find(|s| s.name == "localhost")
            .or_else(|| servers.first())
            .map(|s| s.name.clone())
    })?;
    let profile = std::env::var("SMUDGY_SPIKE_PROFILE").ok().or_else(|| {
        let mut profiles = smudgy_core::models::profile::list_profiles(&server).ok()?;
        profiles.sort_by(|a, b| a.name.cmp(&b.name));
        profiles.first().map(|p| p.name.clone())
    })?;
    Some((server, profile))
}

fn update(smudgy: &mut Smudgy, message: Message) -> Task<Message> {
    let task = update_body(smudgy, message);
    // Structural pane mutations mark their window's grid dirty instead of
    // rebuilding eagerly; settling them here coalesces every mutation an
    // update cycle landed into one rebuild per window, re-deriving the
    // divider→edge map with the grid so no stale target survives into the
    // next cycle. Runs after the whole message is handled and before iced
    // paints, so `view` always reads a settled grid.
    for window in smudgy.smudgy_windows.values_mut() {
        window.flush_grid_rebuild();
    }
    // The workspace-dirty sweep: fold every window's and session's cheap
    // mutation flag into the autosave schedule (arming the trailing
    // debounce). This fixed small scan once per update is the entire
    // aggregation cost; the mutations themselves only stored booleans.
    let mut workspace_dirty = false;
    for window in smudgy.smudgy_windows.values_mut() {
        workspace_dirty |= window.take_workspace_dirty();
    }
    workspace_dirty |= smudgy.sessions.take_workspace_dirty();
    if workspace_dirty {
        smudgy.workspace.schedule.mark();
    }
    task
}

/// Start one workspace geometry poll over every live smudgy window (three
/// queries each), superseding any poll still in flight. The final
/// `WorkspaceGeometry` answer triggers the snapshot when the mirror is
/// dirty. With no windows to ask there is nothing asynchronous to wait
/// for, so a dirty mirror snapshots immediately from the cache.
fn begin_workspace_poll(smudgy: &mut Smudgy) -> Task<Message> {
    let ids: Vec<window::Id> = smudgy.smudgy_windows.keys().copied().collect();
    if ids.is_empty() {
        if smudgy.workspace.schedule.is_dirty() && !smudgy.workspace.schedule.is_shutting_down() {
            publish_workspace_snapshot(smudgy, None);
        }
        return Task::none();
    }
    let poll = smudgy.workspace.begin_poll(ids.len() * 3);
    let mut tasks = Vec::with_capacity(ids.len() * 3);
    for id in ids {
        tasks.push(
            window::position(id).map(move |origin| Message::WorkspaceGeometry {
                poll: Some(poll),
                window: id,
                sample: workspace::autosave::GeometrySample::Position(origin),
            }),
        );
        tasks.push(
            window::size(id).map(move |size| Message::WorkspaceGeometry {
                poll: Some(poll),
                window: id,
                sample: workspace::autosave::GeometrySample::Size(size),
            }),
        );
        tasks.push(
            window::scale_factor(id).map(move |scale| Message::WorkspaceGeometry {
                poll: Some(poll),
                window: id,
                sample: workspace::autosave::GeometrySample::Scale(scale),
            }),
        );
    }
    Task::batch(tasks)
}

/// The server owning the active session right now: the most recently
/// focused smudgy window hosting a live active session answers, windows
/// never yet focused trail in creation order. `None` with no active session
/// anywhere — the clean connect view, which persists nothing.
fn active_server_name(smudgy: &Smudgy) -> Option<String> {
    let of_window = |window_id: &window::Id| {
        smudgy
            .smudgy_windows
            .get(window_id)
            .and_then(SmudgyWindow::active_session_id)
            .and_then(|active| smudgy.sessions.get(active))
            .map(|session| session.server_name.clone())
    };
    smudgy
        .window_tracker
        .mru_order()
        .iter()
        .filter_map(of_window)
        .next()
        .or_else(|| smudgy.smudgy_windows.keys().filter_map(of_window).next())
}

/// Serialize the active session's server's footprint and hand it to the
/// writer worker as that server's last-session snapshot — how each server
/// comes to hold the most recent arrangement in which it was active. With
/// no active session (or nothing captured for it) the snapshot is settled
/// as taken and nothing is written: the files on disk keep their
/// arrangements, which is exactly what the clean connect view should leave
/// behind.
///
/// `ack` makes the write awaited (the quit flush); it is always resolved —
/// on publish, on every skip, and on every failure path — so a waiter can
/// never hang. Returns whether new bytes were actually published.
fn publish_workspace_snapshot(smudgy: &mut Smudgy, ack: Option<workspace::writer::Ack>) -> bool {
    // Snapshots read the layout model at a settled point: flush any rebuild
    // marks first (idempotent, and cheap when already settled).
    for window in smudgy.smudgy_windows.values_mut() {
        window.flush_grid_rebuild();
    }
    let force = ack.is_some();
    let resolve = |ack: Option<workspace::writer::Ack>| {
        if let Some(ack) = ack {
            let _ = ack.send(());
        }
    };
    let Some((path, snapshot)) = active_server_name(smudgy).and_then(|server| {
        let path = workspace::last_session::path(&server)?;
        let (snapshot, _notes) = capture_server_footprint(smudgy, &server)?;
        Some((path, snapshot))
    }) else {
        // The current model offers nothing to persist; the dirty flag is
        // settled so the debounce timer can go quiet. Any later mutation
        // re-marks.
        smudgy.workspace.schedule.snapshot_taken();
        resolve(ack);
        return false;
    };
    let bytes: Arc<[u8]> = match serde_json::to_vec_pretty(&snapshot) {
        Ok(mut bytes) => {
            bytes.push(b'\n');
            Arc::from(bytes)
        }
        Err(err) => {
            log::warn!("[workspace] failed to serialize the workspace snapshot: {err}");
            resolve(ack);
            return false;
        }
    };
    let Some(generation) = smudgy.workspace.adopt_bytes(&bytes, force) else {
        // Byte-identical to the previous snapshot: nothing to write.
        resolve(ack);
        return false;
    };
    match workspace::writer::global() {
        Some(writer) => {
            writer.publish(generation, path, bytes, ack);
            true
        }
        None => {
            resolve(ack);
            false
        }
    }
}

/// Translate one layout tab into durable terms — the shared `describe`
/// closure body behind both the autosave snapshot and named-layout capture.
///
/// A bound tab is described through its live pane definition. An unbound
/// tab is describable exactly when it stands for a live session's
/// not-yet-materialized pane: its slot comes from the pending session, its
/// identity from the stored descriptor, its hidden state from the pending
/// record — a quit between restore and materialization must not shed script
/// panes. A placeholder with no pending record is a vacancy and drops out:
/// closed stays closed by omission. Anything whose session has no slot in
/// `slot_of` drops out too, which is how a scoped capture excludes panes it
/// cannot round-trip.
fn describe_layout_tab(
    win: &SmudgyWindow,
    sessions: &SessionStore,
    slot_of: &HashMap<smudgy_core::session::SessionId, u64>,
    tab: &pane_groups::Tab<PaneRef>,
) -> Option<workspace::snapshot::PaneRecord> {
    use workspace::dto;

    let Some(slot_ref) = tab.binding().copied() else {
        let (session_id, key, hidden) = win.pending_pane_for_tab(tab.id())?;
        let slot = *slot_of.get(&session_id)?;
        return Some(workspace::snapshot::PaneRecord {
            slot,
            identity: workspace::restore::identity_from_key(key),
            hidden,
        });
    };
    let slot = *slot_of.get(&slot_ref.session_id)?;
    let identity = if slot_ref.key == MAIN_PANE_KEY {
        dto::PaneIdentity::Main
    } else {
        let def = sessions.get(slot_ref.session_id)?.pane_def(slot_ref.key)?;
        dto::PaneIdentity::Script {
            namespace: match &def.namespace {
                smudgy_core::session::runtime::pane::PaneNamespace::User => dto::Namespace::User,
                smudgy_core::session::runtime::pane::PaneNamespace::Package { owner, name } => {
                    dto::Namespace::Package {
                        owner: owner.to_string(),
                        name: name.to_string(),
                    }
                }
            },
            name: smudgy_core::session::runtime::pane::fold(&def.name),
            display: Some(def.name.to_string()),
        }
    };
    Some(workspace::snapshot::PaneRecord {
        slot,
        identity,
        hidden: win.pane_hidden(slot_ref),
    })
}

/// Whether `win` hosts at least one pane of `server` — a bound pane of one
/// of its sessions, or a placeholder a session of that server still owes.
/// The footprint predicate for both capture and apply scoping
/// (`docs/panes.md` §18).
fn window_hosts_server(win: &SmudgyWindow, sessions: &SessionStore, server: &str) -> bool {
    let of_server = |session_id: smudgy_core::session::SessionId| {
        sessions
            .get(session_id)
            .is_some_and(|session| session.server_name == server)
    };
    win.pane_refs()
        .into_iter()
        .any(|slot| of_server(slot.session_id))
        || win
            .layout()
            .panes()
            .iter()
            .filter_map(|tab| win.pending_pane_for_tab(tab.id()))
            .any(|(session_id, _, _)| of_server(session_id))
}

/// Capture `server`'s window footprint as a named-layout template: every
/// window hosting at least one pane of `server`, captured completely —
/// foreign panes sharing those windows included — with fully separate
/// windows never captured. Only loaded slots enter the template (a pending
/// placeholder with a live session counts as loaded; a vacancy never does);
/// the notes say what had to be left out so the caller can annotate the
/// save. `None` when no window hosts a pane of the server.
fn capture_server_footprint(
    smudgy: &mut Smudgy,
    server: &str,
) -> Option<(workspace::dto::Workspace, workspace::snapshot::CaptureNotes)> {
    use workspace::dto;

    let entries: Vec<(window::Id, u64)> = smudgy.workspace.window_entries().collect();
    let Smudgy {
        sessions,
        smudgy_windows,
        workspace: mirror,
        ..
    } = smudgy;

    let captured: Vec<(window::Id, u64)> = entries
        .into_iter()
        .filter(|(window_id, _)| {
            smudgy_windows
                .get(window_id)
                .is_some_and(|win| window_hosts_server(win, sessions, server))
        })
        .collect();
    if captured.is_empty() {
        return None;
    }

    // Only sessions whose main pane stands inside the captured footprint
    // can round-trip: everything else is annotated away below.
    let mut hosted_mains: HashSet<smudgy_core::session::SessionId> = HashSet::new();
    for (window_id, _) in &captured {
        if let Some(win) = smudgy_windows.get(window_id) {
            hosted_mains.extend(win.hosted_main_sessions());
        }
    }
    let mut slots = Vec::new();
    let mut slot_of: HashMap<smudgy_core::session::SessionId, u64> = HashMap::new();
    for (session_id, session) in sessions.iter() {
        if !hosted_mains.contains(&session_id) {
            continue;
        }
        let slot = mirror.slot_id(session_id);
        slot_of.insert(session_id, slot);
        slots.push(dto::SessionSlot {
            id: slot,
            server: session.server_name.clone(),
            profile: session.profile_name.clone(),
            connect: session.connect_intent(),
        });
    }

    let mut notes = workspace::snapshot::CaptureNotes::default();
    let mut windows = Vec::new();
    for (window_id, stable_id) in captured {
        let Some(win) = smudgy_windows.get(&window_id) else {
            continue;
        };
        if win.layout().is_empty() {
            continue;
        }
        let mut describe = |tab: &pane_groups::Tab<PaneRef>| {
            let record = describe_layout_tab(win, sessions, &slot_of, tab);
            if record.is_none() {
                if tab.binding().is_some() || win.pending_pane_for_tab(tab.id()).is_some() {
                    notes.omitted_foreign += 1;
                } else {
                    notes.omitted_vacancies += 1;
                }
            }
            record
        };
        let clusters = workspace::snapshot::clusters(win.layout(), &mut describe);
        if clusters.is_empty() {
            continue;
        }
        windows.push(dto::Window {
            id: stable_id,
            geometry: mirror.geometry_of(window_id).cloned().unwrap_or_default(),
            maximized: win.is_maximized(),
            active_slot: win
                .active_session_id()
                .and_then(|active| slot_of.get(&active).copied()),
            clusters,
        });
    }
    if windows.is_empty() {
        return None;
    }

    // Slots no captured window ended up hosting a main for would be
    // sanitized away on load; write the file in its sanitized form so what
    // is saved is exactly what will apply.
    let template = dto::Workspace {
        version: dto::SCHEMA_VERSION,
        sessions: slots,
        windows,
    }
    .sanitized();
    Some((template, notes))
}

/// The live workspace as the pure apply projection sees it: sessions in
/// open order, windows in stable-id creation order, each window's bound
/// panes grouped as its tab groups group them.
fn build_live_workspace(smudgy: &Smudgy) -> workspace::apply::LiveWorkspace {
    let mut live = workspace::apply::LiveWorkspace::default();
    for (session_id, session) in smudgy.sessions.iter() {
        live.sessions.push(workspace::apply::LiveSessionInfo {
            id: session_id,
            server: session.server_name.clone(),
            profile: session.profile_name.clone(),
        });
    }
    for (window_id, stable_id) in smudgy.workspace.window_entries() {
        let Some(win) = smudgy.smudgy_windows.get(&window_id) else {
            continue;
        };
        let layout = win.layout();
        let mut groups = Vec::new();
        // Visual emptiness counts every tab, bound or placeholder: a
        // window showing placeholder tabs is showing content and must not
        // be adopted, even though no bound pane below survives into its
        // groups.
        let mut has_tabs = false;
        for gid in layout.groups_depth_first() {
            let Some(tabs) = layout.tabs(gid) else {
                continue;
            };
            has_tabs |= !tabs.is_empty();
            let mut group = Vec::with_capacity(tabs.len());
            for tab in tabs {
                let Some(&pane) = tab.binding() else {
                    continue;
                };
                let descriptor = if pane.key == MAIN_PANE_KEY {
                    None
                } else {
                    smudgy
                        .sessions
                        .get(pane.session_id)
                        .and_then(|session| session.pane_def(pane.key))
                        .map(|def| workspace::restore::descriptor_key(&def.namespace, &def.name))
                };
                group.push(workspace::apply::LivePane {
                    pane,
                    descriptor,
                    hidden: win.pane_hidden(pane),
                });
            }
            groups.push(group);
        }
        live.windows.push(workspace::apply::LiveWindow {
            stable_id,
            empty: !has_tabs,
            groups,
        });
    }
    live
}

/// Execute a validated apply plan: strip template-claimed panes out of
/// extra windows, replace each planned window's arrangement wholesale
/// (vacancy records minted from the shared ordinal well), replay the
/// planned eyeball states through the normal user-toggle path, close what
/// the user explicitly answered close for, and — user restores only —
/// create planned windows, move adopted empty windows to their stored
/// geometry, and close emptied ones. Model mutation plus grid rebuild
/// only: no disk I/O happens here (the mirror catches up through the
/// normal debounce).
fn execute_layout_apply(smudgy: &mut Smudgy, plan: &workspace::apply::ApplyPlan) -> Task<Message> {
    use workspace::apply::WindowTarget;

    debug_assert!(plan.is_executable(), "unanswered plans must not execute");

    let stable_to_window: HashMap<u64, window::Id> = smudgy
        .workspace
        .window_entries()
        .map(|(window_id, stable_id)| (stable_id, window_id))
        .collect();

    // Every window the plan mutates. An in-flight drag or press anchored in
    // one of them is stale identity the moment the rebuild re-mints the
    // grid, so it stands down before anything moves — same terminal the
    // purge paths use.
    let mut mutated: HashSet<window::Id> = HashSet::new();
    for planned in &plan.windows {
        if let WindowTarget::Existing { stable_id } | WindowTarget::Adopted { stable_id, .. } =
            &planned.target
            && let Some(window_id) = stable_to_window.get(stable_id)
        {
            mutated.insert(*window_id);
        }
    }
    for (stable_id, _) in &plan.removals {
        if let Some(window_id) = stable_to_window.get(stable_id) {
            mutated.insert(*window_id);
        }
    }
    if smudgy.tab_drag.as_ref().is_some_and(|drag| {
        mutated.contains(&drag.source_window) || plan.close_sessions.contains(&drag.slot.session_id)
    }) {
        cancel_tab_drag(smudgy, "layout applied");
    }
    if smudgy
        .tab_press
        .is_some_and(|press| mutated.contains(&press.window))
    {
        smudgy.tab_press = None;
    }

    // Strip claimed panes out of extra in-scope windows (they keep the rest
    // of their arrangement).
    let mut emptied: Vec<window::Id> = Vec::new();
    for (stable_id, pane) in &plan.removals {
        let Some(window_id) = stable_to_window.get(stable_id) else {
            continue;
        };
        if let Some(win) = smudgy.smudgy_windows.get_mut(window_id)
            && win.remove_pane_slot(pane.session_id, pane.key)
        {
            emptied.push(*window_id);
        }
    }

    let mut tasks: Vec<Task<Message>> = Vec::new();
    let mut replays: Vec<(smudgy_core::session::SessionId, PaneKey, bool)> = Vec::new();
    for planned in &plan.windows {
        let realized = workspace::apply::realize_window(planned, &plan.vacancies);
        if realized.clusters.is_empty() {
            log::info!("[layouts] a planned window realized empty; leaving its live window as-is");
            continue;
        }
        let vacancies: Vec<workspace::restore::SessionVacancy> = realized
            .vacancies
            .into_iter()
            .map(|vacancy| workspace::restore::SessionVacancy {
                server: vacancy.server,
                profile: vacancy.profile,
                ordinal: smudgy.restore.next_vacancy_ordinal(),
                main_tab: vacancy.main_tab,
                panes: vacancy.panes,
            })
            .collect();
        replays.extend(realized.replays.iter().copied());
        match &planned.target {
            WindowTarget::Existing { stable_id } | WindowTarget::Adopted { stable_id, .. } => {
                let Some(window_id) = stable_to_window.get(stable_id) else {
                    log::info!(
                        "[layouts] planned window {stable_id} is gone; skipping its install"
                    );
                    continue;
                };
                let Some(win) = smudgy.smudgy_windows.get_mut(window_id) else {
                    continue;
                };
                win.install_applied_layout(
                    pane_groups::GroupLayout::from_blueprint(realized.clusters),
                    realized.pending,
                    vacancies,
                    realized.hidden,
                    realized.active,
                );
                // An adopted empty window takes the template window's
                // stored geometry, exactly as a created window would.
                if let WindowTarget::Adopted {
                    geometry,
                    maximized,
                    ..
                } = &planned.target
                {
                    debug_assert!(!plan.script_scoped, "script applies never adopt OS windows");
                    let bounds = workspace::restore::virtual_screen_bounds();
                    let (position, size) = workspace::restore::clamp_geometry(
                        geometry,
                        bounds,
                        Size::new(640.0, 400.0),
                    );
                    tasks.push(window::resize(*window_id, size));
                    if let Some(point) = position {
                        tasks.push(window::move_to(*window_id, point));
                    }
                    if *maximized {
                        tasks.push(window::maximize(*window_id, true));
                        // Seed the mirror alongside the request; the resize
                        // event's authoritative answer lands a few frames
                        // later.
                        if let Some(win) = smudgy.smudgy_windows.get_mut(window_id) {
                            win.seed_maximized(true);
                        }
                    }
                }
            }
            WindowTarget::New {
                geometry,
                maximized,
            } => {
                debug_assert!(
                    !plan.script_scoped,
                    "script applies never create OS windows"
                );
                let bounds = workspace::restore::virtual_screen_bounds();
                let (position, size) =
                    workspace::restore::clamp_geometry(geometry, bounds, Size::new(640.0, 400.0));
                let mut settings = smudgy_window_settings();
                settings.size = size;
                // Open maximized directly (no floating-size flash), and seed
                // the window's maximize mirror below so the first frames don't
                // draw the floating frame chrome while the async
                // `window::is_maximized` round trip is still in flight.
                settings.maximized = *maximized;
                if let Some(point) = position {
                    settings.position = window::Position::Specific(point);
                }
                let (id, open) = window::open(settings);
                let mut fresh = SmudgyWindow::new(id, smudgy.account.handles());
                fresh.seed_maximized(*maximized);
                fresh.install_applied_layout(
                    pane_groups::GroupLayout::from_blueprint(realized.clusters),
                    realized.pending,
                    vacancies,
                    realized.hidden,
                    realized.active,
                );
                smudgy.smudgy_windows.insert(id, fresh);
                smudgy.workspace.register_window(id);
                tasks.push(open.map(Message::NewSmudgyWindow));
            }
        }
    }

    // The planned eyeball states go through the same report path a click
    // takes, so core's registry stays the source of truth; runtimes that
    // are not ready yet are owed the replay instead.
    for (session, key, hidden) in replays {
        if smudgy.restore.is_ready(session) {
            if let Some(store_session) = smudgy.sessions.get(session) {
                store_session.report_user_hidden(key, hidden);
            }
        } else {
            smudgy.restore.owe_hidden(session, key, hidden);
        }
    }

    // Closing is never silent: every id here carries an explicit answer.
    for &session in &plan.close_sessions {
        let close = close_session(smudgy, session);
        tasks.push(close);
    }

    // A stripped-empty extra window closes for a user restore; a script
    // apply never closes an OS window, so it stays open on its empty
    // connect state.
    if !plan.script_scoped {
        tasks.push(close_emptied_windows(smudgy, emptied));
    } else if !emptied.is_empty() {
        log::info!(
            "[layouts] a script apply emptied {} window(s); leaving them open",
            emptied.len()
        );
    }

    tasks.push(report_pane_sizes(smudgy));
    Task::batch(tasks)
}

/// Capture the acting server's footprint and save it under `name`,
/// reporting the outcome — including how much the capture had to leave out
/// — back into the initiating window's Layouts modal.
///
/// Unlike the script path, the write here is synchronous: a user save is
/// explicit and rare, and the modal's saved/failed status line reports the
/// write's real outcome, which a deferred best-effort write could not.
fn save_named_layout(
    smudgy: &mut Smudgy,
    window_id: window::Id,
    server: &str,
    name: &str,
) -> Task<Message> {
    let outcome = match capture_server_footprint(smudgy, server) {
        Some((template, notes)) => match workspace::layouts::save(server, name, &template) {
            Ok(()) => {
                if notes.is_partial() {
                    log::info!(
                        "[layouts] capture of '{name}' for {server} was partial: \
                         {} vacancy tab(s), {} foreign pane(s) omitted",
                        notes.omitted_vacancies,
                        notes.omitted_foreign
                    );
                }
                components::modal::layouts::SaveOutcome::Saved {
                    name: name.to_string(),
                    omitted: notes.omitted_vacancies + notes.omitted_foreign,
                }
            }
            Err(error) => components::modal::layouts::SaveOutcome::Failed {
                error: error.to_string(),
            },
        },
        None => components::modal::layouts::SaveOutcome::Failed {
            error: workspace::apply::ApplyError::NoLiveFootprint.to_string(),
        },
    };
    Task::done(Message::SmudgyWindowMessage(
        window_id,
        windows::smudgy_window::Message::LayoutSaveOutcome(outcome),
    ))
}

/// Apply a stored template of `server`'s — a named layout or the server's
/// last-session snapshot — as a user restore: project the plan, route
/// unanswered keep-or-close questions back to the initiating window, spawn
/// missing slots per their stored intent, re-run the projection over the
/// workspace as it now stands, and execute only a revalidated plan.
fn apply_workspace_template(
    smudgy: &mut Smudgy,
    window_id: window::Id,
    server: &str,
    source: &workspace::TemplateSource,
    answers: &HashMap<SessionId, workspace::apply::OmittedAnswer>,
) -> Task<Message> {
    if smudgy.workspace.schedule.is_shutting_down() {
        return Task::none();
    }
    let template = match source {
        workspace::TemplateSource::Named(name) => match workspace::layouts::load(server, name) {
            Ok(template) => template,
            Err(error) => {
                log::info!("[layouts] cannot apply {source} for {server}: {error}");
                return Task::none();
            }
        },
        workspace::TemplateSource::LastSession => {
            match workspace::last_session::read(server) {
                Some(template) => template,
                None => {
                    // The affordance is offered only while the file parses,
                    // so this is a rare race with a concurrent rewrite.
                    log::info!("[layouts] {server} has no usable last-session snapshot");
                    return Task::none();
                }
            }
        }
    };
    // The initiating window, by stable id: when it is visually empty —
    // a fresh window whose connect surface drove the restore — the plan
    // adopts it first, so the restore lands in it instead of beside it.
    let mode = workspace::apply::ApplyMode::User {
        initiating: smudgy
            .workspace
            .window_entries()
            .find_map(|(id, stable_id)| (id == window_id).then_some(stable_id)),
    };
    let live = build_live_workspace(smudgy);
    let plan = match workspace::apply::plan_apply(&template, &live, mode, answers) {
        Ok(plan) => plan,
        Err(error) => {
            log::info!("[layouts] cannot apply {source} for {server}: {error}");
            return Task::none();
        }
    };
    if !plan.questions.is_empty() {
        // Keep-or-close is asynchronous: ask, and re-project with the
        // answers once they arrive (the workspace may drift meanwhile).
        let rows: Vec<components::modal::layouts::OmittedRow> = plan
            .questions
            .iter()
            .filter_map(|&session_id| {
                let session = smudgy.sessions.get(session_id)?;
                Some(components::modal::layouts::OmittedRow {
                    session: session_id,
                    label: format!("{} @ {}", session.profile_name, session.server_name),
                    close: false,
                })
            })
            .collect();
        return Task::done(Message::SmudgyWindowMessage(
            window_id,
            windows::smudgy_window::Message::PromptLayoutAnswers {
                server: server.to_string(),
                source: source.clone(),
                rows,
            },
        ));
    }
    let plan = if plan.spawns.is_empty() {
        plan
    } else {
        // Spawn through the normal open path with each slot's stored
        // intent (an online slot reconnects exactly as the Connect button
        // would), then re-project so the fresh sessions bind and the plan
        // is validated against the workspace actually being mutated.
        for spawn in &plan.spawns {
            let session_id = smudgy.sessions.open_session(
                spawn.server.clone(),
                spawn.profile.clone(),
                spawn.connect,
            );
            log::info!(
                "[layouts] spawned {} ({}/{}) for {source}",
                session_id,
                spawn.server,
                spawn.profile
            );
        }
        let live = build_live_workspace(smudgy);
        match workspace::apply::plan_apply(&template, &live, mode, answers) {
            Ok(plan) => plan,
            Err(error) => {
                log::info!("[layouts] replan of {source} failed after spawning: {error}");
                return Task::none();
            }
        }
    };
    if !plan.is_executable() {
        log::info!("[layouts] plan for {source} did not settle; not applying");
        return Task::none();
    }
    let live = build_live_workspace(smudgy);
    if let Err(error) = workspace::apply::validate_conservation(&template, &live, mode, &plan) {
        log::info!("[layouts] conservation check refused {source}: {error}");
        return Task::none();
    }
    execute_layout_apply(smudgy, &plan)
}

/// The Reset action: release `session_id`'s retained slot geometry — its
/// pending placeholders and every vacancy matching its server/profile —
/// then re-place its script panes from their current definitions through
/// the normal placement chain (beside the session's main, at split
/// defaults), with each def's hidden state re-asserted. The escape hatch
/// for persisted geometry shadowing script changes.
fn reset_session_layout(smudgy: &mut Smudgy, session_id: SessionId) -> Task<Message> {
    let Some((server, profile)) = smudgy
        .sessions
        .get(session_id)
        .map(|session| (session.server_name.clone(), session.profile_name.clone()))
    else {
        return Task::none();
    };
    // A drag anchored in geometry about to be re-placed is stale identity.
    if smudgy
        .tab_drag
        .as_ref()
        .is_some_and(|drag| drag.slot.session_id == session_id)
    {
        cancel_tab_drag(smudgy, "layout reset");
    }

    let mut emptied: Vec<window::Id> = Vec::new();
    let mut script_panes: Vec<PaneRef> = Vec::new();
    for (id, win) in smudgy.smudgy_windows.iter_mut() {
        let reaped = win.reap_session_placeholders(session_id);
        let released = win.release_vacancies(&server, &profile);
        if reaped || released {
            emptied.push(*id);
        }
        script_panes.extend(
            win.pane_refs()
                .into_iter()
                .filter(|slot| slot.session_id == session_id && slot.key != MAIN_PANE_KEY),
        );
    }
    let mut removal_emptied: Vec<window::Id> = Vec::new();
    for pane in &script_panes {
        for (id, win) in smudgy.smudgy_windows.iter_mut() {
            if win.remove_pane_slot(pane.session_id, pane.key) {
                removal_emptied.push(*id);
            }
        }
    }
    emptied.extend(removal_emptied);
    for pane in &script_panes {
        place_pane_in_windows(
            smudgy,
            session_id,
            pane.key,
            PanePlacement::Split {
                reference: MAIN_PANE_KEY,
                direction: SplitDirection::Right,
                size_px: None,
            },
        );
        // The def's own hidden state is the truth being re-asserted.
        let hidden = smudgy
            .sessions
            .get(session_id)
            .and_then(|session| session.pane_def(pane.key))
            .is_some_and(|def| def.hidden);
        sync_pane_hidden(smudgy, *pane, hidden);
    }
    log::info!(
        "[layouts] reset session {session_id} ({server}/{profile}): re-placed {} pane(s)",
        script_panes.len()
    );
    let close = close_emptied_windows(smudgy, emptied);
    let report = report_pane_sizes(smudgy);
    Task::batch([close, report])
}

/// Latch the autosave schedule shut and publish the final pre-teardown
/// snapshot with an awaited ack. Returns the task that completes the quit
/// (`WorkspaceQuitFlushed` → `iced::exit`), or `None` when the flush was
/// already taken by an earlier quit path.
fn begin_workspace_quit_flush(smudgy: &mut Smudgy) -> Option<Task<Message>> {
    // The latch must be tested-and-set before building: after it, no
    // teardown event can mark the schedule or mint a newer generation, so
    // the flush below is guaranteed to stay the newest snapshot.
    if !smudgy.workspace.schedule.begin_shutdown() {
        return None;
    }
    let (ack, done) = tokio::sync::oneshot::channel();
    // `publish_workspace_snapshot` resolves the ack on every path, so the
    // awaited task below always completes — and the await is bounded
    // besides, so a wedged disk write cannot hold the exit hostage (the
    // last completed write stands; the atomic replace cannot tear).
    let _ = publish_workspace_snapshot(smudgy, Some(ack));
    Some(Task::perform(
        workspace::writer::await_ack_bounded(done, workspace::writer::QUIT_FLUSH_TIMEOUT),
        |()| Message::WorkspaceQuitFlushed,
    ))
}

fn update_body(smudgy: &mut Smudgy, message: Message) -> Task<Message> {
    // QA forensics (debug builds only): change-gated OS capture-owner
    // sampling. Runs for every message; during a drag the motion message
    // stream gives it per-sample resolution.
    #[cfg(debug_assertions)]
    spike_log_capture_owner();
    match message {
        Message::WindowTracking(id, event) => {
            smudgy.window_tracker.apply(id, event);
            // Every motion sample drives the hover classification the
            // overlay renders; tracking only runs mid-drag.
            if let pane_drag::TrackEvent::CursorMoved(position) = event {
                track_drag_motion(smudgy, id, position);
            }
            Task::none()
        }
        Message::PaneDragTerminal(window_id, terminal) => match terminal {
            pane_drag::DragTerminal::Released => {
                // A release always settles the pending press: below the
                // deadband the gesture was a click, and selection is the
                // press surface's fast path (its release event).
                smudgy.tab_press = None;
                if smudgy.tab_drag.is_some() {
                    // The authoritative terminal: resolve against the last
                    // tracked cursor sample of the source window. No sample
                    // means no honest release point — cancel, never a
                    // fabricated origin.
                    log::info!(
                        "[pane-drag] raw ButtonReleased via {window_id:?} — authoritative terminal"
                    );
                    let point = smudgy
                        .tab_drag
                        .as_ref()
                        .and_then(|drag| smudgy.window_tracker.get(drag.source_window))
                        .and_then(|track| track.cursor);
                    finish_tab_drag(smudgy, point)
                } else {
                    Task::none()
                }
            }
            pane_drag::DragTerminal::Escape => {
                // Escape cancels a live drag. Below the deadband it
                // deliberately does nothing: the press continues and the
                // release still classifies as a click.
                cancel_tab_drag(smudgy, "escape");
                Task::none()
            }
            pane_drag::DragTerminal::Unfocused => {
                // The gesture's source window losing focus is the daemon's
                // capture-loss terminal (Win+L, UAC, WM_CANCELMODE): the raw
                // release will never arrive, so an armed gesture must stand
                // down here — an orphaned press would keep full-rate
                // tracking subscribed forever and promote into a buttonless
                // drag on the next cursor pass over the source window. The
                // widget's own CaptureLost is the fast path; this terminal
                // survives a press surface wiped by a subtree rebuild.
                match pane_drag::unfocus_stand_down(
                    window_id,
                    smudgy.tab_press.map(|press| press.window),
                    smudgy.tab_drag.as_ref().map(|drag| drag.source_window),
                ) {
                    Some(pane_drag::StandDown::Press) => {
                        log::info!("[pane-drag] press stood down (source {window_id:?} unfocused)");
                        smudgy.tab_press = None;
                    }
                    Some(pane_drag::StandDown::Drag) => {
                        smudgy.tab_press = None;
                        cancel_tab_drag(smudgy, "source window unfocused");
                    }
                    None => {}
                }
                Task::none()
            }
        },
        Message::CloseWindow(id) => {
            smudgy.window_tracker.remove(id);
            smudgy.closing_windows.remove(&id);
            // The source window dying mid-drag ends the drag: its model is
            // gone, so the drag identity can never re-resolve. A press
            // candidate in the dying window dies with it.
            if smudgy
                .tab_drag
                .as_ref()
                .is_some_and(|drag| drag.source_window == id)
            {
                cancel_tab_drag(smudgy, "source window closed");
            }
            if smudgy.tab_press.is_some_and(|press| press.window == id) {
                smudgy.tab_press = None;
            }
            // Closing the LAST smudgy window is quit, and the final snapshot
            // must capture the workspace as it stood — this window, its
            // sessions, everything — before any teardown empties it. Taken
            // here, ahead of the removal below; the teardown that follows can
            // no longer mark or build (the schedule latches shut), so no
            // emptied state can overwrite the flush.
            let quit_flush =
                if smudgy.smudgy_windows.len() == 1 && smudgy.smudgy_windows.contains_key(&id) {
                    begin_workspace_quit_flush(smudgy)
                } else {
                    None
                };
            if let Some(window) = smudgy.smudgy_windows.remove(&id) {
                smudgy.workspace.forget_window(id);
                // Window-close cascade: closing a window closes every session
                // whose MAIN pane lived in it. The store entries are shut
                // down and removed *before* any grid cleanup so events still
                // in flight for those ids are dropped at the daemon; the
                // purge then sweeps the dead sessions' panes out of the
                // remaining windows' grids. Surviving sessions' panes hosted
                // in the closing window re-home next to their main pane —
                // a first-class flow (closing a torn-out chat-pane window
                // sends the chat pane back beside its session).
                let victims = window.hosted_main_sessions();
                let orphans: Vec<PaneRef> = window
                    .pane_refs()
                    .into_iter()
                    .filter(|slot| slot.key != MAIN_PANE_KEY && !victims.contains(&slot.session_id))
                    .collect();
                for session_id in &victims {
                    smudgy.sessions.shutdown_and_remove(*session_id);
                    forget_session_pane_commands(smudgy, *session_id);
                }
                let purge_task = purge_sessions_from_windows(smudgy, &victims);
                for slot in orphans {
                    // The session may have raced to a close of its own; a
                    // missing store entry just drops the pane.
                    if smudgy.sessions.get(slot.session_id).is_none() {
                        continue;
                    }
                    place_pane_in_windows(
                        smudgy,
                        slot.session_id,
                        slot.key,
                        PanePlacement::Split {
                            reference: MAIN_PANE_KEY,
                            direction: SplitDirection::Right,
                            size_px: None,
                        },
                    );
                }
                if smudgy.smudgy_windows.is_empty() {
                    for editor in smudgy.map_editor_windows.values() {
                        editor.prepare_to_close();
                    }
                    // Quit defers `iced::exit()` behind the write-complete
                    // message so the final flush finishes before loop
                    // teardown — the one documented place the fire-and-
                    // forget write shape is replaced by an awaited one.
                    let exit_task = quit_flush.unwrap_or_else(iced::exit);
                    Task::batch([purge_task, exit_task])
                } else {
                    // Closing one of several windows is a workspace
                    // mutation and persists through the normal debounce.
                    smudgy.workspace.schedule.mark();
                    purge_task
                }
            } else if smudgy.automations_windows.contains_key(&id) {
                smudgy.automations_windows.remove(&id);
                Task::none()
            } else if smudgy.settings_windows.contains_key(&id) {
                smudgy.settings_windows.remove(&id);
                Task::none()
            } else {
                if let Some(window) = smudgy.map_editor_windows.get(&id) {
                    window.prepare_to_close();
                }
                smudgy.map_editor_windows.remove(&id);
                Task::none()
            }
        }
        Message::Account(msg) => smudgy.account.update(msg).map(Message::Account),
        Message::SmudgyWindowMessage(id, msg) => {
            let Some(window) = smudgy.smudgy_windows.get_mut(&id) else {
                log::warn!("Received message for unknown window index: {}", id);
                return Task::none();
            };
            let update = window.update(msg, &mut smudgy.sessions);
            let task = update
                .task
                .map(move |message| Message::SmudgyWindowMessage(id, message));

            let handled = match update.event {
                Some(SmudgyWindowEvent::CreateNewScriptEditorWindow {
                    server_name,
                    session_id,
                }) => Task::batch([
                    task,
                    Task::done(Message::CreateAutomationsWindow {
                        server_name,
                        session_id,
                    }),
                ]),
                Some(SmudgyWindowEvent::CreateNewMapEditorWindow {
                    mapper,
                    server_name,
                }) => Task::batch([
                    task,
                    Task::done(Message::CreateMapEditorWindow {
                        mapper,
                        server_name,
                    }),
                ]),
                Some(SmudgyWindowEvent::SetMapperCurrentLocation(area_id, room_number)) => {
                    Task::batch([
                        task,
                        Task::done(Message::SetMapperCurrentLocation(area_id, room_number)),
                    ])
                }
                Some(SmudgyWindowEvent::CloseSession(session_id)) => {
                    Task::batch([task, close_session(smudgy, session_id)])
                }
                Some(SmudgyWindowEvent::TabDragPressed {
                    tab,
                    slot,
                    group,
                    point,
                }) => {
                    // Not yet a drag, but the gesture is daemon-owned from
                    // here: tracked motion past the deadband promotes this
                    // press even if the press surface's widget state is
                    // erased by an async subtree rebuild mid-gesture. Also
                    // refresh every candidate window's origin *and* scale
                    // factor so a drag that follows hit-tests fresh
                    // geometry: origins go stale while idle (`Moved` is
                    // only tracked mid-drag), and scale is only otherwise
                    // learned from `Rescaled`, which never fires for a
                    // window that opened at its final DPI. The answers race
                    // the drag, but a human drag outlasts a task round-trip
                    // by orders of magnitude.
                    log::info!(
                        "[pane-drag] press {}/{} window={id:?} local=({:.1}, {:.1})",
                        slot.session_id,
                        slot.key,
                        point.x,
                        point.y,
                    );
                    smudgy.tab_press = Some(pane_drag::PendingPress {
                        window: id,
                        tab,
                        slot,
                        group,
                        press: point,
                    });
                    let mut tasks = vec![task];
                    for &window_id in smudgy.smudgy_windows.keys() {
                        tasks.push(window::position(window_id).map(move |origin| {
                            Message::WindowTracking(
                                window_id,
                                pane_drag::TrackEvent::Origin(origin),
                            )
                        }));
                        tasks.push(window::scale_factor(window_id).map(move |scale| {
                            Message::WindowTracking(
                                window_id,
                                pane_drag::TrackEvent::Rescaled(scale),
                            )
                        }));
                    }
                    Task::batch(tasks)
                }
                Some(SmudgyWindowEvent::TabDragStarted {
                    tab,
                    slot,
                    group,
                    press,
                    point,
                }) => {
                    // The widget's own deadband crossing — the fast path.
                    // The daemon may already have promoted the pending press
                    // from tracked motion; a second start for the same tab
                    // is a no-op.
                    if smudgy.tab_drag.as_ref().is_some_and(|drag| drag.tab == tab) {
                        return Task::batch([task, report_pane_sizes(smudgy)]);
                    }
                    smudgy.tab_press = None;
                    log::info!(
                        "[pane-drag] drag started: tab {tab:?} ({}/{}) from {id:?}, press=({:.1}, {:.1})",
                        slot.session_id,
                        slot.key,
                        press.x,
                        press.y,
                    );
                    // A fresh start supersedes any stale record (a drag that
                    // somehow ended without a terminal).
                    smudgy.tab_drag = Some(pane_drag::TabDrag {
                        source_window: id,
                        tab,
                        slot,
                        source_group: group,
                        press,
                        hover: None,
                    });
                    // Classify the starting point immediately so the first
                    // overlay frame agrees with the cursor.
                    track_drag_motion(smudgy, id, point);
                    task
                }
                Some(SmudgyWindowEvent::TabDragReleased { point }) => {
                    // The widget's release is evidence, never resolution:
                    // the strip's scrollable reports the cursor to children
                    // as unavailable outside its viewport (most of any
                    // drag) and scroll-translated inside it, so this point
                    // is absent or content-space exactly when it matters.
                    // The authoritative raw-release terminal fires from the
                    // same OS event and resolves with the window-space
                    // tracked sample; a drag that started has at least one
                    // sample, so nothing is lost by deferring (and a drag
                    // with no sample at all cancels there).
                    log::info!(
                        "[pane-drag] widget release observed (point {}) — deferring to the raw terminal",
                        if point.is_some() {
                            "present"
                        } else {
                            "unavailable"
                        },
                    );
                    task
                }
                Some(SmudgyWindowEvent::TabDragCanceled { reason }) => {
                    cancel_tab_drag(smudgy, reason);
                    task
                }
                Some(SmudgyWindowEvent::OpenSettingsWindow) => {
                    Task::batch([task, Task::done(Message::CreateSettingsWindow)])
                }
                Some(SmudgyWindowEvent::OpenDownloadPage) => {
                    // User clicked an "out of date"/"upgrade available" link —
                    // opening the browser here is user-initiated, not autonomous.
                    log::info!("opening the download page ({DOWNLOAD_URL})");
                    std::thread::spawn(|| {
                        if let Err(e) = open::that(DOWNLOAD_URL) {
                            log::warn!("failed to open the download page ({DOWNLOAD_URL}): {e}");
                        }
                    });
                    task
                }
                Some(SmudgyWindowEvent::DismissUpgrade) => {
                    smudgy.account.dismiss_upgrade();
                    task
                }
                Some(SmudgyWindowEvent::DismissUpgradeForVersion) => {
                    smudgy.account.dismiss_upgrade_for_version();
                    task
                }
                Some(SmudgyWindowEvent::PaneVisibilityToggled { slot, hidden }) => {
                    // The window flipped optimistically; the def lives on the
                    // pane's session runtime, which echoes `PaneUpdated` to
                    // converge every consumer (and fires `pane:visibility`).
                    if let Some(session) = smudgy.sessions.get(slot.session_id) {
                        session.report_user_hidden(slot.key, hidden);
                    }
                    task
                }
                Some(SmudgyWindowEvent::SaveLayout { server, name }) => {
                    let save = save_named_layout(smudgy, id, &server, &name);
                    Task::batch([task, save])
                }
                Some(SmudgyWindowEvent::ApplyLayout { server, name }) => {
                    let apply = apply_workspace_template(
                        smudgy,
                        id,
                        &server,
                        &workspace::TemplateSource::Named(name),
                        &HashMap::new(),
                    );
                    Task::batch([task, apply])
                }
                Some(SmudgyWindowEvent::RestoreLastSession { server }) => {
                    let apply = apply_workspace_template(
                        smudgy,
                        id,
                        &server,
                        &workspace::TemplateSource::LastSession,
                        &HashMap::new(),
                    );
                    Task::batch([task, apply])
                }
                Some(SmudgyWindowEvent::ApplyLayoutWithAnswers {
                    server,
                    source,
                    close,
                    keep,
                }) => {
                    let mut answers: HashMap<SessionId, workspace::apply::OmittedAnswer> =
                        HashMap::new();
                    for session in keep {
                        answers.insert(session, workspace::apply::OmittedAnswer::Keep);
                    }
                    for session in close {
                        answers.insert(session, workspace::apply::OmittedAnswer::Close);
                    }
                    let apply = apply_workspace_template(smudgy, id, &server, &source, &answers);
                    Task::batch([task, apply])
                }
                Some(SmudgyWindowEvent::ResetSessionLayout(session_id)) => {
                    let reset = reset_session_layout(smudgy, session_id);
                    Task::batch([task, reset])
                }
                None => task,
            };
            // Any window update may have moved pane geometry (divider drags,
            // window resizes, toolbar toggles): feed the pane-size mirror.
            // Cheap, and a no-op for sessions without mirror interest.
            let report = report_pane_sizes(smudgy);
            Task::batch([handled, report])
        }
        Message::FlushPaneSizes(session_id) => {
            if let Some(session) = smudgy.sessions.get_mut(session_id) {
                session.flush_pane_sizes();
            }
            Task::none()
        }
        Message::WorkspaceDebounceTick => {
            if smudgy.workspace.schedule.debounce_settled() {
                // The churn settled: snapshot at a fresh geometry poll, so
                // the write carries current placement too.
                begin_workspace_poll(smudgy)
            } else {
                Task::none()
            }
        }
        Message::WorkspaceCheckpointTick => {
            if smudgy.workspace.schedule.is_shutting_down() {
                Task::none()
            } else {
                // Unconditional: the poll's change detection is what
                // notices window moves (idle tracking deliberately drops
                // them), and a dirty mirror writes at most once per
                // checkpoint under sustained churn.
                begin_workspace_poll(smudgy)
            }
        }
        Message::WorkspaceGeometry {
            poll,
            window,
            sample,
        } => {
            let outcome = smudgy.workspace.record_sample(poll, window, sample);
            if outcome.poll_complete
                && smudgy.workspace.schedule.is_dirty()
                && !smudgy.workspace.schedule.is_shutting_down()
            {
                publish_workspace_snapshot(smudgy, None);
            }
            Task::none()
        }
        Message::WorkspaceQuitFlushed => {
            // The final snapshot is durable — or the writer is gone, or the
            // bounded wait expired, and waiting longer cannot help either
            // way: finish the deferred quit.
            iced::exit()
        }
        Message::UiCommand(envelope) => handle_ui_command(smudgy, envelope),
        Message::SessionEvent(TaggedSessionEvent { session_id, event }) => {
            // Open is repeated on the owning session stream to order display
            // state before output. A fast later Close can win the independent
            // bus subscription; do not let that delayed echo resurrect the
            // retired layout entry.
            if let SessionEvent::PaneOpened { def, .. } = &event {
                let pane = PaneRef {
                    session_id,
                    key: def.key,
                };
                if smudgy.retired_panes.contains(&pane) {
                    log::debug!("Dropping delayed PaneOpened for retired pane {pane:?}");
                    return Task::none();
                }
            }
            // The command bus and each session stream are separate iced
            // subscriptions. If the flush-confirming close event wins their
            // race, hold it until the command has removed the pane from the
            // layout in canonical bus order.
            if let SessionEvent::PaneClosedOrdered(key) = &event {
                let pane = PaneRef {
                    session_id,
                    key: *key,
                };
                if !smudgy.retired_panes.contains(&pane) {
                    if smudgy.sessions.get(session_id).is_some() {
                        smudgy.pending_ordered_pane_closes.insert(pane);
                    }
                    return Task::none();
                }
            }
            // Connection edges re-derive the Discord activity — after the
            // session's own update below has adopted the new connected state.
            let presence_edge =
                matches!(event, SessionEvent::Connected | SessionEvent::Disconnected);
            // Per-server map-scope reactions live on the daemon (it owns the
            // authoritative `map_scopes`, which the session store doesn't), so
            // handle them here before the event is forwarded to the session.
            // The session's own update no-ops on them.
            let scope_task = match &event {
                SessionEvent::MapperNavigated(area_id) => {
                    observe_navigation_for_binding(smudgy, session_id, *area_id)
                }
                SessionEvent::MapAreaCreated(area_id) => {
                    associate_created_area(smudgy, session_id, *area_id)
                }
                SessionEvent::MapAtlasCreated(atlas_id) => {
                    associate_created_atlas(smudgy, session_id, *atlas_id)
                }
                _ => Task::none(),
            };
            // Pane lifecycle, def-state, and placement events touch both the
            // store (display state, handled by the session's own update
            // below) and the windows' grids (handled here at the daemon,
            // which owns the window map).
            let pane_follow_up = match &event {
                SessionEvent::PaneOpened { def, placement } => Some(PaneFollowUp::Opened {
                    key: def.key,
                    placement: *placement,
                    hidden: def.hidden,
                }),
                SessionEvent::PaneClosed(key) => {
                    retire_pane_commands(
                        smudgy,
                        PaneRef {
                            session_id,
                            key: *key,
                        },
                    );
                    Some(PaneFollowUp::Closed(*key))
                }
                SessionEvent::PaneClosedOrdered(_) => None,
                SessionEvent::PaneUpdated(def) => Some(PaneFollowUp::DefSync {
                    key: def.key,
                    hidden: def.hidden,
                }),
                SessionEvent::PaneResize { key, width, height } => Some(PaneFollowUp::Resize {
                    key: *key,
                    width: *width,
                    height: *height,
                }),
                SessionEvent::PaneRelocate {
                    key,
                    reference,
                    direction,
                    size_px,
                } => Some(PaneFollowUp::Relocate {
                    key: *key,
                    reference: *reference,
                    direction: *direction,
                    size_px: *size_px,
                }),
                SessionEvent::PaneGroupWith {
                    key,
                    reference_session,
                    reference,
                    position,
                    selected,
                } => Some(PaneFollowUp::GroupWith {
                    key: *key,
                    reference_session: *reference_session,
                    reference: *reference,
                    position: *position,
                    selected: *selected,
                }),
                SessionEvent::PaneSelect { key } => Some(PaneFollowUp::Select { key: *key }),
                SessionEvent::PaneTearOut { key, width, height } => Some(PaneFollowUp::TearOut {
                    key: *key,
                    width: *width,
                    height: *height,
                }),
                SessionEvent::PaneSwap {
                    key,
                    other_session,
                    other_key,
                } => Some(PaneFollowUp::Swap {
                    key: *key,
                    other_session: *other_session,
                    other_key: *other_key,
                }),
                SessionEvent::PaneMirrorInterest => Some(PaneFollowUp::MirrorInterest),
                SessionEvent::LayoutSave { name } => Some(PaneFollowUp::LayoutSave(name.clone())),
                SessionEvent::LayoutApply { name } => Some(PaneFollowUp::LayoutApply(name.clone())),
                _ => None,
            };
            let runtime_ready = matches!(event, SessionEvent::RuntimeReady(_));
            if let Some(session) = smudgy.sessions.get_mut(session_id) {
                let task = session
                    .update(session_store::Message::SessionEvent(event))
                    .map(move |msg| Message::SessionAction(session_id, msg));
                if runtime_ready {
                    // The runtime can accept reports now (the session just
                    // adopted its channel): flush the owed eyeball replays —
                    // once per restored pane, through the same path a click
                    // takes — and reap the placeholders whose panes never
                    // materialized (missing, renamed, unauthorized). Both
                    // are loud no-ops on reload-triggered readiness.
                    for (key, hidden) in smudgy.restore.mark_ready(session_id) {
                        if let Some(session) = smudgy.sessions.get(session_id) {
                            session.report_user_hidden(key, hidden);
                        }
                    }
                    let mut emptied = Vec::new();
                    for (window_id, window) in smudgy.smudgy_windows.iter_mut() {
                        if window.reap_session_placeholders(session_id) {
                            emptied.push(*window_id);
                        }
                    }
                    if !emptied.is_empty() {
                        let close = close_emptied_windows(smudgy, emptied);
                        let follow = Task::batch([task, close]);
                        if presence_edge {
                            refresh_discord_presence(smudgy);
                        }
                        return follow;
                    }
                }
                let pane_task = match pane_follow_up {
                    Some(PaneFollowUp::Opened {
                        key,
                        placement,
                        hidden,
                    }) => {
                        let placed = place_pane_in_windows(smudgy, session_id, key, placement);
                        // A pre-hidden spec (`hidden: true` at split) seeds
                        // the hosting window's toggle before first paint —
                        // reveal-on-event panes never flash at load.
                        if hidden {
                            sync_pane_hidden(smudgy, PaneRef { session_id, key }, true);
                        }
                        let select = if placed
                            && matches!(placement, PanePlacement::Tab { selected: true, .. })
                        {
                            select_script_pane(smudgy, PaneRef { session_id, key })
                        } else {
                            Task::none()
                        };
                        Task::batch([select, report_pane_sizes(smudgy)])
                    }
                    Some(PaneFollowUp::Closed(key)) => {
                        remove_pane_from_windows(smudgy, session_id, key)
                    }
                    Some(PaneFollowUp::DefSync { key, hidden }) => {
                        sync_pane_hidden(smudgy, PaneRef { session_id, key }, hidden);
                        report_pane_sizes(smudgy)
                    }
                    Some(PaneFollowUp::Resize { key, width, height }) => {
                        let slot = PaneRef { session_id, key };
                        for window in smudgy.smudgy_windows.values_mut() {
                            if window.hosts_pane(session_id, key) {
                                window.resize_pane_slot(slot, width, height);
                            }
                        }
                        report_pane_sizes(smudgy)
                    }
                    Some(PaneFollowUp::Relocate {
                        key,
                        reference,
                        direction,
                        size_px,
                    }) => {
                        relocate_script_pane(smudgy, session_id, key, reference, direction, size_px)
                    }
                    Some(PaneFollowUp::GroupWith {
                        key,
                        reference_session,
                        reference,
                        position,
                        selected,
                    }) => group_script_pane(
                        smudgy,
                        PaneRef { session_id, key },
                        PaneRef {
                            session_id: reference_session,
                            key: reference,
                        },
                        position,
                        selected,
                    ),
                    Some(PaneFollowUp::Select { key }) => {
                        select_script_pane(smudgy, PaneRef { session_id, key })
                    }
                    Some(PaneFollowUp::TearOut { key, width, height }) => {
                        tear_out_script_pane(smudgy, session_id, key, width, height)
                    }
                    Some(PaneFollowUp::Swap {
                        key,
                        other_session,
                        other_key,
                    }) => swap_script_panes(
                        smudgy,
                        PaneRef { session_id, key },
                        PaneRef {
                            session_id: other_session,
                            key: other_key,
                        },
                    ),
                    Some(PaneFollowUp::MirrorInterest) => {
                        // Warm-up: measure everything now and flush without
                        // the debounce, so the first `pane.size` reads see
                        // reality (the store arm above armed the feed).
                        let report = report_pane_sizes(smudgy);
                        if let Some(session) = smudgy.sessions.get_mut(session_id) {
                            session.flush_pane_sizes();
                        }
                        report
                    }
                    Some(PaneFollowUp::LayoutSave(name)) => {
                        script_save_layout(smudgy, session_id, &name);
                        Task::none()
                    }
                    Some(PaneFollowUp::LayoutApply(name)) => {
                        script_apply_layout(smudgy, session_id, &name)
                    }
                    None => Task::none(),
                };
                if presence_edge {
                    refresh_discord_presence(smudgy);
                }
                Task::batch([
                    task,
                    pane_task,
                    scope_task,
                    drain_pending_pane_commands(smudgy),
                ])
            } else {
                // The session was torn down (its store entry goes first) with
                // this event already in flight; dropping the event here is
                // what keeps a dead session from re-entering any grid.
                log::debug!("Dropping event for closed session {session_id}");
                Task::none()
            }
        }
        Message::SessionAction(session_id, msg) => {
            // Store-routed task continuations (notably script input.focus())
            // carry no window wrapper. Reconcile their confirmed focus edge
            // here so they obey the same single-focused-input invariant as a
            // user action routed through SmudgyWindow.
            if let Some((key, focused)) = msg.input_focus_change()
                && smudgy.sessions.note_input_focus(session_id, key, focused)
                && focused
            {
                for window in smudgy.smudgy_windows.values_mut() {
                    if window.hosts_pane(session_id, key) {
                        window.note_session_input_focus(session_id);
                    }
                }
            }

            // The session's own map widgets update below; the standalone map
            // editor windows track the current location too, and a sustained
            // locate streak is the passive bind-on-use signal (daemon-owned).
            let (editor_fan_out, bind_task) =
                if let session_store::Message::SetMapperCurrentLocation(area_id, room_number) = &msg
                {
                    let (area_id, room_number) = (*area_id, *room_number);
                    (
                        Task::done(Message::SetMapperCurrentLocation(area_id, room_number)),
                        observe_locate_for_binding(smudgy, session_id, area_id),
                    )
                } else {
                    (Task::none(), Task::none())
                };
            if let Some(session) = smudgy.sessions.get_mut(session_id) {
                let session_task = session
                    .update(msg)
                    .map(move |msg| Message::SessionAction(session_id, msg));
                Task::batch([session_task, editor_fan_out, bind_task])
            } else {
                log::debug!("Dropping action for closed session {session_id}");
                Task::none()
            }
        }
        Message::CreateSmudgyWindow => {
            let (_, task) = window::open(smudgy_window_settings());
            task.map(Message::NewSmudgyWindow)
        }
        Message::NewSmudgyWindow(id) => {
            // Tear-out inserts its window synchronously (it must adopt the
            // transplanted pane before the open task completes), so this may
            // find the entry already present.
            smudgy.smudgy_windows.entry(id).or_insert_with(|| {
                windows::smudgy_window::SmudgyWindow::new(id, smudgy.account.handles())
            });
            // Mint the window's stable workspace id (creation order is the
            // durable ordinal) and note the change for the mirror.
            smudgy.workspace.register_window(id);
            smudgy.workspace.schedule.mark();
            // QA hook (debug builds only): the first window under
            // SMUDGY_SPIKE_AUTOSESSION=<n> gets n offline sessions and
            // spawns the second (empty) window, so the scripted drag matrix
            // needs no GUI driving. Once-guarded: the second window
            // re-enters this arm.
            #[cfg(not(debug_assertions))]
            let spike_task = Task::none();
            #[cfg(debug_assertions)]
            let autosession_count = spike_autosession_count();
            #[cfg(debug_assertions)]
            let spike_task = if autosession_count > 0
                && !SPIKE_AUTOSESSION_DONE.swap(true, std::sync::atomic::Ordering::Relaxed)
            {
                match spike_autosession_target() {
                    Some((server, profile)) => {
                        log::info!(
                            "[pane-drag] autosession: opening {autosession_count} offline {profile} on {server} in {id:?} + second window"
                        );
                        let window = smudgy
                            .smudgy_windows
                            .get_mut(&id)
                            .expect("window inserted above");
                        let mut tasks = Vec::new();
                        for _ in 0..autosession_count {
                            tasks.push(
                                window
                                    .autosession_open_offline_session(
                                        server.clone(),
                                        profile.clone(),
                                        &mut smudgy.sessions,
                                    )
                                    .map(move |msg| Message::SmudgyWindowMessage(id, msg)),
                            );
                        }
                        let (_, open_second) = window::open(smudgy_window_settings());
                        tasks.push(open_second.map(Message::NewSmudgyWindow));
                        Task::batch(tasks)
                    }
                    None => {
                        log::warn!("[pane-drag] autosession: no server/profile found");
                        Task::none()
                    }
                }
            } else {
                Task::none()
            };
            Task::batch([
                spike_task,
                // Install the Restart Manager shutdown hook on this window's
                // HWND so the installer can close smudgy for an in-place
                // upgrade.
                window::raw_id::<Message>(id).map(move |raw| {
                    // QA forensics (debug builds only): announce the HWND so
                    // the scripted drag matrix and GetCapture logs can be
                    // correlated to iced window ids.
                    #[cfg(debug_assertions)]
                    log::info!("[pane-drag] window {id:?} hwnd={raw:#x}");
                    Message::HookWindowForShutdown(raw)
                }),
                // Seed the tracker: the window's `Opened` event may have
                // fired before the daemon subscription was polled (true for
                // the first window at startup).
                window::position(id).map(move |origin| {
                    Message::WindowTracking(id, pane_drag::TrackEvent::Origin(origin))
                }),
                window::size(id).map(move |size| {
                    Message::WindowTracking(id, pane_drag::TrackEvent::Resized(size))
                }),
                window::scale_factor(id).map(move |scale| {
                    Message::WindowTracking(id, pane_drag::TrackEvent::Rescaled(scale))
                }),
                // Seed the workspace geometry cache too, so a snapshot taken
                // before the first checkpoint poll (an early quit included)
                // still carries real placement for this window.
                window::position(id).map(move |origin| Message::WorkspaceGeometry {
                    poll: None,
                    window: id,
                    sample: workspace::autosave::GeometrySample::Position(origin),
                }),
                window::size(id).map(move |size| Message::WorkspaceGeometry {
                    poll: None,
                    window: id,
                    sample: workspace::autosave::GeometrySample::Size(size),
                }),
                window::scale_factor(id).map(move |scale| Message::WorkspaceGeometry {
                    poll: None,
                    window: id,
                    sample: workspace::autosave::GeometrySample::Scale(scale),
                }),
            ])
        }
        Message::HookWindowForShutdown(raw_id) => {
            win_rm::hook_window(raw_id);
            Task::none()
        }
        Message::AutomationsWindowMessage(id, msg) => {
            if let Some(window) = smudgy.automations_windows.get_mut(&id) {
                let update = window
                    .update(msg)
                    .map_message(move |msg| Message::AutomationsWindowMessage(id, msg));

                match update.event {
                    Some(AutomationsWindowEvent::ScriptsChanged { server_name }) => {
                        let reload_tasks = smudgy
                            .sessions
                            .iter()
                            .filter(|(_, session)| {
                                session.server_name.as_str() == server_name.as_str()
                            })
                            .map(|(session_id, _)| {
                                Task::done(Message::SessionAction(
                                    session_id,
                                    session_store::Message::Reload,
                                ))
                            });

                        Task::batch([update.task, Task::batch(reload_tasks)])
                    }
                    None => update.task,
                }
            } else {
                log::warn!("Received message for unknown window index: {}", id);
                Task::none()
            }
        }
        Message::CreateAutomationsWindow {
            server_name,
            session_id,
        } => {
            let (_, task) = window::open(secondary_window_settings(Size::new(900.0, 560.0)));
            task.map(move |id| Message::NewAutomationsWindow {
                id,
                server_name: server_name.clone(),
                session_id,
            })
        }
        Message::NewAutomationsWindow {
            id,
            server_name,
            session_id,
        } => {
            let window = AutomationsWindow::new(
                id,
                server_name.to_string(),
                smudgy.account.handles(),
                session_id,
            );
            let task = window.init();
            smudgy.automations_windows.insert(id, window);

            task.map(move |message| Message::AutomationsWindowMessage(id, message))
        }
        Message::MapEditorWindowMessage(id, msg) => {
            if let Some(window) = smudgy.map_editor_windows.get_mut(&id) {
                let update = window
                    .update(msg)
                    .map_message(move |msg| Message::MapEditorWindowMessage(id, msg));

                match update.event {
                    Some(map_editor_window::Event::OpenSettings) => {
                        // Land on the Account tab: a reused settings window
                        // may be sitting on another tab, and a fresh one
                        // defaults to Account anyway.
                        let retab = smudgy.settings_windows.keys().next().map(|&id| {
                            Task::done(Message::SettingsWindowMessage(
                                id,
                                settings_window::Message::TabSelected(
                                    settings_window::Tab::Account,
                                ),
                            ))
                        });
                        Task::batch(
                            [
                                Some(update.task),
                                retab,
                                Some(Task::done(Message::CreateSettingsWindow)),
                            ]
                            .into_iter()
                            .flatten(),
                        )
                    }
                    Some(map_editor_window::Event::DisabledAreasChanged(set)) => {
                        // Stamp the areas whose enabled/disabled state actually
                        // flipped with `now`, persist the timestamped prefs +
                        // derived set, fan out to live mappers, and push the
                        // changes to the cloud (last-write-wins).
                        let changed =
                            stamp_area_pref_changes(&mut smudgy.area_prefs, &set, Utc::now());
                        // An explicit toggle un-parks its area: the user may
                        // have just been granted access, and one attempt per
                        // action can't loop.
                        for (area_id, _) in &changed {
                            smudgy.area_prefs_push_parked.remove(area_id);
                        }
                        smudgy.disabled_map_areas = set.clone();
                        persist_area_prefs(&smudgy.area_prefs);
                        apply_disabled_map_areas(smudgy, &set);
                        let push = if smudgy.account.snapshot().signed_in {
                            push_area_prefs_task(smudgy, &changed)
                        } else {
                            Task::none()
                        };
                        Task::batch([update.task, push])
                    }
                    Some(map_editor_window::Event::ScopeAssociationsChanged(deltas)) => {
                        // The editor changed a cloud-map scope association (or
                        // observed new atlases). Replay its targeted deltas
                        // against the authoritative copy rather than adopting a
                        // whole-store snapshot — a concurrent bind / rescue /
                        // homing / other-editor write is thereby preserved
                        // instead of silently erased by stale editor state.
                        for delta in &deltas {
                            smudgy.map_scopes.apply(delta);
                        }
                        // Persist, recompute each server's exclusions and push
                        // them to every live mapper, and mirror the corrected
                        // store back into *every* editor — including the sender,
                        // whose optimistic snapshot the mirror reconciles.
                        let commit = commit_scope_change(smudgy);
                        Task::batch([update.task, commit])
                    }
                    None => update.task,
                }
            } else {
                log::warn!("Received message for unknown window index: {}", id);
                Task::none()
            }
        }
        Message::CreateMapEditorWindow {
            mapper,
            server_name,
        } => {
            let (_, task) = window::open(secondary_window_settings(Size::new(600.0, 400.0)));
            task.map(move |id| Message::NewMapEditorWindow {
                id,
                mapper: mapper.clone(),
                server_name: server_name.clone(),
            })
        }
        Message::NewMapEditorWindow {
            id,
            mapper,
            server_name,
        } => {
            // CloudHandles are app-global, so they're attached here at
            // construction (like SettingsWindow) rather than threaded through
            // the per-session event payload the way the mapper is.
            //
            // Apply the user's disabled-area preferences and this server's
            // cloud-map scope to the window's mapper up front (the editor may
            // outlive its originating pane; both setters are idempotent), and
            // hand it the app-global clipboard so all editor windows share one
            // (merge workflow) plus a snapshot of the scope associations.
            mapper.set_disabled_areas(smudgy.disabled_map_areas.clone());
            mapper.set_scope_exclusions(
                smudgy.map_scopes.excluded_atlases(&server_name),
                smudgy.map_scopes.excluded_areas(&server_name),
            );
            let window = MapEditorWindow::with_clipboard(
                id,
                mapper,
                smudgy.account.handles(),
                smudgy.map_editor_clipboard.clone(),
                (*server_name).clone(),
                smudgy.map_scopes.clone(),
            );
            smudgy.map_editor_windows.insert(id, window);
            Task::none()
        }
        Message::CreateSettingsWindow => {
            // Reuse an existing settings window rather than stacking copies.
            if let Some((&id, _)) = smudgy.settings_windows.iter().next() {
                window::gain_focus(id)
            } else {
                let (_, task) = window::open(secondary_window_settings(Size::new(640.0, 480.0)));
                task.map(Message::NewSettingsWindow)
            }
        }
        Message::NewSettingsWindow(id) => {
            smudgy
                .settings_windows
                .insert(id, SettingsWindow::new(smudgy.account.handles()));
            Task::none()
        }
        Message::SettingsWindowMessage(id, msg) => {
            if let Some(window) = smudgy.settings_windows.get_mut(&id) {
                let update = window
                    .update(msg)
                    .map_message(move |msg| Message::SettingsWindowMessage(id, msg));

                let event_task = match update.event {
                    Some(SettingsWindowEvent::SessionEstablished(session)) => {
                        let task = smudgy
                            .account
                            .establish_session(*session)
                            .map(Message::Account);
                        poke_all_mappers(smudgy);
                        // Now signed in: reconcile area prefs against the cloud.
                        // A fresh session can carry fresh grants, so parked
                        // pushes get another attempt.
                        smudgy.area_prefs_push_parked.clear();
                        Task::batch([task, reconcile_area_prefs(smudgy)])
                    }
                    Some(SettingsWindowEvent::SignOut { everywhere }) => {
                        let task = smudgy.account.sign_out(everywhere).map(Message::Account);
                        poke_all_mappers(smudgy);
                        task
                    }
                    Some(SettingsWindowEvent::ProfileUpdated(profile)) => {
                        smudgy.account.absorb_profile(*profile);
                        Task::none()
                    }
                    Some(SettingsWindowEvent::Poke) => smudgy.account.poke().map(Message::Account),
                    Some(SettingsWindowEvent::SettingsChanged(settings)) => {
                        let mut settings = *settings;
                        // The settings window never edits the area prefs; its
                        // copy may be stale (read before a map-editor toggle or
                        // a cloud reconcile). Keep the authoritative timestamped
                        // prefs *and* their derived disabled list so saving the
                        // settings form doesn't clobber either.
                        let mut prefs: Vec<MapAreaPref> =
                            smudgy.area_prefs.values().cloned().collect();
                        prefs.sort_by_key(|pref| pref.area_id.0);
                        let mut areas: Vec<AreaId> =
                            smudgy.disabled_map_areas.iter().copied().collect();
                        areas.sort_by_key(|id| id.0);
                        settings.map_area_prefs = prefs;
                        settings.disabled_map_areas = areas;
                        if let Err(err) = smudgy_core::models::settings::save_settings(&settings) {
                            log::warn!("failed to save settings: {err}");
                        }
                        // Keep the account controller's master switch in step so
                        // the soft upgrade prompt and the periodic check follow
                        // the toggle immediately (off clears the prompt now).
                        smudgy
                            .account
                            .set_auto_check_for_updates(settings.auto_check_for_updates);
                        // Same for the Discord toggle: enabling mid-session
                        // publishes the current game at once, disabling clears
                        // the activity from the user's profile.
                        smudgy.discord.set_enabled(settings.discord_rich_presence);
                        refresh_discord_presence(smudgy);
                        // Swap the hot prefs snapshot (fonts/palette/line
                        // length take effect next frame) and fan the change
                        // out to every live session (scrollback, span
                        // restyle, runtime separator/prefix/logging).
                        prefs::apply(&settings);
                        let fan_out: Vec<Task<Message>> = smudgy
                            .sessions
                            .iter()
                            .map(|(session_id, _)| {
                                Task::done(Message::SessionAction(
                                    session_id,
                                    session_store::Message::ApplySettings(settings.clone()),
                                ))
                            })
                            .collect();
                        Task::batch(fan_out)
                    }
                    None => Task::none(),
                };

                Task::batch([update.task, event_task])
            } else {
                log::warn!("Received message for unknown window index: {}", id);
                Task::none()
            }
        }
        Message::SetMapperCurrentLocation(area_id, room_number) => {
            // SetCurrentLocation yields only a repaint task (no Event) and only
            // when the marker actually moved; route those back so the editor
            // repaints promptly instead of on the next incidental redraw.
            let tasks: Vec<Task<Message>> = smudgy
                .map_editor_windows
                .iter_mut()
                .map(|(id, window)| {
                    let id = *id;
                    window
                        .update(map_editor_window::Message::SetCurrentLocation(
                            area_id,
                            room_number,
                        ))
                        .map_message(move |msg| Message::MapEditorWindowMessage(id, msg))
                        .task
                })
                .collect();
            Task::batch(tasks)
        }
        Message::SessionRefreshTick => smudgy.account.refresh_session().map(Message::Account),
        Message::UpdateCheckTick => smudgy.account.check_for_updates().map(Message::Account),
        Message::AreaPrefsReconcileTick => reconcile_area_prefs(smudgy),
        Message::AreaPrefsFetched(result) => {
            let server = match result {
                Ok(server) => server,
                Err(err) => {
                    // Offline or server trouble: keep the local set as-is.
                    log::warn!("area-prefs fetch failed: {err}");
                    return Task::none();
                }
            };
            let pushes = merge_server_area_prefs(
                &mut smudgy.area_prefs,
                &server,
                &smudgy.area_prefs_push_parked,
            );
            apply_and_persist_area_prefs(smudgy);
            push_area_prefs_task(smudgy, &pushes)
        }
        Message::AreaPrefPushed { area_id, result } => {
            match result {
                Ok(row) => {
                    // Adopt the server-stamped row so later LWW comparisons use
                    // the server clock. The value is what we pushed, so the
                    // derived disabled set is unchanged — just re-persist.
                    smudgy.area_prefs.insert(
                        area_id,
                        MapAreaPref {
                            area_id,
                            disabled: row.disabled,
                            updated_at: row.updated_at,
                        },
                    );
                    persist_area_prefs(&smudgy.area_prefs);
                }
                Err(CloudError::NotFoundOrNoAccess) => {
                    // The area isn't viewable (a local-tier map, or access was
                    // lost): the pref can't sync. Leave it local — a residual
                    // pref for a vanished area matches nothing and is harmless
                    // — but PARK it so the 90s reconcile stops re-attempting a
                    // push the server will keep refusing. A user toggle or a
                    // fresh sign-in un-parks it.
                    smudgy.area_prefs_push_parked.insert(area_id);
                    log::debug!(
                        "area-prefs push for {area_id} returned 404; kept local pref, parked until user action or sign-in"
                    );
                }
                Err(err) => log::warn!("area-prefs push for {area_id} failed: {err}"),
            }
            Task::none()
        }
    }
}

/// Close one session (the user's explicit ✕): shut its runtime down and
/// remove it from the store *first* — so events still in flight for the id
/// are dropped at the daemon — then **vacate** its slot rather than delete
/// it: the session's tabs stay in place as unbound placeholders (geometry
/// retained for the run) and the window hosting its main pane records the
/// vacancy a later open there adopts. A repeat close (double-clicked ✕, a
/// late queued task) is a no-op. Vacancies are runtime-only: the next
/// snapshot simply omits the unbound tabs, so closed stays closed across
/// restarts by omission.
fn close_session(smudgy: &mut Smudgy, session_id: SessionId) -> Task<Message> {
    // The vacancy's descriptors must be captured while the store entry (the
    // only holder of the session's pane defs) still exists.
    let vacate = smudgy.sessions.get(session_id).map(|session| {
        let mut descriptors = HashMap::new();
        for window in smudgy.smudgy_windows.values() {
            for slot in window.pane_refs() {
                if slot.session_id != session_id || slot.key == MAIN_PANE_KEY {
                    continue;
                }
                if let Some(def) = session.pane_def(slot.key) {
                    descriptors.insert(
                        slot.key,
                        workspace::restore::descriptor_key(&def.namespace, &def.name),
                    );
                }
            }
        }
        (
            session.server_name.clone(),
            session.profile_name.clone(),
            descriptors,
        )
    });
    if !smudgy.sessions.shutdown_and_remove(session_id) {
        return Task::none();
    }
    forget_session_pane_commands(smudgy, session_id);
    log::info!("Closed session {session_id}");
    smudgy.restore.forget_session(session_id);
    smudgy.workspace.forget_session(session_id);
    // A session closed while still connected never sees a Disconnected event.
    refresh_discord_presence(smudgy);
    let Some((server, profile, descriptors)) = vacate else {
        // Already gone: nothing to vacate, just sweep like a cascade close.
        return purge_sessions_from_windows(smudgy, &[session_id]);
    };
    // The dying panes cancel gestures exactly like a purge would.
    if smudgy
        .tab_drag
        .as_ref()
        .is_some_and(|drag| drag.slot.session_id == session_id)
    {
        cancel_tab_drag(smudgy, "session closed mid-drag");
    }
    if smudgy
        .tab_press
        .is_some_and(|press| press.slot.session_id == session_id)
    {
        smudgy.tab_press = None;
    }
    let ordinal = smudgy.restore.next_vacancy_ordinal();
    let mut emptied: Vec<window::Id> = Vec::new();
    for (window_id, window) in smudgy.smudgy_windows.iter_mut() {
        let vacated_empty =
            window.vacate_session(session_id, &server, &profile, &descriptors, ordinal);
        // Placeholders the dead session still owed (a restore it never
        // finished) can no longer materialize.
        let reaped_empty = window.reap_session_placeholders(session_id);
        if vacated_empty || reaped_empty {
            emptied.push(*window_id);
        }
    }
    // A window this emptied hosted nothing but the dead session's doomed
    // tabs (torn-out script panes whose main lived elsewhere, or
    // placeholders it still owed): it closes like any other emptied window,
    // keep-one-alive included.
    close_emptied_windows(smudgy, emptied)
}

/// Re-derives the Discord presence from the session store and hands it to
/// the controller, which change-gates (and no-ops while the setting is
/// off). The longest-connected session provides the label; an empty store
/// publishes `Idle`, keeping the activity up for the app's whole run.
fn refresh_discord_presence(smudgy: &mut Smudgy) {
    let primary = smudgy
        .sessions
        .iter()
        .filter_map(|(_, session)| {
            session.connected_at_unix_ms().map(|at| {
                let label =
                    discord_presence::server_label(&session.server_host(), &session.server_name);
                (label, at)
            })
        })
        .min_by_key(|(_, at)| *at);
    let presence = primary.map_or(discord_presence::Presence::Idle, |(server_label, at)| {
        discord_presence::Presence::Playing {
            server_label,
            connected_at_ms: at,
        }
    });
    smudgy.discord.publish(presence);
}

/// Remove the dead sessions' panes from every window's grid, repairing each
/// window's active-session state, then close any window the purge emptied —
/// always keeping at least one smudgy window alive (the last one stays open
/// showing the empty connect state).
fn purge_sessions_from_windows(smudgy: &mut Smudgy, dead: &[SessionId]) -> Task<Message> {
    // A dragged pane whose session died mid-drag must never drop, and a
    // pressed one must never promote.
    if smudgy
        .tab_drag
        .as_ref()
        .is_some_and(|drag| dead.contains(&drag.slot.session_id))
    {
        cancel_tab_drag(smudgy, "session closed mid-drag");
    }
    if smudgy
        .tab_press
        .is_some_and(|press| dead.contains(&press.slot.session_id))
    {
        smudgy.tab_press = None;
    }

    let mut tasks: Vec<Task<Message>> = Vec::new();
    let mut emptied: Vec<window::Id> = Vec::new();

    for &session_id in dead {
        smudgy.restore.forget_session(session_id);
        smudgy.workspace.forget_session(session_id);
    }
    for (window_id, window) in smudgy.smudgy_windows.iter_mut() {
        for &session_id in dead {
            let (task, now_empty) = window.handle_session_removed(session_id, &smudgy.sessions);
            let window_id = *window_id;
            tasks.push(task.map(move |msg| Message::SmudgyWindowMessage(window_id, msg)));
            if now_empty {
                emptied.push(window_id);
            }
        }
    }

    tasks.push(close_emptied_windows(smudgy, emptied));
    Task::batch(tasks)
}

/// Close each emptied window, always keeping at least one smudgy window
/// alive (the last one stays open showing the empty connect state).
///
/// "Emptied" is visual: callers report windows left with no bound or
/// pending tab, which includes windows still holding invisible vacancy
/// tabs. Closing such a secondary window drops its vacancy records —
/// acceptable by design, since adoption is window-local and a record in a
/// closed window could never be adopted again. The kept-alive last window
/// retains its vacancies invisibly behind the connect view, where a later
/// open adopts them.
///
/// "Remaining" excludes windows already told to close but still lingering in
/// the map (their `CloseWindow` event is in flight): counting them would let
/// two independently-emptied windows each decide another survives, close both,
/// and exit the app.
fn close_emptied_windows(smudgy: &mut Smudgy, emptied: Vec<window::Id>) -> Task<Message> {
    let mut tasks: Vec<Task<Message>> = Vec::new();
    let mut remaining = smudgy
        .smudgy_windows
        .keys()
        .filter(|id| !smudgy.closing_windows.contains(id))
        .count();
    for window_id in emptied {
        // Already scheduled to close (e.g. emptied twice in one sweep): skip.
        if smudgy.closing_windows.contains(&window_id) {
            continue;
        }
        if remaining > 1 {
            remaining -= 1;
            smudgy.closing_windows.insert(window_id);
            tasks.push(window::close(window_id));
        }
    }
    Task::batch(tasks)
}

/// Accept one command from the shared runtime -> daemon sequencer. Channel
/// order is authoritative; the per-origin stamp is an assertion/diagnostic,
/// not a second ordering mechanism.
fn handle_ui_command(smudgy: &mut Smudgy, envelope: UiCommandEnvelope) -> Task<Message> {
    let expected = smudgy
        .last_ui_command_seq
        .entry(envelope.origin)
        .or_insert(0);
    if envelope.origin_seq != *expected {
        log::warn!(
            "UI command sequence gap for {}: expected {}, received {}",
            envelope.origin,
            *expected,
            envelope.origin_seq
        );
    }
    *expected = (*expected).max(envelope.origin_seq.saturating_add(1));

    match envelope.command {
        UiCommand::Pane(command) => queue_pane_command(smudgy, command),
    }
}

fn pane_command_dependencies(command: &PaneCommand) -> Vec<PaneRef> {
    match command {
        // Open creates readiness; Close terminates it and is valid even if a
        // stale layout no longer hosts the pane.
        PaneCommand::Open { .. } | PaneCommand::Close { .. } => Vec::new(),
        PaneCommand::Resize {
            session_id, key, ..
        }
        | PaneCommand::Select { session_id, key }
        | PaneCommand::TearOut {
            session_id, key, ..
        } => vec![PaneRef {
            session_id: *session_id,
            key: *key,
        }],
        PaneCommand::Relocate {
            session_id,
            key,
            reference,
            ..
        } => vec![
            PaneRef {
                session_id: *session_id,
                key: *key,
            },
            PaneRef {
                session_id: *session_id,
                key: *reference,
            },
        ],
        PaneCommand::GroupWith {
            session_id,
            key,
            reference_session,
            reference,
            ..
        } => vec![
            PaneRef {
                session_id: *session_id,
                key: *key,
            },
            PaneRef {
                session_id: *reference_session,
                key: *reference,
            },
        ],
        PaneCommand::Swap {
            session_id,
            key,
            other_session,
            other_key,
        } => vec![
            PaneRef {
                session_id: *session_id,
                key: *key,
            },
            PaneRef {
                session_id: *other_session,
                key: *other_key,
            },
        ],
    }
}

fn pane_is_hosted(smudgy: &Smudgy, pane: PaneRef) -> bool {
    smudgy
        .smudgy_windows
        .values()
        .any(|window| window.hosts_pane(pane.session_id, pane.key))
}

fn pane_command_ready(smudgy: &Smudgy, command: &PaneCommand) -> bool {
    pane_command_dependencies(command)
        .into_iter()
        .all(|pane| pane_is_hosted(smudgy, pane))
}

fn pane_command_retired(smudgy: &Smudgy, command: &PaneCommand) -> bool {
    let lifecycle_pane = match command {
        PaneCommand::Open {
            session_id, def, ..
        } => Some(PaneRef {
            session_id: *session_id,
            key: def.key,
        }),
        PaneCommand::Close { session_id, key } => Some(PaneRef {
            session_id: *session_id,
            key: *key,
        }),
        _ => None,
    };
    if lifecycle_pane.is_some_and(|pane| smudgy.retired_panes.contains(&pane)) {
        return true;
    }
    pane_command_dependencies(command)
        .into_iter()
        .any(|pane| smudgy.retired_panes.contains(&pane))
}

fn pane_command_has_closed_session(smudgy: &Smudgy, command: &PaneCommand) -> bool {
    let closed = |session_id| smudgy.sessions.get(session_id).is_none();
    match command {
        PaneCommand::Open { session_id, .. }
        | PaneCommand::Close { session_id, .. }
        | PaneCommand::Resize { session_id, .. }
        | PaneCommand::Relocate { session_id, .. }
        | PaneCommand::Select { session_id, .. }
        | PaneCommand::TearOut { session_id, .. } => closed(*session_id),
        PaneCommand::GroupWith {
            session_id,
            reference_session,
            ..
        } => closed(*session_id) || closed(*reference_session),
        PaneCommand::Swap {
            session_id,
            other_session,
            ..
        } => closed(*session_id) || closed(*other_session),
    }
}

/// Preserve bus order for commands that unexpectedly arrive before a pane is
/// hosted. Lifecycle edges are allowed through: Open makes a pane ready and
/// Close retires any stale commands waiting on it. Under the registry-lock
/// publication invariant this queue is normally empty; keeping it makes a
/// missing host recoverable instead of a permanent warn-and-drop.
fn queue_pane_command(smudgy: &mut Smudgy, command: PaneCommand) -> Task<Message> {
    if pane_command_has_closed_session(smudgy, &command) {
        log::debug!("Dropping UI pane command for a closed session");
        return Task::none();
    }
    if pane_command_retired(smudgy, &command) {
        log::warn!("Dropping UI pane command that references a retired pane");
        return Task::none();
    }

    let is_lifecycle_edge = matches!(
        command,
        PaneCommand::Open { .. } | PaneCommand::Close { .. }
    );
    let mut tasks = Vec::new();
    if is_lifecycle_edge
        || (smudgy.pending_pane_commands.is_empty() && pane_command_ready(smudgy, &command))
    {
        tasks.push(apply_pane_command(smudgy, command));
    } else {
        smudgy.pending_pane_commands.push_back(command);
    }
    tasks.push(drain_pending_pane_commands(smudgy));
    Task::batch(tasks)
}

fn drain_pending_pane_commands(smudgy: &mut Smudgy) -> Task<Message> {
    let mut tasks = Vec::new();
    while let Some(front) = smudgy.pending_pane_commands.front() {
        if pane_command_has_closed_session(smudgy, front) {
            smudgy.pending_pane_commands.pop_front();
            continue;
        }
        if pane_command_retired(smudgy, front) {
            smudgy.pending_pane_commands.pop_front();
            continue;
        }
        if !pane_command_ready(smudgy, front) {
            break;
        }
        let command = smudgy
            .pending_pane_commands
            .pop_front()
            .expect("front checked above");
        tasks.push(apply_pane_command(smudgy, command));
    }
    Task::batch(tasks)
}

fn retire_pane_commands(smudgy: &mut Smudgy, pane: PaneRef) {
    smudgy.retired_panes.insert(pane);
    smudgy.pending_pane_commands.retain(|command| {
        !pane_command_dependencies(command)
            .into_iter()
            .any(|dependency| dependency == pane)
    });
}

fn pane_command_mentions_session(command: &PaneCommand, session_id: SessionId) -> bool {
    match command {
        PaneCommand::Open {
            session_id: target, ..
        }
        | PaneCommand::Resize {
            session_id: target, ..
        }
        | PaneCommand::Close {
            session_id: target, ..
        }
        | PaneCommand::Relocate {
            session_id: target, ..
        }
        | PaneCommand::Select {
            session_id: target, ..
        }
        | PaneCommand::TearOut {
            session_id: target, ..
        } => *target == session_id,
        PaneCommand::GroupWith {
            session_id: target,
            reference_session,
            ..
        } => *target == session_id || *reference_session == session_id,
        PaneCommand::Swap {
            session_id: target,
            other_session,
            ..
        } => *target == session_id || *other_session == session_id,
    }
}

fn forget_session_pane_commands(smudgy: &mut Smudgy, session_id: SessionId) {
    smudgy
        .pending_pane_commands
        .retain(|command| !pane_command_mentions_session(command, session_id));
    smudgy
        .retired_panes
        .retain(|pane| pane.session_id != session_id);
    smudgy
        .pending_ordered_pane_closes
        .retain(|pane| pane.session_id != session_id);
    smudgy.last_ui_command_seq.remove(&session_id);
}

fn apply_pane_command(smudgy: &mut Smudgy, command: PaneCommand) -> Task<Message> {
    match command {
        PaneCommand::Open {
            session_id,
            def,
            placement,
        } => {
            let key = def.key;
            let hidden = def.hidden;
            let Some(session) = smudgy.sessions.get_mut(session_id) else {
                log::debug!("Dropping pane Open for closed session {session_id}");
                return Task::none();
            };
            // The owning session event repeats this materialization ahead of
            // AppendTo; `open_pane` is deliberately idempotent by key.
            session.open_pane(def);
            let placed = place_pane_in_windows(smudgy, session_id, key, placement);
            if hidden {
                sync_pane_hidden(smudgy, PaneRef { session_id, key }, true);
            }
            let select = if placed && matches!(placement, PanePlacement::Tab { selected: true, .. })
            {
                select_script_pane(smudgy, PaneRef { session_id, key })
            } else {
                Task::none()
            };
            Task::batch([select, report_pane_sizes(smudgy)])
        }
        PaneCommand::Close { session_id, key } => {
            let pane = PaneRef { session_id, key };
            retire_pane_commands(smudgy, pane);
            let remove = remove_pane_from_windows(smudgy, session_id, key);
            let retire_display = if smudgy.pending_ordered_pane_closes.remove(&pane) {
                if let Some(session) = smudgy.sessions.get_mut(session_id) {
                    session
                        .update(session_store::Message::SessionEvent(
                            SessionEvent::PaneClosedOrdered(key),
                        ))
                        .map(move |msg| Message::SessionAction(session_id, msg))
                } else {
                    Task::none()
                }
            } else {
                Task::none()
            };
            Task::batch([remove, retire_display])
        }
        PaneCommand::Resize {
            session_id,
            key,
            width,
            height,
        } => {
            let slot = PaneRef { session_id, key };
            for window in smudgy.smudgy_windows.values_mut() {
                if window.hosts_pane(session_id, key) {
                    window.resize_pane_slot(slot, width, height);
                }
            }
            report_pane_sizes(smudgy)
        }
        PaneCommand::Relocate {
            session_id,
            key,
            reference,
            direction,
            size_px,
        } => relocate_script_pane(smudgy, session_id, key, reference, direction, size_px),
        PaneCommand::GroupWith {
            session_id,
            key,
            reference_session,
            reference,
            position,
            selected,
        } => group_script_pane(
            smudgy,
            PaneRef { session_id, key },
            PaneRef {
                session_id: reference_session,
                key: reference,
            },
            position,
            selected,
        ),
        PaneCommand::Select { session_id, key } => {
            select_script_pane(smudgy, PaneRef { session_id, key })
        }
        PaneCommand::TearOut {
            session_id,
            key,
            width,
            height,
        } => tear_out_script_pane(smudgy, session_id, key, width, height),
        PaneCommand::Swap {
            session_id,
            key,
            other_session,
            other_key,
        } => swap_script_panes(
            smudgy,
            PaneRef { session_id, key },
            PaneRef {
                session_id: other_session,
                key: other_key,
            },
        ),
    }
}

/// Place a freshly opened script pane into the window hosting its reference
/// pane — falling back to the window hosting the session's main pane, then
/// any window. (A script splitting against a pane whose window vanished
/// mid-flight lands next to the main pane.)
/// The daemon's half of a pane session event — captured by value before the
/// event is forwarded into the session store, then applied to the windows.
enum PaneFollowUp {
    Opened {
        key: PaneKey,
        placement: PanePlacement,
        hidden: bool,
    },
    Closed(PaneKey),
    DefSync {
        key: PaneKey,
        hidden: bool,
    },
    Resize {
        key: PaneKey,
        width: Option<f32>,
        height: Option<f32>,
    },
    Relocate {
        key: PaneKey,
        reference: PaneKey,
        direction: smudgy_core::session::runtime::pane::SplitDirection,
        size_px: Option<f32>,
    },
    GroupWith {
        key: PaneKey,
        reference_session: SessionId,
        reference: PaneKey,
        position: TabPosition,
        selected: bool,
    },
    Select {
        key: PaneKey,
    },
    TearOut {
        key: PaneKey,
        width: Option<f32>,
        height: Option<f32>,
    },
    Swap {
        key: PaneKey,
        other_session: SessionId,
        other_key: PaneKey,
    },
    MirrorInterest,
    /// `layout.save` — capture the calling session's server footprint and
    /// write its store, on the daemon (the only owner of the live model).
    LayoutSave(String),
    /// `layout.apply` — the script-scoped, layout-only apply.
    LayoutApply(String),
}

/// The daemon half of a script `layout.save`: capture the calling
/// session's server footprint and queue it for the store. The capture and
/// serialization happen here, synchronously — the snapshot must be
/// consistent with this cycle's model — while the fsync-bearing atomic
/// write coalesces on the background saver, so a save in a per-line
/// trigger costs the update thread a serialization, never a disk write.
fn script_save_layout(smudgy: &mut Smudgy, session_id: SessionId, name: &str) {
    let Some(server) = smudgy
        .sessions
        .get(session_id)
        .map(|session| session.server_name.clone())
    else {
        return;
    };
    let Some(dir) = workspace::layouts::layouts_dir(&server) else {
        log::warn!("[layouts] script save of '{name}' skipped: no resolvable store for {server}");
        return;
    };
    match capture_server_footprint(smudgy, &server) {
        Some((template, notes)) => match smudgy.layout_saver.submit(dir, name, &template) {
            Ok(()) => {
                if notes.is_partial() {
                    log::info!(
                        "[layouts] script save of '{name}' for {server} was partial: \
                         {} vacancy tab(s), {} foreign pane(s) omitted",
                        notes.omitted_vacancies,
                        notes.omitted_foreign
                    );
                }
            }
            Err(error) => {
                log::warn!("[layouts] script save of '{name}' for {server} failed: {error}");
            }
        },
        None => {
            log::info!("[layouts] script save of '{name}' skipped: no window hosts a {server} pane")
        }
    }
}

/// The daemon half of a script `layout.apply`: project under Script
/// scoping — content-scoped to the calling session's server, layout-only —
/// revalidate conservation against the live workspace, and execute. Never
/// spawns, closes, prompts, or touches OS windows; rapid repeated applies
/// are model mutations only, coalescing into the normal autosave debounce.
fn script_apply_layout(smudgy: &mut Smudgy, session_id: SessionId, name: &str) -> Task<Message> {
    if smudgy.workspace.schedule.is_shutting_down() {
        return Task::none();
    }
    let Some(server) = smudgy
        .sessions
        .get(session_id)
        .map(|session| session.server_name.clone())
    else {
        return Task::none();
    };
    let template = match workspace::layouts::load(&server, name) {
        Ok(template) => template,
        Err(error) => {
            log::info!("[layouts] script apply of '{name}' for {server} failed: {error}");
            return Task::none();
        }
    };
    let live = build_live_workspace(smudgy);
    let mode = workspace::apply::ApplyMode::Script {
        calling_server: &server,
    };
    let plan = match workspace::apply::plan_apply(&template, &live, mode, &HashMap::new()) {
        Ok(plan) => plan,
        Err(error) => {
            log::info!("[layouts] script apply of '{name}' for {server} failed: {error}");
            return Task::none();
        }
    };
    if let Err(error) = workspace::apply::validate_conservation(&template, &live, mode, &plan) {
        log::info!("[layouts] conservation check refused script apply of '{name}': {error}");
        return Task::none();
    }
    log::info!("[layouts] script apply of '{name}' for {server}");
    execute_layout_apply(smudgy, &plan)
}

/// Sync one pane's def-owned hidden state into whichever window hosts it —
/// idempotent for the window whose own eyeball click originated the change.
fn sync_pane_hidden(smudgy: &mut Smudgy, slot: PaneRef, hidden: bool) {
    for window in smudgy.smudgy_windows.values_mut() {
        if window.hosts_pane(slot.session_id, slot.key) {
            window.set_pane_hidden(slot, hidden);
        }
    }
}

/// Feed the pane-size mirror: measure every rendered slot in every smudgy
/// window and report it to its session's feed (change-gated; a no-op for
/// sessions without mirror interest). Sessions that gained pending entries
/// get one trailing flush scheduled — the debounce that turns divider-drag
/// streams into settled reports. Settles each window's pending grid
/// rebuild first: the mutations of the operation being reported on have
/// already landed, so the measurements must come from the grid they
/// produced.
fn report_pane_sizes(smudgy: &mut Smudgy) -> Task<Message> {
    let measured: Vec<(PaneRef, Size)> = smudgy
        .smudgy_windows
        .values_mut()
        .flat_map(|window| {
            window.flush_grid_rebuild();
            window.pane_sizes()
        })
        .collect();
    let mut flushes = Vec::new();
    for (slot, size) in measured {
        let Some(session) = smudgy.sessions.get_mut(slot.session_id) else {
            continue;
        };
        if !session.pane_size_interest() {
            continue;
        }
        if session.report_pane_size(slot.key, size.width, size.height) {
            let session_id = slot.session_id;
            flushes.push(Task::perform(
                async move {
                    tokio::time::sleep(std::time::Duration::from_millis(150)).await;
                },
                move |()| Message::FlushPaneSizes(session_id),
            ));
        }
    }
    Task::batch(flushes)
}

/// Select a pane from script without requesting input focus. Durable tab
/// selection and the hosting window's active session still follow the pane.
fn select_script_pane(smudgy: &mut Smudgy, slot: PaneRef) -> Task<Message> {
    let Some(window_id) = smudgy
        .smudgy_windows
        .iter()
        .find_map(|(id, window)| window.hosts_pane(slot.session_id, slot.key).then_some(*id))
    else {
        log::warn!(
            "No window hosts {} for session {}; dropping select",
            slot.key,
            slot.session_id
        );
        return Task::none();
    };
    let select = smudgy
        .smudgy_windows
        .get_mut(&window_id)
        .map_or_else(Task::none, |window| {
            window
                .select_pane_without_focus(slot, &mut smudgy.sessions)
                .map(move |message| Message::SmudgyWindowMessage(window_id, message))
        });
    Task::batch([select, report_pane_sizes(smudgy)])
}

/// Move one pane into the reference pane's current tab group, including main
/// panes and cross-window/cross-session pairs. Selection is opt-in and never
/// requests keyboard focus.
fn group_script_pane(
    smudgy: &mut Smudgy,
    slot: PaneRef,
    reference: PaneRef,
    position: TabPosition,
    selected: bool,
) -> Task<Message> {
    if slot == reference {
        return Task::none();
    }
    let source_id = smudgy
        .smudgy_windows
        .iter()
        .find_map(|(id, window)| window.hosts_pane(slot.session_id, slot.key).then_some(*id));
    let target_id = smudgy.smudgy_windows.iter().find_map(|(id, window)| {
        window
            .hosts_pane(reference.session_id, reference.key)
            .then_some(*id)
    });
    let (Some(source_id), Some(target_id)) = (source_id, target_id) else {
        log::warn!("Dropping groupWith because one of its panes is no longer hosted");
        return Task::none();
    };

    if source_id == target_id {
        let moved = smudgy
            .smudgy_windows
            .get_mut(&source_id)
            .is_some_and(|window| window.group_pane_with(slot, reference, position));
        if !moved {
            return Task::none();
        }
        let select = if selected {
            select_script_pane(smudgy, slot)
        } else {
            Task::none()
        };
        return Task::batch([select, report_pane_sizes(smudgy)]);
    }

    let Some((target_group, insertion_slot)) = smudgy
        .smudgy_windows
        .get(&target_id)
        .and_then(|window| window.tab_merge_target(reference, position))
    else {
        return Task::none();
    };
    let Some((tab, hidden, emptied)) = smudgy
        .smudgy_windows
        .get_mut(&source_id)
        .and_then(|source| source.extract_pane_tab(slot))
    else {
        return Task::none();
    };
    if let Some(source) = smudgy.smudgy_windows.get_mut(&source_id) {
        source.repair_active_session_without_focus();
    }
    if let Some(target) = smudgy.smudgy_windows.get_mut(&target_id) {
        target.adopt_drag_tab_merge(tab, target_group, insertion_slot);
        target.set_pane_hidden(slot, hidden);
    } else {
        debug_assert!(false, "groupWith destination vanished during one update");
        if let Some(source) = smudgy.smudgy_windows.get_mut(&source_id) {
            source.adopt_torn_out_tab(tab);
            source.set_pane_hidden(slot, hidden);
        }
        return report_pane_sizes(smudgy);
    }

    let select = if selected {
        select_script_pane(smudgy, slot)
    } else {
        Task::none()
    };
    let close = if emptied {
        close_emptied_windows(smudgy, vec![source_id])
    } else {
        Task::none()
    };
    Task::batch([select, close, report_pane_sizes(smudgy)])
}

/// Apply a script `pane.relocate` (panes.md placement commands): detach the
/// pane's tab from whichever window holds it — one tab, preserving the rest
/// of any group it sat in — and re-attach it as a singleton group split
/// beside the reference's WHOLE group, riding the transplant machinery when
/// the reference lives in another window. The tab value carries its stable
/// id across the move, so a same-window relocation keeps the pane's keyed
/// widget state, exactly like the equivalent body-edge drop. The hidden
/// toggle travels with the pane; unlike a user drop, focus does not.
fn relocate_script_pane(
    smudgy: &mut Smudgy,
    session_id: SessionId,
    key: PaneKey,
    reference: PaneKey,
    direction: smudgy_core::session::runtime::pane::SplitDirection,
    size_px: Option<f32>,
) -> Task<Message> {
    let slot = PaneRef { session_id, key };
    let ref_slot = PaneRef {
        session_id,
        key: reference,
    };
    let source_id = smudgy
        .smudgy_windows
        .iter()
        .find_map(|(id, window)| window.hosts_pane(session_id, key).then_some(*id));
    let target_id = smudgy
        .smudgy_windows
        .iter()
        .find_map(|(id, window)| window.hosts_pane(session_id, reference).then_some(*id))
        .or(source_id);
    let (Some(source_id), Some(target_id)) = (source_id, target_id) else {
        log::warn!("No window hosts {key} for session {session_id}; dropping relocate");
        return Task::none();
    };
    if source_id == target_id {
        if let Some(window) = smudgy.smudgy_windows.get_mut(&source_id) {
            // The re-attach lands in this same window unconditionally (the
            // placement chain ends at a fresh cluster), so a transiently
            // emptied model needs no empty-window handling here.
            if let Some((tab, hidden, _emptied)) = window.extract_pane_tab(slot) {
                window.adopt_tab_beside(tab, ref_slot, direction, size_px);
                window.set_pane_hidden(slot, hidden);
            }
        }
        return report_pane_sizes(smudgy);
    }
    let Some(source) = smudgy.smudgy_windows.get_mut(&source_id) else {
        return Task::none();
    };
    let Some((tab, hidden, emptied)) = source.extract_pane_tab(slot) else {
        return Task::none();
    };
    let repair = source
        .repair_active_session(&smudgy.sessions)
        .map(move |msg| Message::SmudgyWindowMessage(source_id, msg));
    let landed_in_target = match smudgy.smudgy_windows.get_mut(&target_id) {
        Some(target) => {
            target.adopt_tab_beside(tab, ref_slot, direction, size_px);
            target.set_pane_hidden(slot, hidden);
            true
        }
        None => {
            // Unreachable (both windows were resolved above in this same
            // update); re-host in the source rather than strand the tab.
            if let Some(source) = smudgy.smudgy_windows.get_mut(&source_id) {
                source.adopt_tab_beside(tab, ref_slot, direction, size_px);
                source.set_pane_hidden(slot, hidden);
            }
            false
        }
    };
    let close = if emptied && landed_in_target {
        close_emptied_windows(smudgy, vec![source_id])
    } else {
        Task::none()
    };
    let report = report_pane_sizes(smudgy);
    Task::batch([repair, close, report])
}

/// Apply a pane swap — ONE semantics for script `pane.swap` and drag center
/// drops alike: a swap exchanges the two panes' hosted positions, leaving
/// both split trees untouched.
///
/// Same-window, that is a tab-slot exchange (`swap_pane_slots`, backed by
/// the model's `swap_tabs`): the two TabIds travel with their panes, so the
/// keyed body host re-pairs each subtree with its moved tab and per-window
/// widget state follows the pane — including same-group pairs, where the
/// exchange is a strip-slot swap inside one group. Cross-window, it is a
/// pane BINDING exchange (`replace_pane_slot` on each side): the tabs stay
/// in their windows and only the pane payloads trade places. The asymmetry
/// is deliberate and observationally equivalent: a TabId is a runtime-local
/// continuity key for per-window widget state, which cannot cross window
/// trees regardless, and tab identities are never persisted (the durable
/// form records stable pane descriptors), so nothing outlives the exchange
/// that could tell the shapes apart.
///
/// Inactive tabs participate like any others — selection follows the slot,
/// so a pane swapped away from an unselected tab leaves the destination tab
/// unselected, and a swap between two off-screen tabs changes nothing on
/// screen. Activation follows the rendered slot: each involved window is
/// probed before mutation and settled after both halves (payloads and
/// hidden state) land, so whatever the user was looking at keeps their
/// attention, without any focus operation. Hidden state follows each pane
/// identity. Both leaves are resolved before either model mutates, and a
/// failed second rebinding rolls the first back — a half-swap can never
/// escape. No window can become empty: each side loses and gains exactly
/// one pane.
fn swap_script_panes(smudgy: &mut Smudgy, first: PaneRef, second: PaneRef) -> Task<Message> {
    if first == second {
        return Task::none();
    }
    let first_window = smudgy.smudgy_windows.iter().find_map(|(id, window)| {
        window
            .hosts_pane(first.session_id, first.key)
            .then_some(*id)
    });
    let second_window = smudgy.smudgy_windows.iter().find_map(|(id, window)| {
        window
            .hosts_pane(second.session_id, second.key)
            .then_some(*id)
    });
    let (Some(first_window), Some(second_window)) = (first_window, second_window) else {
        log::warn!("Dropping pane swap because one of its leaves is no longer hosted");
        return Task::none();
    };

    if first_window == second_window {
        if let Some(window) = smudgy.smudgy_windows.get_mut(&first_window) {
            let probe = window.pane_swap_render_probe(first, second);
            window.swap_pane_slots(first, second);
            window.settle_active_session_after_pane_swap(probe);
        }
        return report_pane_sizes(smudgy);
    }

    // Both leaves were resolved before either model changes, and the two
    // rebindings form a remove/restore-safe transaction: if the second
    // rebinding fails, the first is rolled back before returning, so a
    // half-swap can never escape this function.
    let first_hidden = smudgy.smudgy_windows[&first_window].pane_hidden(first);
    let second_hidden = smudgy.smudgy_windows[&second_window].pane_hidden(second);
    // Rendered-slot facts are probed before either model mutates; they are
    // settled only after the hidden-state transfers below, which feed each
    // window's post-swap rendered slots.
    let first_probe = smudgy.smudgy_windows[&first_window].pane_swap_render_probe(first, second);
    let second_probe = smudgy.smudgy_windows[&second_window].pane_swap_render_probe(first, second);
    let replaced_first = smudgy
        .smudgy_windows
        .get_mut(&first_window)
        .is_some_and(|window| window.replace_pane_slot(first, second));
    if !replaced_first {
        // Nothing has mutated: rejecting here is a clean no-op.
        log::error!("Pane swap failed to rebind its first leaf after resolution");
        return Task::none();
    }
    let replaced_second = smudgy
        .smudgy_windows
        .get_mut(&second_window)
        .is_some_and(|window| window.replace_pane_slot(second, first));
    if !replaced_second {
        // Roll the first rebinding back so the swap is all-or-nothing. The
        // undo addresses the binding just written, so it cannot itself fail.
        debug_assert!(
            false,
            "pane swap invariant failed after both leaves were resolved"
        );
        let restored = smudgy
            .smudgy_windows
            .get_mut(&first_window)
            .is_some_and(|window| window.replace_pane_slot(second, first));
        if restored {
            if let Some(window) = smudgy.smudgy_windows.get_mut(&first_window) {
                window.set_pane_hidden(first, first_hidden);
            }
            log::error!("Pane swap rejected: second leaf failed to rebind; first restored");
        } else {
            log::error!("Pane swap rollback failed; first window rebound without its partner");
        }
        return Task::none();
    }
    if let Some(window) = smudgy.smudgy_windows.get_mut(&first_window) {
        window.set_pane_hidden(second, second_hidden);
    }
    if let Some(window) = smudgy.smudgy_windows.get_mut(&second_window) {
        window.set_pane_hidden(first, first_hidden);
    }

    if let Some(window) = smudgy.smudgy_windows.get_mut(&first_window) {
        window.settle_active_session_after_pane_swap(first_probe);
    }
    if let Some(window) = smudgy.smudgy_windows.get_mut(&second_window) {
        window.settle_active_session_after_pane_swap(second_probe);
    }
    report_pane_sizes(smudgy)
}

/// Apply a script `pane.tearOut`: the drag tear-out flow minus the drag —
/// detach the pane's tab (one tab, preserving the rest of any group it sat
/// in, its stable id traveling with it) into a fresh dedicated window,
/// sized by the request (or like the pane it carries), positioned by the
/// OS. Windows stay emergent: no script-facing window identity is minted,
/// and the empty-window rule closes the window when its last pane leaves.
fn tear_out_script_pane(
    smudgy: &mut Smudgy,
    session_id: SessionId,
    key: PaneKey,
    width: Option<f32>,
    height: Option<f32>,
) -> Task<Message> {
    let slot = PaneRef { session_id, key };
    let Some(source_id) = smudgy
        .smudgy_windows
        .iter()
        .find_map(|(id, window)| window.hosts_pane(session_id, key).then_some(*id))
    else {
        log::warn!("No window hosts {key} for session {session_id}; dropping tearOut");
        return Task::none();
    };
    let Some(source) = smudgy.smudgy_windows.get_mut(&source_id) else {
        return Task::none();
    };
    // The size is grid-derived; it must be read before the extraction below
    // mutates the model.
    let measured = source.pane_size(slot);
    let Some((tab, hidden, emptied)) = source.extract_pane_tab(slot) else {
        return Task::none();
    };
    let repair = source
        .repair_active_session(&smudgy.sessions)
        .map(move |msg| Message::SmudgyWindowMessage(source_id, msg));

    let mut settings = smudgy_window_settings();
    // Size the window by the request, falling back per dimension to the
    // pane's measured rect (plus the toolbar band), floored by the window
    // minimum — the drag tear-out's sizing rule.
    let fallback = measured.map(|size| (size.width, size.height + TORN_OUT_CHROME_HEIGHT));
    let width = width.or(fallback.map(|(w, _)| w));
    let height = height.or(fallback.map(|(_, h)| h));
    if width.is_some() || height.is_some() {
        settings.size = Size::new(
            width.unwrap_or(settings.size.width).max(640.0),
            height.unwrap_or(settings.size.height).max(400.0),
        );
    }

    let (id, open_task) = window::open(settings);
    let mut torn_out = windows::smudgy_window::SmudgyWindow::new(id, smudgy.account.handles());
    torn_out.adopt_torn_out_tab(tab);
    torn_out.set_pane_hidden(slot, hidden);
    smudgy.smudgy_windows.insert(id, torn_out);
    // The workspace mirror must learn the window in the same update that
    // inserts it: snapshot capture walks only mirror-registered windows, so
    // a checkpoint firing before the deferred `NewSmudgyWindow` arm would
    // otherwise drop the torn-out window (and its pane) from the persisted
    // workspace. Registration is idempotent — the open task's arm
    // re-announces harmlessly.
    smudgy.workspace.register_window(id);

    let activate = Task::done(Message::SmudgyWindowMessage(
        id,
        windows::smudgy_window::Message::SetActiveSession(session_id),
    ));
    let close = if emptied {
        close_emptied_windows(smudgy, vec![source_id])
    } else {
        Task::none()
    };
    let report = report_pane_sizes(smudgy);
    Task::batch([
        open_task.map(Message::NewSmudgyWindow),
        activate,
        close,
        repair,
        report,
    ])
}

fn place_pane_in_windows(
    smudgy: &mut Smudgy,
    session_id: SessionId,
    key: PaneKey,
    placement: PanePlacement,
) -> bool {
    // Open also travels the owning session stream to order display-state
    // materialization before AppendTo. Whichever path reaches the daemon
    // second must not duplicate a pane that a later bus command already moved.
    if smudgy
        .smudgy_windows
        .values()
        .any(|window| window.hosts_pane(session_id, key))
    {
        return false;
    }
    // A placeholder staged for this pane (a template restore or an adopted
    // vacancy) wins over the script's placement request: the pane binds in
    // place, in its stored position, and its stored eyeball preference
    // replays once through the normal user-toggle path. Unknown panes fall
    // through to normal placement.
    if let Some(descriptor) = smudgy
        .sessions
        .get(session_id)
        .and_then(|session| session.pane_def(key))
        .map(|def| workspace::restore::descriptor_key(&def.namespace, &def.name))
    {
        for window in smudgy.smudgy_windows.values_mut() {
            let Some(hidden) = window.claim_pending_pane(session_id, &descriptor, key) else {
                continue;
            };
            if smudgy.restore.is_ready(session_id) {
                if let Some(session) = smudgy.sessions.get(session_id) {
                    session.report_user_hidden(key, hidden);
                }
            } else {
                smudgy.restore.owe_hidden(session_id, key, hidden);
            }
            return false;
        }
    }
    let target = smudgy
        .smudgy_windows
        .iter()
        .find_map(|(id, window)| {
            window
                .hosts_pane(session_id, placement.reference())
                .then_some(*id)
        })
        .or_else(|| {
            smudgy.smudgy_windows.iter().find_map(|(id, window)| {
                window.hosts_pane(session_id, MAIN_PANE_KEY).then_some(*id)
            })
        })
        .or_else(|| smudgy.smudgy_windows.keys().next().copied());
    match target.and_then(|id| smudgy.smudgy_windows.get_mut(&id)) {
        Some(window) => {
            window.place_session_pane(session_id, key, placement);
            true
        }
        None => {
            log::warn!("No window available to place {key} for session {session_id}");
            false
        }
    }
}

/// Drop one closed pane's slot from whatever window hosts it, then apply the
/// empty-window rule.
fn remove_pane_from_windows(
    smudgy: &mut Smudgy,
    session_id: SessionId,
    key: PaneKey,
) -> Task<Message> {
    // The dragged pane closing mid-drag (script `pane.close()`) aborts the
    // drag with zero mutation; a pressed one must never promote.
    if smudgy
        .tab_drag
        .as_ref()
        .is_some_and(|drag| drag.slot.session_id == session_id && drag.slot.key == key)
    {
        cancel_tab_drag(smudgy, "dragged pane closed mid-drag");
    }
    if smudgy
        .tab_press
        .is_some_and(|press| press.slot.session_id == session_id && press.slot.key == key)
    {
        smudgy.tab_press = None;
    }

    let mut emptied: Vec<window::Id> = Vec::new();
    for (window_id, window) in smudgy.smudgy_windows.iter_mut() {
        if window.remove_pane_slot(session_id, key) {
            emptied.push(*window_id);
        }
    }
    close_emptied_windows(smudgy, emptied)
}

/// Vertical chrome (collapsed toolbar band) added to a pane's size when
/// sizing the window torn out around it — approximate by design; the OS
/// minimum-size floor applies on top.
const TORN_OUT_CHROME_HEIGHT: f32 = 34.0;

/// A concise signature of a drag hover for change-gated logging: the
/// hovered window, the action kind, and the target group (if any).
fn hover_signature(
    hover: Option<&pane_drag::DragHover>,
) -> Option<(window::Id, &'static str, u64)> {
    let hover = hover?;
    let (tag, group) = match hover.target.as_ref().map(|t| &t.action) {
        None => ("none", 0),
        Some(pane_drag::DragAction::GridEdge(_)) => ("grid-edge", 0),
        Some(pane_drag::DragAction::Merge { group, .. }) => ("merge", group.as_u64()),
        Some(pane_drag::DragAction::Swap { group }) => ("swap", group.as_u64()),
        Some(pane_drag::DragAction::Split { group, .. }) => ("split", group.as_u64()),
        Some(pane_drag::DragAction::Vacant) => ("vacant", 0),
    };
    Some((hover.window, tag, group))
}

/// Process one tracked cursor sample while a tab drag is in flight: resolve
/// the hovered smudgy window (most-recently-focused wins on overlap, exactly
/// like the release hit-test), classify the target against its live
/// geometry, and store the hover for the overlay. Hit-test plus overlay
/// content only — the per-move cost bound.
fn track_drag_motion(smudgy: &mut Smudgy, id: window::Id, position: Point) {
    // Daemon-owned deadband: promote a pending press whose tracked motion
    // crossed the threshold. The widget's own crossing is the fast path;
    // this one survives a press surface whose state was erased mid-gesture.
    if smudgy.tab_drag.is_none()
        && let Some(press) = smudgy.tab_press
        && id == press.window
        && position.distance(press.press) > pane_drag::DRAG_DEADBAND
    {
        let resolves = smudgy
            .smudgy_windows
            .get(&press.window)
            .is_some_and(|window| window.drag_tab_resolves(press.tab, press.slot, press.group));
        smudgy.tab_press = None;
        if resolves {
            log::info!(
                "[pane-drag] drag started (daemon deadband): tab {:?} ({}/{}) from {:?}, press=({:.1}, {:.1})",
                press.tab,
                press.slot.session_id,
                press.slot.key,
                press.window,
                press.press.x,
                press.press.y,
            );
            smudgy.tab_drag = Some(pane_drag::TabDrag {
                source_window: press.window,
                tab: press.tab,
                slot: press.slot,
                source_group: press.group,
                press: press.press,
                hover: None,
            });
        }
    }
    let Some(drag) = smudgy.tab_drag.as_ref() else {
        return;
    };
    if id != drag.source_window {
        // The OS capture routes all mid-drag motion to the source window;
        // anything else is post-release noise or capture evidence.
        return;
    }
    let Some(track) = smudgy.window_tracker.get(drag.source_window).copied() else {
        return;
    };
    let inside = Rectangle::with_size(track.size).contains(position);
    let hovered: Option<(window::Id, Point, Size)> = if inside {
        Some((drag.source_window, position, track.size))
    } else {
        track
            .origin
            .map(|origin| pane_drag::screen_point(origin, position, track.scale))
            .and_then(|screen| {
                smudgy
                    .window_tracker
                    .mru_order()
                    .into_iter()
                    .filter(|window_id| smudgy.smudgy_windows.contains_key(window_id))
                    .find_map(|window_id| {
                        let target = smudgy.window_tracker.get(window_id)?;
                        let local = pane_drag::window_local(target, screen)?;
                        Some((window_id, local, target.size))
                    })
            })
    };
    let hover = hovered.map(|(window_id, local, window_size)| {
        let target = smudgy
            .smudgy_windows
            .get(&window_id)
            .and_then(|window| window.classify_drag_target(local, window_size, drag));
        pane_drag::DragHover {
            window: window_id,
            target,
        }
    });

    let old_signature = hover_signature(drag.hover.as_ref());
    let new_signature = hover_signature(hover.as_ref());
    if new_signature != old_signature {
        match &new_signature {
            Some((window_id, tag, group)) => log::info!(
                "[pane-drag] hover {window_id:?}: target={tag}{}",
                if *group != 0 {
                    format!(" group={group}")
                } else {
                    String::new()
                }
            ),
            None => log::info!("[pane-drag] hover: no smudgy window (tear-out territory)"),
        }
    }

    if let Some(drag) = smudgy.tab_drag.as_mut() {
        drag.hover = hover;
    }
}

/// Cancel the drag in flight, if any: zero mutation, feedback cleared on the
/// next frame (windows derive drag state from the daemon), and the press
/// surfaces stand down via their `drag_live` diff reset. A cancel is a drag
/// terminal, so it dumps the (unchanged) layouts — the scripted matrix
/// asserts zero mutation against exactly this post-cancel evidence.
fn cancel_tab_drag(smudgy: &mut Smudgy, reason: &str) {
    if let Some(drag) = smudgy.tab_drag.take() {
        log::info!(
            "[pane-drag] cancel ({reason}): tab {:?} ({}/{})",
            drag.tab,
            drag.slot.session_id,
            drag.slot.key,
        );
        log_drag_layouts(smudgy);
    }
}

/// Log every smudgy window's group/tab structure — the post-state evidence
/// the scripted drag matrix asserts after each terminal. Drag terminals
/// only; never a hot path.
fn log_drag_layouts(smudgy: &Smudgy) {
    for (id, window) in &smudgy.smudgy_windows {
        log::info!("[pane-drag] layout {id:?}: {}", window.describe_layout());
    }
}

/// Resolve a tab-drag release at `point` (source-window local; `None` = no
/// honest cursor sample = cancel). Consumes the drag record, re-resolves
/// every participant against the live model, classifies the release against
/// live geometry, and applies exactly one terminal operation — or cancels
/// with zero mutation.
fn finish_tab_drag(smudgy: &mut Smudgy, point: Option<Point>) -> Task<Message> {
    let Some(drag) = smudgy.tab_drag.take() else {
        return Task::none();
    };
    // Stale-identity re-resolution: the session lives, the source window
    // lives, and the dragged pane is still bound to the dragged tab.
    if smudgy.sessions.get(drag.slot.session_id).is_none() {
        log::info!("[pane-drag] cancel (session gone at release)");
        return Task::none();
    }
    let Some(source) = smudgy.smudgy_windows.get(&drag.source_window) else {
        log::info!("[pane-drag] cancel (source window gone at release)");
        return Task::none();
    };
    if !source.drag_tab_resolves(drag.tab, drag.slot, drag.source_group) {
        log::info!("[pane-drag] cancel (stale drag identity at release)");
        return Task::none();
    }
    let Some(point) = point else {
        log::info!("[pane-drag] cancel (no cursor sample at release)");
        return Task::none();
    };
    log::info!(
        "[pane-drag] release at ({:.1}, {:.1}), {:.1} from press",
        point.x,
        point.y,
        point.distance(drag.press),
    );
    let Some(track) = smudgy.window_tracker.get(drag.source_window).copied() else {
        log::info!("[pane-drag] cancel (source window untracked at release)");
        return Task::none();
    };

    // Inside the source window: classify window-locally — correct even on
    // platforms without global window origins.
    if Rectangle::with_size(track.size).contains(point) {
        let source_window = drag.source_window;
        let target = source.classify_drag_target(point, track.size, &drag);
        return apply_drag_action(smudgy, drag, source_window, target);
    }

    // Outside the source window: reconstruct screen space and hit-test the
    // other smudgy windows, most-recently-focused first. An unknown source
    // origin (Wayland) cannot resolve any cross-window target, so the drop
    // degrades to tear-out; a release over no smudgy window tears out.
    if let Some(screen) = track
        .origin
        .map(|origin| pane_drag::screen_point(origin, point, track.scale))
    {
        for target_id in smudgy.window_tracker.mru_order() {
            if !smudgy.smudgy_windows.contains_key(&target_id) {
                continue;
            }
            let Some(target_track) = smudgy.window_tracker.get(target_id).copied() else {
                continue;
            };
            let Some(local) = pane_drag::window_local(&target_track, screen) else {
                continue;
            };
            let target = smudgy
                .smudgy_windows
                .get(&target_id)
                .and_then(|window| window.classify_drag_target(local, target_track.size, &drag));
            return apply_drag_action(smudgy, drag, target_id, target);
        }
        return tear_out_dragged_tab(smudgy, drag, Some(screen));
    }
    tear_out_dragged_tab(smudgy, drag, None)
}

/// Apply one classified drop. All participants were re-resolved by the
/// caller; window-local operations re-validate their own participants and
/// reject (not partially apply) anything that no longer resolves.
fn apply_drag_action(
    smudgy: &mut Smudgy,
    drag: pane_drag::TabDrag,
    window_id: window::Id,
    target: Option<pane_drag::ClassifiedTarget>,
) -> Task<Message> {
    let Some(target) = target else {
        // No drop surface under the release: the no-op re-dock.
        log::info!("[pane-drag] drop: no target under release — no-op re-dock");
        return Task::none();
    };
    let same_window = window_id == drag.source_window;
    let task = match target.action {
        pane_drag::DragAction::Merge { group, slot } if same_window => {
            let task = smudgy
                .smudgy_windows
                .get_mut(&window_id)
                .and_then(|window| {
                    window.apply_drag_merge(drag.tab, group, slot, &mut smudgy.sessions)
                });
            match task {
                Some(task) => {
                    log::info!(
                        "[pane-drag] drop: merge tab {:?} into group {} at slot {} (same window)",
                        drag.tab,
                        group.as_u64(),
                        slot,
                    );
                    task.map(move |msg| Message::SmudgyWindowMessage(window_id, msg))
                }
                None => {
                    log::info!("[pane-drag] drop rejected: merge participants no longer resolve");
                    Task::none()
                }
            }
        }
        pane_drag::DragAction::Merge { group, slot } => cross_window_drop(
            smudgy,
            drag,
            window_id,
            CrossPlacement::Merge { group, slot },
        ),
        pane_drag::DragAction::Swap { group } => {
            // The swap partner is the target group's currently RENDERED tab
            // — what the user sees is what swaps.
            let partner = smudgy
                .smudgy_windows
                .get(&window_id)
                .and_then(|window| window.rendered_slot(group));
            match partner {
                Some(partner) if partner != drag.slot => {
                    log::info!(
                        "[pane-drag] drop: swap {}/{} with rendered {}/{}",
                        drag.slot.session_id,
                        drag.slot.key,
                        partner.session_id,
                        partner.key,
                    );
                    let swap = swap_script_panes(smudgy, drag.slot, partner);
                    // A cross-window swap lands the dragged pane in the
                    // window under the release; OS focus follows the drop
                    // (see `cross_window_drop`), and a same-window swap
                    // never churns it.
                    if window_id == drag.source_window {
                        swap
                    } else {
                        Task::batch([swap, window::gain_focus(window_id)])
                    }
                }
                Some(_) => {
                    log::info!("[pane-drag] drop: swap with itself — no-op");
                    Task::none()
                }
                None => {
                    log::info!("[pane-drag] drop rejected: swap target renders no pane");
                    Task::none()
                }
            }
        }
        pane_drag::DragAction::Split { group, region } if same_window => {
            let task = smudgy
                .smudgy_windows
                .get_mut(&window_id)
                .and_then(|window| {
                    window.apply_drag_split(drag.tab, group, region, &mut smudgy.sessions)
                });
            match task {
                Some(task) => {
                    log::info!(
                        "[pane-drag] drop: split tab {:?} beside group {} ({region:?})",
                        drag.tab,
                        group.as_u64(),
                    );
                    task.map(move |msg| Message::SmudgyWindowMessage(window_id, msg))
                }
                None => {
                    log::info!("[pane-drag] drop rejected: split participants no longer resolve");
                    Task::none()
                }
            }
        }
        pane_drag::DragAction::Split { group, region } => cross_window_drop(
            smudgy,
            drag,
            window_id,
            CrossPlacement::Split { group, region },
        ),
        pane_drag::DragAction::GridEdge(side) if same_window => {
            let task = smudgy
                .smudgy_windows
                .get_mut(&window_id)
                .and_then(|window| {
                    window.apply_drag_grid_edge(drag.tab, side, &mut smudgy.sessions)
                });
            match task {
                Some(task) => {
                    log::info!("[pane-drag] drop: grid edge {side:?} (same window)");
                    task.map(move |msg| Message::SmudgyWindowMessage(window_id, msg))
                }
                None => {
                    log::info!("[pane-drag] drop rejected: grid-edge tab no longer resolves");
                    Task::none()
                }
            }
        }
        pane_drag::DragAction::GridEdge(side) => {
            cross_window_drop(smudgy, drag, window_id, CrossPlacement::Edge(side))
        }
        pane_drag::DragAction::Vacant => {
            cross_window_drop(smudgy, drag, window_id, CrossPlacement::Cluster)
        }
    };
    log_drag_layouts(smudgy);
    Task::batch([task, report_pane_sizes(smudgy)])
}

/// Where a cross-window drop lands in the destination window.
enum CrossPlacement {
    Merge {
        group: pane_groups::GroupId,
        slot: usize,
    },
    Split {
        group: pane_groups::GroupId,
        region: pane_drag::DropRegion,
    },
    Edge(pane_drag::GridEdgeSide),
    Cluster,
}

/// Move the dragged tab between two windows as a remove/restore-safe
/// transaction: both windows were validated before the first mutation (the
/// caller re-resolved the source; the placement was classified against the
/// destination's live model in this same update), the tab value carries its
/// stable identity across, and a destination rejection re-hosts the tab as
/// its own cluster there — a detached tab is never stranded. Attention
/// moves with the tab: it is selected in the destination and its session
/// becomes active there.
fn cross_window_drop(
    smudgy: &mut Smudgy,
    drag: pane_drag::TabDrag,
    target_id: window::Id,
    placement: CrossPlacement,
) -> Task<Message> {
    let source_id = drag.source_window;
    if !smudgy.smudgy_windows.contains_key(&target_id) {
        log::info!("[pane-drag] drop rejected: destination window gone");
        return Task::none();
    }
    let Some(source) = smudgy.smudgy_windows.get_mut(&source_id) else {
        return Task::none();
    };
    let Some((tab_value, hidden, emptied)) = source.extract_drag_tab(drag.tab) else {
        log::info!("[pane-drag] drop rejected: dragged tab no longer resolves");
        return Task::none();
    };
    let repair = source
        .repair_active_session(&smudgy.sessions)
        .map(move |msg| Message::SmudgyWindowMessage(source_id, msg));
    let Some(target) = smudgy.smudgy_windows.get_mut(&target_id) else {
        // Unreachable (checked above; no await separates the check from
        // here). Restore rather than strand — hidden state included, so the
        // re-hosted tab keeps exactly what the extraction removed.
        if let Some(source) = smudgy.smudgy_windows.get_mut(&source_id) {
            source.adopt_drag_tab_cluster(tab_value);
            source.set_pane_hidden(drag.slot, hidden);
        }
        return repair;
    };
    match placement {
        CrossPlacement::Merge { group, slot } => {
            log::info!(
                "[pane-drag] drop: merge tab {:?} into group {} at slot {} (cross-window)",
                drag.tab,
                group.as_u64(),
                slot,
            );
            target.adopt_drag_tab_merge(tab_value, group, slot);
        }
        CrossPlacement::Split { group, region } => {
            log::info!(
                "[pane-drag] drop: split tab {:?} beside group {} ({region:?}, cross-window)",
                drag.tab,
                group.as_u64(),
            );
            target.adopt_drag_tab_split(tab_value, group, region);
        }
        CrossPlacement::Edge(side) => {
            log::info!("[pane-drag] drop: grid edge {side:?} (cross-window)");
            target.adopt_drag_tab_edge(tab_value, side);
        }
        CrossPlacement::Cluster => {
            log::info!("[pane-drag] drop: adopt into empty window {target_id:?}");
            target.adopt_drag_tab_cluster(tab_value);
        }
    }
    target.set_pane_hidden(drag.slot, hidden);
    let select = target
        .select_tab(drag.tab, &mut smudgy.sessions)
        .map(move |msg| Message::SmudgyWindowMessage(target_id, msg));
    // Selection hands keyboard focus to the landed pane's input, but
    // keystrokes only reach a window the OS has focused — and the
    // destination of a cross-window drop need not be (the drag began over
    // the source window). Bring it forward so typing lands immediately,
    // exactly as a tear-out's freshly opened window does. Guarded so a
    // same-window landing never churns OS focus.
    let focus = if target_id == source_id {
        Task::none()
    } else {
        window::gain_focus(target_id)
    };
    let close = if emptied {
        close_emptied_windows(smudgy, vec![source_id])
    } else {
        Task::none()
    };
    Task::batch([repair, select, focus, close])
}

/// Tear the dragged tab out into a new smudgy window at the release point
/// — the terminal for a release outside every smudgy window, and the
/// documented degradation for cross-window drops without global window
/// origins. The window is sized like the pane it carries (its last
/// measured size — the stale-size fallback), the entry is inserted
/// synchronously so the pane has a grid to live in from this update on, and
/// attention moves with the tab.
fn tear_out_dragged_tab(
    smudgy: &mut Smudgy,
    drag: pane_drag::TabDrag,
    screen: Option<Point>,
) -> Task<Message> {
    let source_id = drag.source_window;
    let scale = smudgy
        .window_tracker
        .get(source_id)
        .map_or(1.0, |track| track.scale);
    let Some(source) = smudgy.smudgy_windows.get_mut(&source_id) else {
        return Task::none();
    };
    let pane_size = source.pane_size(drag.slot);
    let Some((tab_value, hidden, emptied)) = source.extract_drag_tab(drag.tab) else {
        log::info!("[pane-drag] cancel (tear-out tab no longer resolves)");
        return Task::none();
    };
    log::info!(
        "[pane-drag] drop: tear out {}/{} into a new window",
        drag.slot.session_id,
        drag.slot.key,
    );
    let repair = source
        .repair_active_session(&smudgy.sessions)
        .map(move |msg| Message::SmudgyWindowMessage(source_id, msg));

    let mut settings = smudgy_window_settings();
    // Size the window like the pane it carries (plus the toolbar band),
    // bounded below by the window minimum.
    if let Some(size) = pane_size {
        settings.size = Size::new(
            size.width.max(640.0),
            (size.height + TORN_OUT_CHROME_HEIGHT).max(400.0),
        );
    }
    // Put the pane's title bar roughly under the cursor. `Specific` takes
    // logical coordinates; the source window's scale stands in for the
    // target monitor's (exact when they match). Without a screen point
    // (Wayland) the OS chooses the position.
    if let Some(screen) = screen {
        settings.position = window::Position::Specific(Point::new(
            screen.x / scale - 40.0,
            screen.y / scale - 12.0,
        ));
    }

    let (id, open_task) = window::open(settings);
    let mut torn_out = windows::smudgy_window::SmudgyWindow::new(id, smudgy.account.handles());
    torn_out.adopt_torn_out_tab(tab_value);
    torn_out.set_pane_hidden(drag.slot, hidden);
    smudgy.smudgy_windows.insert(id, torn_out);
    // The workspace mirror must learn the window in the same update that
    // inserts it: snapshot capture walks only mirror-registered windows, so
    // a checkpoint firing before the deferred `NewSmudgyWindow` arm would
    // otherwise drop the torn-out window (and its pane) from the persisted
    // workspace. Registration is idempotent — the open task's arm
    // re-announces harmlessly.
    smudgy.workspace.register_window(id);

    let activate = Task::done(Message::SmudgyWindowMessage(
        id,
        windows::smudgy_window::Message::SetActiveSession(drag.slot.session_id),
    ));
    let close = if emptied {
        close_emptied_windows(smudgy, vec![source_id])
    } else {
        Task::none()
    };
    log_drag_layouts(smudgy);
    Task::batch([
        open_task.map(Message::NewSmudgyWindow),
        activate,
        close,
        repair,
        report_pane_sizes(smudgy),
    ])
}

/// Loads the per-area prefs from settings, migrating a legacy disabled-only
/// file: each `disabled_map_areas` entry becomes an explicit `disabled:true`
/// pref stamped at the Unix epoch, so any real server pref — or a fresh local
/// edit — wins on the first reconcile.
fn load_area_prefs(settings: &Settings) -> HashMap<AreaId, MapAreaPref> {
    if !settings.map_area_prefs.is_empty() {
        return settings
            .map_area_prefs
            .iter()
            .map(|pref| (pref.area_id, pref.clone()))
            .collect();
    }
    let epoch = DateTime::<Utc>::from_timestamp(0, 0).expect("unix epoch is a valid timestamp");
    settings
        .disabled_map_areas
        .iter()
        .map(|&area_id| {
            (
                area_id,
                MapAreaPref {
                    area_id,
                    disabled: true,
                    updated_at: epoch,
                },
            )
        })
        .collect()
}

/// The derived effective disabled set: exactly the `disabled == true` prefs.
fn disabled_set_from_prefs(prefs: &HashMap<AreaId, MapAreaPref>) -> HashSet<AreaId> {
    prefs
        .iter()
        .filter(|(_, pref)| pref.disabled)
        .map(|(id, _)| *id)
        .collect()
}

/// Persists the per-area prefs by re-reading settings.json fresh and
/// overwriting only the pref fields — the timestamped set plus its derived
/// `disabled_map_areas` list (both sorted for stable diffs) — so a concurrent
/// settings edit isn't clobbered.
fn persist_area_prefs(prefs: &HashMap<AreaId, MapAreaPref>) {
    let mut settings = smudgy_core::models::settings::load_settings();
    let mut rows: Vec<MapAreaPref> = prefs.values().cloned().collect();
    rows.sort_by_key(|pref| pref.area_id.0);
    let mut disabled: Vec<AreaId> = disabled_set_from_prefs(prefs).into_iter().collect();
    disabled.sort_by_key(|id| id.0);
    settings.map_area_prefs = rows;
    settings.disabled_map_areas = disabled;
    if let Err(err) = smudgy_core::models::settings::save_settings(&settings) {
        log::warn!("failed to persist map area prefs: {err}");
    }
}

/// Recomputes the derived disabled set from the prefs, fans it out to every
/// live mapper, and persists. Call after any reconcile-driven pref change.
fn apply_and_persist_area_prefs(smudgy: &mut Smudgy) {
    let set = disabled_set_from_prefs(&smudgy.area_prefs);
    smudgy.disabled_map_areas = set.clone();
    persist_area_prefs(&smudgy.area_prefs);
    apply_disabled_map_areas(smudgy, &set);
}

/// Stamps the areas whose disabled state flips relative to the current prefs
/// with `now` and returns `(area_id, disabled)` for each change, so the caller
/// can push them to the cloud. An un-mute is stored as an explicit
/// `disabled:false` row (not a deletion) so its timestamp can win a later
/// last-write-wins reconcile against another device.
fn stamp_area_pref_changes(
    prefs: &mut HashMap<AreaId, MapAreaPref>,
    set: &HashSet<AreaId>,
    now: DateTime<Utc>,
) -> Vec<(AreaId, bool)> {
    let mut changed: Vec<(AreaId, bool)> = Vec::new();

    // Newly disabled (or first-time disabled) areas.
    for &area_id in set {
        let was_disabled = prefs.get(&area_id).is_some_and(|p| p.disabled);
        if !was_disabled {
            prefs.insert(
                area_id,
                MapAreaPref {
                    area_id,
                    disabled: true,
                    updated_at: now,
                },
            );
            changed.push((area_id, true));
        }
    }

    // Areas that left the disabled set become explicit `disabled:false`.
    let newly_enabled: Vec<AreaId> = prefs
        .iter()
        .filter(|(id, pref)| pref.disabled && !set.contains(*id))
        .map(|(id, _)| *id)
        .collect();
    for area_id in newly_enabled {
        prefs.insert(
            area_id,
            MapAreaPref {
                area_id,
                disabled: false,
                updated_at: now,
            },
        );
        changed.push((area_id, false));
    }

    changed
}

/// Merges a freshly fetched server pref set into the local prefs by
/// last-write-wins on `updated_at`, mutating `prefs` in place and returning
/// the `(area_id, disabled)` changes to push back:
/// - both sides present → newer `updated_at` wins; a local-newer row whose
///   value differs from the server is queued for push.
/// - server only → adopt the server row.
/// - local only (no server row) and `disabled` → queue for push, unless the
///   area is `parked` — a prior push already came back "not viewable"
///   (local-tier or access lost) this launch, and the server's answer won't
///   change on a timer. Skipping keeps the pref local (never silently flipped
///   to enabled) without re-attempting a refused PUT every reconcile tick.
fn merge_server_area_prefs(
    prefs: &mut HashMap<AreaId, MapAreaPref>,
    server: &[AreaPref],
    parked: &HashSet<AreaId>,
) -> Vec<(AreaId, bool)> {
    let mut pushes: Vec<(AreaId, bool)> = Vec::new();
    let server_ids: HashSet<AreaId> = server.iter().map(|pref| pref.area_id).collect();

    for srv in server {
        match prefs.get(&srv.area_id) {
            Some(local) if local.updated_at > srv.updated_at => {
                if local.disabled != srv.disabled {
                    pushes.push((srv.area_id, local.disabled));
                }
            }
            _ => {
                prefs.insert(
                    srv.area_id,
                    MapAreaPref {
                        area_id: srv.area_id,
                        disabled: srv.disabled,
                        updated_at: srv.updated_at,
                    },
                );
            }
        }
    }

    for (area_id, local) in prefs.iter() {
        if local.disabled && !server_ids.contains(area_id) && !parked.contains(area_id) {
            pushes.push((*area_id, true));
        }
    }

    pushes
}

/// A reconcile pull (`GET /me/area-prefs`) when signed in, else a no-op.
fn reconcile_area_prefs(smudgy: &Smudgy) -> Task<Message> {
    if smudgy.account.snapshot().signed_in {
        reconcile_area_prefs_task(&smudgy.account.handles().client)
    } else {
        Task::none()
    }
}

fn reconcile_area_prefs_task(client: &CloudApiClient) -> Task<Message> {
    let client = client.clone();
    Task::perform(
        async move { client.area_prefs().await },
        Message::AreaPrefsFetched,
    )
}

/// Pushes each `(area_id, disabled)` change to `/me/area-prefs` via PUT,
/// routing the server-stamped result back as [`Message::AreaPrefPushed`].
fn push_area_prefs_task(smudgy: &Smudgy, changes: &[(AreaId, bool)]) -> Task<Message> {
    if changes.is_empty() {
        return Task::none();
    }
    let client = smudgy.account.handles().client;
    let tasks = changes.iter().map(|&(area_id, disabled)| {
        let client = client.clone();
        Task::perform(
            async move { client.set_area_pref(area_id, disabled).await },
            move |result| Message::AreaPrefPushed { area_id, result },
        )
    });
    Task::batch(tasks)
}

/// Fans the disabled-map-areas set out to every live session's mapper and
/// every open map editor window's mapper (set_disabled_areas is idempotent,
/// so double-application is harmless).
fn apply_disabled_map_areas(smudgy: &Smudgy, set: &HashSet<AreaId>) {
    for (_, session) in smudgy.sessions.iter() {
        if let Some(mapper) = &session.mapper {
            mapper.set_disabled_areas(set.clone());
        }
    }
    for window in smudgy.map_editor_windows.values() {
        window.mapper().set_disabled_areas(set.clone());
    }
}

/// Recomputes each server entry's cloud-map scope exclusions from the
/// authoritative [`Smudgy::map_scopes`] and pushes them to every live session's
/// mapper and every open map editor window's mapper. Unlike the (global)
/// disabled set, scope exclusions are per-entry, so this resolves each mapper's
/// server context before applying (`set_scope_exclusions` is idempotent).
fn apply_scope_exclusions(smudgy: &Smudgy) {
    for (_, session) in smudgy.sessions.iter() {
        if let Some(mapper) = &session.mapper {
            mapper.set_scope_exclusions(
                smudgy.map_scopes.excluded_atlases(&session.server_name),
                smudgy.map_scopes.excluded_areas(&session.server_name),
            );
        }
    }
    for window in smudgy.map_editor_windows.values() {
        if let Some(server) = window.server_name() {
            window.mapper().set_scope_exclusions(
                smudgy.map_scopes.excluded_atlases(server),
                smudgy.map_scopes.excluded_areas(server),
            );
        }
    }
}

// ===== per-server map scoping: bind-on-use, cross-entry rescue, creation =====
//
// The daemon owns the authoritative `map_scopes`, so every convergence signal
// (a locate streak, a speedwalk, a rescue accept, a creation) resolves and
// commits here. Session runtimes only report *evidence* (locations, navigation,
// rescue hits, creations); the policy lives entirely in these functions.

/// Resolve a session location/navigation area to the scope target it would bind
/// (its atlas, or the atlas-less cloud area itself), or `None` when the area is
/// ephemeral or local-tier — neither ever binds. Local ids collide across
/// entries (the 0.4.1 migration seeded verbatim copies with preserved ids), so
/// scoping a local area would wrongly hide its twin on another entry; ephemeral
/// areas are session-scoped by nature.
fn bind_target_for_area(mapper: &Mapper, area_id: AreaId) -> Option<BindTarget> {
    if mapper.area_storage(&area_id) == MapStorage::Session
        || mapper.local_area_ids().contains(&area_id)
    {
        return None;
    }
    let atlas = mapper.get_current_atlas();
    let atlas_id = atlas
        .get_area(&area_id)
        .and_then(|area| area.meta().atlas_id);
    Some(match atlas_id {
        Some(atlas_id) => BindTarget::Atlas(atlas_id),
        None => BindTarget::Area(area_id),
    })
}

/// The scope state of `target` for `entry`.
fn target_scope(scopes: &MapScopes, target: BindTarget, entry: &str) -> ScopeState {
    match target {
        BindTarget::Atlas(atlas_id) => scopes.atlas_scope(&atlas_id, entry),
        BindTarget::Area(area_id) => scopes.area_scope(&area_id, entry),
    }
}

/// Show or hide `target` on a single server `entry`.
fn set_scope_entry(scopes: &mut MapScopes, target: BindTarget, entry: &str, show: bool) {
    match target {
        BindTarget::Atlas(atlas_id) => scopes.set_atlas_entry(atlas_id, entry, show),
        BindTarget::Area(area_id) => scopes.set_area_entry(area_id, entry, show),
    }
}

/// The `(target, is-unassigned)` bind input for a session location, or `None`
/// when the area can never bind (ephemeral/local/unknown, or no mapper).
fn resolve_bind_input(
    smudgy: &Smudgy,
    session_id: SessionId,
    area_id: AreaId,
) -> Option<(BindTarget, bool)> {
    let session = smudgy.sessions.get(session_id)?;
    let mapper = session.mapper.as_ref()?;
    let target = bind_target_for_area(mapper, area_id)?;
    let unassigned =
        target_scope(&smudgy.map_scopes, target, &session.server_name) == ScopeState::Unassigned;
    Some((target, unassigned))
}

/// Passive bind-on-use: fold one resolved locate into the session's streak and
/// bind when it reaches [`session_store::LOCATE_BIND_STREAK`]. An
/// ephemeral/local/unknown area (or a non-unassigned target) breaks the streak
/// without binding.
fn observe_locate_for_binding(
    smudgy: &mut Smudgy,
    session_id: SessionId,
    area_id: AreaId,
) -> Task<Message> {
    let Some((target, unassigned)) = resolve_bind_input(smudgy, session_id, area_id) else {
        if let Some(session) = smudgy.sessions.get_mut(session_id) {
            session.bind_tracker.reset_streak();
        }
        return Task::none();
    };
    let should_bind = smudgy
        .sessions
        .get_mut(session_id)
        .is_some_and(|session| session.bind_tracker.observe_locate(target, unassigned));
    if should_bind {
        bind_target(smudgy, session_id, target)
    } else {
        Task::none()
    }
}

/// Demonstrated navigation intent (a speedwalk / find-nearest resolution): binds
/// immediately when the destination target is unassigned.
fn observe_navigation_for_binding(
    smudgy: &mut Smudgy,
    session_id: SessionId,
    area_id: AreaId,
) -> Task<Message> {
    let Some((target, unassigned)) = resolve_bind_input(smudgy, session_id, area_id) else {
        return Task::none();
    };
    if unassigned {
        bind_target(smudgy, session_id, target)
    } else {
        Task::none()
    }
}

/// Associate `target` with the session's server entry and commit + fan out the
/// change. Silent — unwinding an unwanted association is a map-editor decision
/// (the scope checklist), not an in-session one.
fn bind_target(smudgy: &mut Smudgy, session_id: SessionId, target: BindTarget) -> Task<Message> {
    let Some(server_name) = smudgy
        .sessions
        .get(session_id)
        .map(|session| session.server_name.clone())
    else {
        return Task::none();
    };
    set_scope_entry(&mut smudgy.map_scopes, target, &server_name, true);
    commit_scope_change(smudgy)
}

/// A script created a non-ephemeral area in this session; associate it with the
/// session's server entry (silently — creation is deliberate). Gated on being
/// signed in, since only then is a non-ephemeral create a cloud-tier area (a
/// signed-out create lands in the local tier, which stays entry-isolated).
fn associate_created_area(
    smudgy: &mut Smudgy,
    session_id: SessionId,
    area_id: AreaId,
) -> Task<Message> {
    if !smudgy.account.handles().snapshot.get().signed_in {
        return Task::none();
    }
    let Some((server_name, target)) = smudgy.sessions.get(session_id).and_then(|session| {
        let mapper = session.mapper.as_ref()?;
        let target = bind_target_for_area(mapper, area_id)?;
        Some((session.server_name.clone(), target))
    }) else {
        return Task::none();
    };
    if target_scope(&smudgy.map_scopes, target, &server_name) == ScopeState::Here {
        return Task::none();
    }
    set_scope_entry(&mut smudgy.map_scopes, target, &server_name, true);
    commit_scope_change(smudgy)
}

/// Associate a deliberately created/promoted cloud atlas with this session's
/// server entry. Local atlases remain entry-isolated.
fn associate_created_atlas(
    smudgy: &mut Smudgy,
    session_id: SessionId,
    atlas_id: AtlasId,
) -> Task<Message> {
    let Some(session) = smudgy.sessions.get(session_id) else {
        return Task::none();
    };
    let Some(mapper) = session.mapper.as_ref() else {
        return Task::none();
    };
    if mapper.atlas_storage(&atlas_id) != Some(smudgy_cloud::MapStorage::Cloud) {
        return Task::none();
    }
    let server_name = session.server_name.clone();
    let target = BindTarget::Atlas(atlas_id);
    if target_scope(&smudgy.map_scopes, target, &server_name) == ScopeState::Here {
        return Task::none();
    }
    set_scope_entry(&mut smudgy.map_scopes, target, &server_name, true);
    commit_scope_change(smudgy)
}

/// Persist and fan out an authoritative daemon-side scope change: save the
/// store, push each entry's exclusions to every live mapper, and mirror the new
/// store into every open map editor so their trees and checklists agree. The
/// daemon-origin twin of the editor's `ScopeAssociationsChanged` handling.
fn commit_scope_change(smudgy: &mut Smudgy) -> Task<Message> {
    if let Err(e) = smudgy.map_scopes.save() {
        log::warn!("Failed to persist map scopes: {e}");
    }
    apply_scope_exclusions(smudgy);
    let scopes = smudgy.map_scopes.clone();
    let mirror: Vec<Task<Message>> = smudgy
        .map_editor_windows
        .keys()
        .copied()
        .map(|id| {
            Task::done(Message::MapEditorWindowMessage(
                id,
                map_editor_window::Message::ScopesReplaced(scopes.clone()),
            ))
        })
        .collect();
    Task::batch(mirror)
}

/// Wakes every live mapper's sync engine so credential changes (login,
/// logout) take effect immediately instead of on the next poll.
fn poke_all_mappers(smudgy: &Smudgy) {
    for (_, session) in smudgy.sessions.iter() {
        if let Some(mapper) = &session.mapper {
            mapper.sync_now();
        }
    }
}

fn view(smudgy: &Smudgy, id: window::Id) -> Element<'_, Message> {
    if let Some(window) = smudgy.smudgy_windows.get(&id) {
        // Each window derives its view of the daemon-owned drag state: the
        // live flag (temporary header bands, press-surface resets), the
        // tracked modifier state, and — for the hovered window only — the
        // classified target its overlay renders.
        let drag = windows::smudgy_window::DragViewContext {
            live: smudgy.tab_drag.is_some(),
            modifiers: smudgy.window_tracker.modifiers(),
            target: smudgy
                .tab_drag
                .as_ref()
                .and_then(|drag| drag.hover.as_ref())
                .filter(|hover| hover.window == id)
                .and_then(|hover| hover.target.as_ref()),
        };
        let content = center(
            window
                .view(&smudgy.sessions, drag)
                .map(move |message| Message::SmudgyWindowMessage(id, message)),
        );
        return if client_rounded_frame() {
            // The window surface is transparent and this container paints the
            // actual window frame: rounded top corners + hairline border
            // while floating, a plain opaque fill while maximized or
            // fullscreen (the frame chrome disappears, exactly like GTK
            // squares off a maximized headerbar). The 1px padding keeps
            // content off the border line.
            let squared = window.is_maximized() || window.is_fullscreen();
            iced::widget::container(content)
                .width(iced::Length::Fill)
                .height(iced::Length::Fill)
                .padding(if squared { 0.0 } else { 1.0 })
                .style(if squared {
                    theme::builtins::container::opaque
                } else {
                    theme::builtins::container::window_frame
                })
                .into()
        } else {
            content.into()
        };
    }
    let content: Element<'_, Message> = if let Some(window) = smudgy.automations_windows.get(&id) {
        center(
            window
                .view()
                .map(move |message| Message::AutomationsWindowMessage(id, message)),
        )
        .into()
    } else if let Some(window) = smudgy.map_editor_windows.get(&id) {
        center(
            window
                .view()
                .map(move |message| Message::MapEditorWindowMessage(id, message)),
        )
        .into()
    } else if let Some(window) = smudgy.settings_windows.get(&id) {
        center(
            window
                .view()
                .map(move |message| Message::SettingsWindowMessage(id, message)),
        )
        .into()
    } else {
        text(i18n::t!("window-none-open")).into()
    };
    if client_rounded_frame() {
        // With the surface clear color transparent (see the daemon `style`),
        // secondary windows — natively decorated, never rounded client-side —
        // just need their background painted back.
        iced::widget::container(content)
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .style(theme::builtins::container::opaque)
            .into()
    } else {
        content
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smudgy_cloud::Uuid;

    fn area(n: u128) -> AreaId {
        AreaId(Uuid::from_u128(n))
    }

    fn ts(secs: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(secs, 0).expect("valid timestamp")
    }

    fn local(area_id: AreaId, disabled: bool, secs: i64) -> MapAreaPref {
        MapAreaPref {
            area_id,
            disabled,
            updated_at: ts(secs),
        }
    }

    fn srv(area_id: AreaId, disabled: bool, secs: i64) -> AreaPref {
        AreaPref {
            area_id,
            disabled,
            updated_at: ts(secs),
        }
    }

    #[test]
    fn stamp_marks_only_real_flips() {
        let mut prefs = HashMap::new();
        prefs.insert(area(1), local(area(1), true, 10)); // already disabled
        // Disable 1 again (no-op) and 2 (new).
        let set: HashSet<AreaId> = [area(1), area(2)].into_iter().collect();
        let changed = stamp_area_pref_changes(&mut prefs, &set, ts(100));
        assert_eq!(changed, vec![(area(2), true)]);
        // The unchanged area keeps its original timestamp (not restamped).
        assert_eq!(prefs[&area(1)].updated_at, ts(10));
        assert!(prefs[&area(2)].disabled);
    }

    #[test]
    fn stamp_records_unmute_as_explicit_false() {
        let mut prefs = HashMap::new();
        prefs.insert(area(1), local(area(1), true, 10));
        let set: HashSet<AreaId> = HashSet::new(); // enable everything
        let changed = stamp_area_pref_changes(&mut prefs, &set, ts(100));
        assert_eq!(changed, vec![(area(1), false)]);
        // Un-mute is an explicit timestamped false row, not a deletion.
        assert!(!prefs[&area(1)].disabled);
        assert_eq!(prefs[&area(1)].updated_at, ts(100));
    }

    #[test]
    fn merge_server_newer_is_adopted() {
        let mut prefs = HashMap::new();
        prefs.insert(area(1), local(area(1), true, 10));
        let pushes =
            merge_server_area_prefs(&mut prefs, &[srv(area(1), false, 20)], &HashSet::new());
        assert!(pushes.is_empty());
        assert!(!prefs[&area(1)].disabled);
        assert_eq!(prefs[&area(1)].updated_at, ts(20));
    }

    #[test]
    fn merge_local_newer_is_pushed_and_kept() {
        let mut prefs = HashMap::new();
        prefs.insert(area(1), local(area(1), true, 30));
        let pushes =
            merge_server_area_prefs(&mut prefs, &[srv(area(1), false, 20)], &HashSet::new());
        assert_eq!(pushes, vec![(area(1), true)]);
        assert!(prefs[&area(1)].disabled);
    }

    #[test]
    fn merge_adopts_server_only_and_pushes_local_only_disabled() {
        let mut prefs = HashMap::new();
        prefs.insert(area(2), local(area(2), true, 30)); // local-only disabled
        prefs.insert(area(3), local(area(3), false, 30)); // local-only enabled
        let pushes = merge_server_area_prefs(&mut prefs, &[srv(area(1), true, 5)], &HashSet::new());
        // Server-only row adopted.
        assert!(prefs[&area(1)].disabled);
        // A local-only *disabled* pref is pushed; a local-only *enabled* one is
        // not (server-absent already means enabled).
        assert!(pushes.contains(&(area(2), true)));
        assert!(!pushes.iter().any(|(id, _)| *id == area(3)));
    }

    #[test]
    fn merge_never_repushes_a_parked_area() {
        // The 4XX loop regression: a locally-disabled pref for an area the
        // server refuses (local-tier map, revoked grant) must stop being
        // pushed once parked — every 90s reconcile re-attempted it forever.
        let mut prefs = HashMap::new();
        prefs.insert(area(2), local(area(2), true, 30));
        prefs.insert(area(4), local(area(4), true, 30));
        let parked: HashSet<AreaId> = [area(2)].into_iter().collect();
        let pushes = merge_server_area_prefs(&mut prefs, &[], &parked);
        // The parked area is skipped but its local pref survives untouched;
        // the unparked one still pushes.
        assert_eq!(pushes, vec![(area(4), true)]);
        assert!(prefs[&area(2)].disabled);
        // A server row for a parked area still merges normally (parking only
        // gates the local-only push).
        let pushes = merge_server_area_prefs(&mut prefs, &[srv(area(2), false, 99)], &parked);
        assert!(!pushes.iter().any(|(id, _)| *id == area(2)));
        assert!(
            !prefs[&area(2)].disabled,
            "server-newer row adopted despite parking"
        );
    }

    #[test]
    fn disabled_set_is_only_the_true_prefs() {
        let mut prefs = HashMap::new();
        prefs.insert(area(1), local(area(1), true, 1));
        prefs.insert(area(2), local(area(2), false, 1));
        let set = disabled_set_from_prefs(&prefs);
        assert!(set.contains(&area(1)));
        assert!(!set.contains(&area(2)));
    }

    #[test]
    fn legacy_disabled_list_migrates_to_prefs() {
        let settings = Settings {
            disabled_map_areas: vec![area(7)],
            map_area_prefs: Vec::new(),
            ..Settings::default()
        };
        let prefs = load_area_prefs(&settings);
        assert_eq!(prefs.len(), 1);
        assert!(prefs[&area(7)].disabled);
    }

    #[test]
    fn explicit_prefs_take_priority_over_legacy_list() {
        let settings = Settings {
            disabled_map_areas: vec![area(7)],
            map_area_prefs: vec![local(area(9), true, 5)],
            ..Settings::default()
        };
        let prefs = load_area_prefs(&settings);
        // The timestamped prefs win; the legacy list is ignored when present.
        assert_eq!(prefs.len(), 1);
        assert!(prefs.contains_key(&area(9)));
    }
}
