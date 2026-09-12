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
#[cfg(feature = "web-audio-cpal")]
use smudgy_audio::{MixerOutputFailure, MixerOutputFailureReceiver};
use smudgy_cloud::cloud_api::{AreaPref, CloudApiClient};
use smudgy_cloud::{AreaId, AtlasId, CloudError, MapStorage, Mapper};
use smudgy_core::models::map_scopes::{MapScopes, ScopeState};
#[cfg(feature = "web-audio-cpal")]
use smudgy_core::models::settings::{AudioGainSettings, AudioSettings};
use smudgy_core::models::settings::{MapAreaPref, Settings};
use smudgy_core::session::runtime::pane::{
    MAIN_PANE_KEY, PaneKey, PanePlacement, SplitDirection, TabPosition,
};
use smudgy_core::session::ui_command::{
    PaneCommand, UiCommand, UiCommandEnvelope, UiCommandReceiver,
};
use smudgy_core::session::{SessionEvent, SessionId, TaggedSessionEvent};

// Core session imports
use windows::automations_window::{
    AutomationsWindow, Event as AutomationsWindowEvent, Focus as AutomationsFocus,
};
use windows::settings_window::{self, Event as SettingsWindowEvent, SettingsWindow};
use windows::smudgy_window::SmudgyWindow;

#[cfg(feature = "web-audio-cpal")]
mod application_audio;
mod assets;
mod cloud_account;
mod discord_presence;
mod i18n;
mod images;
mod package_requirements;
mod package_update_checker;
mod pane_drag;
mod pane_groups;
pub mod prefs;
mod session_store;
pub mod terminal_buffer;
mod update;
mod widgets;
mod win_chrome;
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
/// (which talks to the dev API) is tagged "DEV BUILD"; prod-like prereleases
/// identify their channel and version. PTB and nightly titles also carry the
/// build timestamp and, when available, Git commit. The channel decision lives
/// in `core` so the title and the API/data-dir defaults can't drift. A clean
/// release gets the bare title.
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
        smudgy_core::models::settings::BuildChannel::PublicTestBuild => {
            i18n::t!(
                "window-main-public-test-build",
                "version" => env!("CARGO_PKG_VERSION"),
                "build" => preview_build_stamp()
            )
        }
        smudgy_core::models::settings::BuildChannel::Nightly => {
            i18n::t!(
                "window-main-nightly",
                "version" => env!("CARGO_PKG_VERSION"),
                "build" => preview_build_stamp()
            )
        }
        smudgy_core::models::settings::BuildChannel::Release => "smudgy".to_string(),
    }
}

fn preview_build_stamp() -> String {
    smudgy_core::BUILD_COMMIT.map_or_else(
        || smudgy_core::BUILD_TIME_UTC.to_string(),
        |commit| format!("{} ({commit})", smudgy_core::BUILD_TIME_UTC),
    )
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

/// The session binding requested for the app-wide Automations window. While the OS window is
/// still opening, later requests replace this value so one delayed `window::open` result cannot
/// create a second window or land on an obsolete session.
#[derive(Debug, Clone, PartialEq, Eq)]
struct AutomationsContext {
    server_name: String,
    session_id: SessionId,
    profile_name: String,
    /// Where the request that asked for this context wants the window to land, once its initial
    /// loads have run. `None` is the ordinary open, which keeps the window's own default view.
    focus: Option<AutomationsFocus>,
}

/// An Automations native window whose asynchronous `window::open` completion has not registered
/// application state yet. Keeping its id lets an intervening native close cancel the exact open.
#[derive(Debug, Clone, PartialEq, Eq)]
struct OpeningAutomationsWindow {
    id: window::Id,
    context: AutomationsContext,
}

// Main application state
struct Smudgy {
    #[cfg(feature = "web-audio-cpal")]
    audio_status: AudioBootStatus,
    #[cfg(feature = "web-audio-cpal")]
    pending_audio_announcement: Option<String>,
    #[cfg(feature = "web-audio-cpal")]
    terminal_audio_failure_presented: bool,
    #[cfg(feature = "web-audio-cpal")]
    audio_panel: AudioPanelState,
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
    /// Main native windows requested through `window::open` but not registered by their deferred
    /// completion yet. A close that overtakes `NewSmudgyWindow` removes the id here so the late
    /// completion cannot resurrect an unclaimed application window.
    opening_smudgy_windows: HashSet<window::Id>,
    /// The app owns one Automations window; opening it for another session rebuilds the state
    /// inside this same native window.
    automations_window: Option<(window::Id, AutomationsWindow)>,
    /// The latest requested context while the singleton OS window is being created.
    automations_window_opening: Option<OpeningAutomationsWindow>,
    /// Stamped on every task and subscription message produced by an Automations state. Rebuilding
    /// the window increments it, so a result from a task started before the rebuild is dropped.
    automations_context_generation: u64,
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
    /// The last main window that is waiting for the Automations terminal guard. It remains alive
    /// until that guard confirms close; cancellation releases it back to ordinary use.
    main_window_close_after_automations: Option<window::Id>,
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

#[cfg(feature = "web-audio-cpal")]
#[derive(Clone, Debug)]
enum AudioBootStatus {
    Physical,
    Unavailable(Arc<str>),
    Failed(Arc<str>),
}

#[cfg(feature = "web-audio-cpal")]
impl AudioBootStatus {
    fn from_availability(availability: &application_audio::ApplicationAudioAvailability) -> Self {
        match availability {
            application_audio::ApplicationAudioAvailability::Physical => Self::Physical,
            application_audio::ApplicationAudioAvailability::Unavailable(cause) => {
                Self::Unavailable(cause.to_string().into())
            }
            application_audio::ApplicationAudioAvailability::InfrastructureUnavailable(cause) => {
                Self::Unavailable(Arc::clone(cause))
            }
        }
    }

    fn banner(&self) -> Option<String> {
        match self {
            Self::Physical => None,
            Self::Unavailable(cause) => Some(i18n::t!(
                "audio-output-unavailable",
                "cause" => cause.as_ref()
            )),
            // Backend details remain in the log. The same concise localized
            // event message is used for the banner, panel notice, and screen
            // reader announcement.
            Self::Failed(_) => Some(i18n::t!("audio-notice-output-dead")),
        }
    }
}

#[cfg(feature = "web-audio-cpal")]
fn apply_terminal_audio_failure(
    status: &mut AudioBootStatus,
    notice: &mut Option<String>,
    presented: &mut bool,
    failure: MixerOutputFailure,
) -> Option<String> {
    if *presented {
        log::debug!("ignoring duplicate terminal physical audio output notification");
        return None;
    }
    *presented = true;
    let cause: Arc<str> = application_audio::ApplicationAudioControlError::OutputFailed(failure)
        .to_string()
        .into();
    log::error!("physical audio output terminalized: {cause}");
    *status = AudioBootStatus::Failed(cause);
    let message = i18n::t!("audio-notice-output-dead");
    *notice = Some(message.clone());
    Some(message)
}

#[cfg(feature = "web-audio-cpal")]
fn audio_output_failure_task(receiver: Option<MixerOutputFailureReceiver>) -> Task<Message> {
    receiver.map_or_else(Task::none, |receiver| {
        Task::run(receiver, Message::AudioOutputTerminated)
    })
}

#[cfg(feature = "web-audio-cpal")]
fn audio_announcement_window<T>(
    tracker: &pane_drag::WindowTracker,
    main_windows: &BTreeMap<window::Id, T>,
) -> Option<window::Id> {
    tracker
        .mru_order()
        .into_iter()
        .find(|id| main_windows.contains_key(id))
        .or_else(|| main_windows.keys().next().copied())
}

#[cfg(feature = "web-audio-cpal")]
fn announce_terminal_audio_failure<T>(
    tracker: &pane_drag::WindowTracker,
    main_windows: &BTreeMap<window::Id, T>,
    pending: &mut Option<String>,
    message: String,
) -> Task<Message> {
    use iced_runtime::window::AnnouncementPriority::Assertive;

    if let Some(window_id) = audio_announcement_window(tracker, main_windows) {
        window::announce(window_id, message, Assertive)
    } else {
        *pending = Some(message);
        Task::none()
    }
}

#[cfg(feature = "web-audio-cpal")]
fn announce_pending_audio_failure(
    pending: &mut Option<String>,
    window_id: window::Id,
) -> Task<Message> {
    use iced_runtime::window::AnnouncementPriority::Assertive;

    pending.take().map_or_else(Task::none, |message| {
        window::announce(window_id, message, Assertive)
    })
}

#[cfg(feature = "web-audio-cpal")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioControlRoute {
    Physical,
    PreferenceOnly,
    Failed,
}

#[cfg(feature = "web-audio-cpal")]
fn audio_control_route(status: &AudioBootStatus) -> AudioControlRoute {
    match status {
        AudioBootStatus::Physical => AudioControlRoute::Physical,
        AudioBootStatus::Unavailable(_) => AudioControlRoute::PreferenceOnly,
        AudioBootStatus::Failed(_) => AudioControlRoute::Failed,
    }
}

#[cfg(feature = "web-audio-cpal")]
fn audio_control_route_with_local_emulation(
    status: &AudioBootStatus,
    local_emulated: bool,
) -> AudioControlRoute {
    match audio_control_route(status) {
        AudioControlRoute::Physical if local_emulated => AudioControlRoute::PreferenceOnly,
        route => route,
    }
}

#[cfg(feature = "web-audio-cpal")]
fn audio_control_route_for_target(smudgy: &Smudgy, target: &AudioTarget) -> AudioControlRoute {
    let local_emulated = match target {
        AudioTarget::Master => false,
        AudioTarget::Session { id, .. } | AudioTarget::Package { id, .. } => {
            let Some(session) = smudgy.sessions.get(*id) else {
                return AudioControlRoute::Failed;
            };
            session.audio_is_emulated()
        }
    };
    audio_control_route_with_local_emulation(&smudgy.audio_status, local_emulated)
}

/// Daemon-owned state behind the Settings window's Audio pane. The policy
/// itself is app-global; only the pane's keyboard-focus bookkeeping is
/// per-window, keyed by the hosting settings window's id.
#[cfg(feature = "web-audio-cpal")]
#[derive(Clone, Debug)]
struct AudioPanelState {
    focused_widgets: HashMap<window::Id, iced::widget::Id>,
    preferences: AudioSettings,
    master_live: AudioGainSettings,
    notice: Option<String>,
    persistence_generation: u64,
    persistence_dirty: bool,
}

/// Whether `window_id` is a settings window currently showing the Audio pane
/// — the gate for every audio-control message and the pane's Tab traversal.
#[cfg(feature = "web-audio-cpal")]
fn audio_settings_pane_open(smudgy: &Smudgy, window_id: window::Id) -> bool {
    smudgy
        .settings_windows
        .get(&window_id)
        .is_some_and(|window| window.tab() == settings_window::Tab::Audio)
}

#[cfg(feature = "web-audio-cpal")]
#[derive(Clone, Debug)]
enum AudioTarget {
    Master,
    Session {
        id: SessionId,
        server: Arc<str>,
        profile: Arc<str>,
    },
    Package {
        id: SessionId,
        server: Arc<str>,
        profile: Arc<str>,
        owner: Arc<str>,
        name: Arc<str>,
        action_key: u64,
    },
}

#[derive(Debug, Clone)]
enum Message {
    RequestCloseWindow(window::Id),
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
    /// Open (or focus) the Settings window on its Audio pane — the toolbar
    /// button and the reserved keyboard chord both land here.
    #[cfg(feature = "web-audio-cpal")]
    OpenAudioSettings,
    /// A fresh settings window opened for [`Message::OpenAudioSettings`]:
    /// register it and land it on the Audio pane.
    #[cfg(feature = "web-audio-cpal")]
    NewAudioSettingsWindow(window::Id),
    /// The shared physical mixer reached an absorbing terminal failure. This
    /// is a one-shot event; recoverable endpoint interruptions stay internal.
    #[cfg(feature = "web-audio-cpal")]
    AudioOutputTerminated(MixerOutputFailure),
    #[cfg(feature = "web-audio-cpal")]
    AudioPanelTraverse {
        window_id: window::Id,
        backwards: bool,
    },
    #[cfg(feature = "web-audio-cpal")]
    AudioControl {
        window_id: window::Id,
        target: AudioTarget,
        widget_id: iced::widget::Id,
        action: widgets::audio_gain::Action,
    },
    #[cfg(feature = "web-audio-cpal")]
    PersistAudioPreferences {
        generation: u64,
        window_id: window::Id,
        preference_only: bool,
    },
    #[cfg(feature = "web-audio-cpal")]
    AudioPreferencesPersisted {
        generation: u64,
        window_id: window::Id,
        preference_only: bool,
        result: Result<(), String>,
    },
    /// An event from a session's runtime stream, routed straight to the
    /// session store (whatever window hosts the session's pane repaints from
    /// the shared state).
    SessionEvent(TaggedSessionEvent),
    UiCommand(UiCommandEnvelope),
    /// A session-level action carrying no window context: task continuations
    /// from store-routed updates and daemon fan-outs (settings changes,
    /// script reloads, widget wake-ups).
    SessionAction(SessionId, session_store::Message),
    /// A granted package update's staging task settled: on success, live-reload
    /// the server's sessions so they pick the staged version up from cache; on
    /// failure, surface a toast — the user granted, so silence would read as
    /// "nothing happened" (`name` names the package for that toast).
    PackageUpdateStaged {
        server_name: String,
        name: String,
        result: Result<(), String>,
    },
    /// The once-per-open background package-update check settled: route its
    /// toasts to the window hosting the owning session and echo its notices
    /// into that session's terminal.
    PackageCheckCompleted {
        session_id: SessionId,
        report: package_update_checker::CheckReport,
    },
    NewSmudgyWindow(window::Id),
    /// The raw HWND of a freshly opened main window, delivered so the
    /// Windows-only native hooks can be installed on it: the Restart Manager
    /// shutdown watcher (`win_rm`) and the `WM_NCHITTEST` chrome
    /// (`win_chrome`). Both are no-ops elsewhere.
    HookNativeWindow(window::Id, u64),
    // Handled in `update()` (opens a window -> `NewSmudgyWindow`), mirroring
    // the other `Create*Window` variants; no sender currently emits it.
    #[allow(dead_code)]
    CreateSmudgyWindow,
    AutomationsWindowMessage {
        id: window::Id,
        generation: u64,
        message: windows::automations_window::Message,
    },
    NewAutomationsWindow(window::Id),
    CreateAutomationsWindow {
        server_name: Arc<String>,
        session_id: smudgy_core::session::SessionId,
        /// Captured with the originating session request. Do not look it up again after this
        /// queued message runs: the session may have closed or changed in the meantime.
        profile_name: String,
        /// Where the requesting click wants the window to land (a package's parameters, say).
        focus: Option<AutomationsFocus>,
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
    /// Fresh geometry sampled before the native window is allowed to close.
    RememberWindowAndClose {
        id: window::Id,
        geometry: workspace::dto::Geometry,
        normal_geometry: Option<workspace::dto::Geometry>,
        maximized: bool,
    },
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
        // The daemon closes main windows explicitly so the last one can first route through the
        // singleton Automations window's unsaved/durability terminal guard.
        exit_on_close_request: false,
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

/// The Automations singleton must receive a close request before the native window disappears so
/// its existing unsaved-draft guard can decide whether to close it.
fn automations_window_settings() -> window::Settings {
    window::Settings {
        exit_on_close_request: false,
        ..secondary_window_settings(Size::new(900.0, 560.0))
    }
}

#[cfg(feature = "web-audio-cpal")]
fn finish_run_with_audio<E>(
    iced_result: Result<(), E>,
    audio_report: application_audio::ApplicationAudioShutdownReport,
) -> anyhow::Result<()>
where
    E: Into<anyhow::Error>,
{
    match (iced_result, audio_report.is_clean()) {
        (Ok(()), true) => Ok(()),
        (Ok(()), false) => anyhow::bail!("Web Audio shutdown failed: {audio_report}"),
        (Err(iced), true) => Err(iced.into()),
        (Err(iced), false) => {
            let iced = iced.into();
            anyhow::bail!("iced failed: {iced}; Web Audio shutdown also failed: {audio_report}")
        }
    }
}

fn init(
    #[cfg(feature = "web-audio-cpal")] audio: Option<application_audio::ApplicationAudioController>,
    #[cfg(feature = "web-audio-cpal")] audio_output_failures: Option<MixerOutputFailureReceiver>,
    #[cfg(feature = "web-audio-cpal")] audio_status: AudioBootStatus,
    #[cfg(feature = "web-audio-cpal")] master_live: AudioGainSettings,
) -> (Smudgy, Task<Message>) {
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
    #[cfg(feature = "web-audio-cpal")]
    let sessions = match audio {
        Some(audio) => {
            SessionStore::with_ui_commands_and_audio(account.handles(), ui_command_bus, audio)
        }
        None => SessionStore::with_ui_commands(account.handles(), ui_command_bus),
    };
    #[cfg(not(feature = "web-audio-cpal"))]
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

    // Reopen the main window at its last placement. Profiles remain an explicit
    // choice on the connect surface; opening one restores its saved pane layout.
    let workspace_mirror = workspace::autosave::Mirror::default();
    let restore_state = workspace::restore::RestoreState::default();
    let preferences = if workspace_enabled {
        workspace::preferences::load()
    } else {
        Default::default()
    };
    let mut window_settings = smudgy_window_settings();
    if let Some(geometry) = &preferences.geometry {
        let (position, size) = workspace::restore::clamp_geometry(
            geometry,
            workspace::restore::virtual_screen_bounds(),
            Size::new(640.0, 400.0),
        );
        window_settings.size = size;
        if let Some(position) = position {
            window_settings.position = window::Position::Specific(position);
        }
    }
    window_settings.maximized = preferences.maximized;
    let (id, open) = window::open(window_settings);
    let mut initial = SmudgyWindow::new(id, account.handles());
    initial.seed_maximized(preferences.maximized);
    let smudgy_windows = BTreeMap::from([(id, initial)]);
    let open_tasks = vec![open.map(Message::NewSmudgyWindow)];
    #[cfg(feature = "web-audio-cpal")]
    let audio_output_failure_task = audio_output_failure_task(audio_output_failures);

    (
        Smudgy {
            #[cfg(feature = "web-audio-cpal")]
            audio_status,
            #[cfg(feature = "web-audio-cpal")]
            pending_audio_announcement: None,
            #[cfg(feature = "web-audio-cpal")]
            terminal_audio_failure_presented: false,
            #[cfg(feature = "web-audio-cpal")]
            audio_panel: AudioPanelState {
                focused_widgets: HashMap::new(),
                preferences: settings.audio.clone(),
                master_live,
                notice: None,
                persistence_generation: 0,
                persistence_dirty: false,
            },
            account,
            discord,
            sessions,
            ui_commands,
            pending_pane_commands: VecDeque::new(),
            retired_panes: HashSet::new(),
            pending_ordered_pane_closes: HashSet::new(),
            last_ui_command_seq: HashMap::new(),
            smudgy_windows,
            opening_smudgy_windows: HashSet::from([id]),
            automations_window: None,
            automations_window_opening: None,
            automations_context_generation: 0,
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
            main_window_close_after_automations: None,
            workspace: workspace_mirror,
            restore: restore_state,
            layout_saver: workspace::layouts::DebouncedSaver::new(),
        },
        Task::batch([
            Task::batch(open_tasks),
            account_task.map(Message::Account),
            reconcile_task,
            update_check_task,
            #[cfg(feature = "web-audio-cpal")]
            audio_output_failure_task,
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

    #[cfg(feature = "web-audio-cpal")]
    let mut application_audio = application_audio::ApplicationAudio::start();
    #[cfg(feature = "web-audio-cpal")]
    let mut audio_status = AudioBootStatus::from_availability(&application_audio.availability());
    #[cfg(feature = "web-audio-cpal")]
    let audio_controller = application_audio.controller();
    #[cfg(feature = "web-audio-cpal")]
    let mut master_live = AudioGainSettings::default();
    #[cfg(feature = "web-audio-cpal")]
    if matches!(audio_status, AudioBootStatus::Physical) {
        match audio_controller.set_master_state(
            startup_settings.audio.master.linear(),
            startup_settings.audio.master.muted,
        ) {
            Ok(state) => {
                master_live = AudioGainSettings {
                    volume: (state.linear() * 100.0).round().clamp(0.0, 100.0) as u8,
                    muted: state.is_muted(),
                };
            }
            Err(error) => {
                // Audio is optional. A failed physical policy application
                // disables live audio for this launch before any session
                // runtime can be published. The failed launch's acknowledged
                // live state remains the physical default; the saved desired
                // policy stays separate and editable for the next start.
                log::error!("Web Audio master policy could not be applied: {error}");
                audio_status = if matches!(
                    error,
                    application_audio::ApplicationAudioControlError::OutputFailed(_)
                ) {
                    AudioBootStatus::Failed(error.to_string().into())
                } else {
                    AudioBootStatus::Unavailable(error.to_string().into())
                };
                let cause = match &error {
                    application_audio::ApplicationAudioControlError::NotApplicable(cause) => {
                        cause.clone()
                    }
                    application_audio::ApplicationAudioControlError::OutputFailed(failure) => {
                        application_audio::SessionAudioUnavailable::System(
                            smudgy_audio::SystemMixerUnavailable::from(
                                smudgy_audio::SystemMixerStartError::DriverFailed(failure.clone()),
                            ),
                        )
                    }
                    _ => {
                        application_audio::SessionAudioUnavailable::Policy(error.to_string().into())
                    }
                };
                audio_controller.force_new_sessions_unavailable(cause);
            }
        }
    }
    #[cfg(feature = "web-audio-cpal")]
    match &audio_status {
        AudioBootStatus::Physical => {}
        AudioBootStatus::Unavailable(cause) => {
            log::warn!("physical audio unavailable for this launch; restart to retry: {cause}");
        }
        AudioBootStatus::Failed(cause) => {
            log::error!("Web Audio failed for this launch; restart to retry: {cause}");
        }
    }

    #[cfg(feature = "web-audio-cpal")]
    let audio_output_failures =
        std::sync::Mutex::new(application_audio.take_output_failure_events());

    #[cfg(feature = "web-audio-cpal")]
    let daemon = iced::daemon(
        move || {
            // iced's boot callback is typed as `Fn`, even though it is called
            // once. Interior mutability moves the unique receiver into that
            // one initializer task without making it cloneable application
            // state or a recurring subscription.
            let audio_output_failures = audio_output_failures
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            init(
                Some(audio_controller.clone()),
                audio_output_failures,
                audio_status.clone(),
                master_live,
            )
        },
        update,
        view,
    );
    #[cfg(not(feature = "web-audio-cpal"))]
    let daemon = iced::daemon(init, update, view);

    let iced_result = daemon
        .theme(|smudgy: &Smudgy, window_id| {
            if smudgy.smudgy_windows.contains_key(&window_id)
                || smudgy.automations_window_id() == Some(window_id)
            {
                // Palette-aware: re-evaluated per frame, so theme changes in
                // the Preferences tab apply live. Automations joins the main
                // windows here so its editor and Smudgy-owned LSP overlays use
                // one coherent light/dark palette.
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
            if let Some(window) = smudgy.automations_window(window_id) {
                i18n::t!("window-automations", "server" => window.server_name())
            } else if let Some(window) = smudgy.map_editor_windows.get(&window_id) {
                window.title()
            } else {
                main_window_title()
            }
        })
        .run();

    log::info!("Application closing");

    // Iced has dropped every Automations window, so all window-scoped language services
    // have begun their nonblocking teardown. Join their tracked reapers before process exit
    // so Deno can release each temporary data directory even when the app itself is closing.
    smudgy_script::language_service_worker::join_language_service_reapers();

    #[cfg(not(feature = "web-audio-cpal"))]
    {
        smudgy_core::session::connection::shutdown_io_runtime();
        smudgy_core::session::runtime::join_runtime_threads();
        iced_result?;
    }

    #[cfg(feature = "web-audio-cpal")]
    {
        // Always consume the proof-bearing retained-host shutdown path,
        // including when either worker could not start or iced itself failed.
        let audio_report = application_audio.shutdown();
        finish_run_with_audio(iced_result, audio_report)?;
    }

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
        Subscription::batch(smudgy.automations_window.iter().map(|(id, window)| {
            window
                .subscription()
                .with((*id, smudgy.automations_context_generation))
        }))
        .map(
            |((id, generation), message)| Message::AutomationsWindowMessage {
                id,
                generation,
                message,
            },
        ),
        window::close_requests().map(Message::RequestCloseWindow),
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

    #[cfg(feature = "web-audio-cpal")]
    if smudgy
        .settings_windows
        .values()
        .any(|window| window.tab() == settings_window::Tab::Audio)
    {
        subs.push(iced::event::listen_with(audio_panel_tab_event));
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

#[cfg(feature = "web-audio-cpal")]
fn audio_panel_tab_event(
    event: iced::Event,
    status: iced::event::Status,
    window_id: window::Id,
) -> Option<Message> {
    if status != iced::event::Status::Ignored {
        return None;
    }
    let iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
        key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
        modifiers,
        ..
    }) = event
    else {
        return None;
    };
    let backwards = modifiers == iced::keyboard::Modifiers::SHIFT;
    (modifiers.is_empty() || backwards).then_some(Message::AudioPanelTraverse {
        window_id,
        backwards,
    })
}

#[cfg(feature = "web-audio-cpal")]
fn audio_panel_id(window_id: window::Id) -> iced::widget::Id {
    iced::widget::Id::from(format!("audio-panel-{window_id:?}"))
}

#[cfg(feature = "web-audio-cpal")]
fn audio_window_root_id(window_id: window::Id) -> iced::widget::Id {
    iced::widget::Id::from(format!("audio-window-root-{window_id:?}"))
}

#[cfg(feature = "web-audio-cpal")]
fn audio_master_id(window_id: window::Id) -> iced::widget::Id {
    iced::widget::Id::from(format!("audio-master-{window_id:?}"))
}

#[cfg(feature = "web-audio-cpal")]
fn audio_scroll_id(window_id: window::Id) -> iced::widget::Id {
    iced::widget::Id::from(format!("audio-scroll-{window_id:?}"))
}

#[cfg(feature = "web-audio-cpal")]
fn scoped_audio_focus(window_id: window::Id, target: iced::widget::Id) -> Task<Message> {
    iced::advanced::widget::operate(iced::advanced::widget::operation::scope(
        audio_window_root_id(window_id),
        iced::advanced::widget::operation::focusable::focus::<()>(target),
    ))
    .discard()
}

#[cfg(feature = "web-audio-cpal")]
fn scoped_audio_step(window_id: window::Id, backwards: bool) -> Task<Message> {
    let operation: Box<dyn iced::advanced::widget::Operation<()>> = if backwards {
        Box::new(iced::advanced::widget::operation::focusable::focus_previous::<()>())
    } else {
        Box::new(iced::advanced::widget::operation::focusable::focus_next::<()>())
    };
    iced::advanced::widget::operate(iced::advanced::widget::operation::scope(
        audio_panel_id(window_id),
        operation,
    ))
    .discard()
}

#[cfg(feature = "web-audio-cpal")]
fn audio_focus_ids(smudgy: &Smudgy, window_id: window::Id) -> Vec<iced::widget::Id> {
    let mut ids = vec![audio_master_id(window_id)];
    for (session_id, session) in smudgy.sessions.iter() {
        ids.push(audio_session_id(window_id, session_id));
        ids.extend(
            session
                .audio_packages()
                .iter()
                .filter(|row| audio_package_row_is_visible(row) && !row.trusted)
                .map(|row| audio_package_id(window_id, session_id, row.ui_key)),
        );
    }
    ids
}

#[cfg(feature = "web-audio-cpal")]
fn next_audio_focus_target(
    ids: &[iced::widget::Id],
    focused: Option<&iced::widget::Id>,
    backwards: bool,
) -> Option<(iced::widget::Id, bool)> {
    let current = focused.and_then(|focused| ids.iter().position(|id| id == focused));
    let (next, direct) = match current {
        Some(current) if backwards => (
            current
                .checked_sub(1)
                .unwrap_or(ids.len().saturating_sub(1)),
            current == 0,
        ),
        Some(current) => ((current + 1) % ids.len(), current + 1 == ids.len()),
        None if backwards => (ids.len().checked_sub(1)?, true),
        None => (0, true),
    };
    Some((ids.get(next)?.clone(), direct))
}

#[cfg(feature = "web-audio-cpal")]
fn reveal_audio_focus(window_id: window::Id, target: iced::widget::Id) -> Task<Message> {
    iced::advanced::widget::operate(iced::advanced::widget::operation::scope(
        audio_panel_id(window_id),
        RevealAudioControl::new(audio_scroll_id(window_id), target),
    ))
    .discard()
}

#[cfg(feature = "web-audio-cpal")]
struct RevealAudioControl {
    scroll: iced::widget::Id,
    target: iced::widget::Id,
    target_bounds: Option<iced::Rectangle>,
}

#[cfg(feature = "web-audio-cpal")]
impl RevealAudioControl {
    fn new(scroll: iced::widget::Id, target: iced::widget::Id) -> Self {
        Self {
            scroll,
            target,
            target_bounds: None,
        }
    }
}

#[cfg(feature = "web-audio-cpal")]
impl iced::advanced::widget::Operation<()> for RevealAudioControl {
    fn focusable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: iced::Rectangle,
        _state: &mut dyn iced::advanced::widget::operation::Focusable,
    ) {
        if id == Some(&self.target) {
            self.target_bounds = Some(bounds);
        }
    }

    fn traverse(
        &mut self,
        operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<()>),
    ) {
        operate(self);
    }

    fn finish(&self) -> iced::advanced::widget::operation::Outcome<()> {
        self.target_bounds
            .map_or(iced::advanced::widget::operation::Outcome::None, |target| {
                iced::advanced::widget::operation::Outcome::Chain(Box::new(
                    RevealAudioControlApply {
                        scroll: self.scroll.clone(),
                        target,
                    },
                ))
            })
    }
}

#[cfg(feature = "web-audio-cpal")]
struct RevealAudioControlApply {
    scroll: iced::widget::Id,
    target: iced::Rectangle,
}

#[cfg(feature = "web-audio-cpal")]
impl iced::advanced::widget::Operation<()> for RevealAudioControlApply {
    fn scrollable(
        &mut self,
        id: Option<&iced::widget::Id>,
        bounds: iced::Rectangle,
        content_bounds: iced::Rectangle,
        translation: iced::Vector,
        state: &mut dyn iced::advanced::widget::operation::Scrollable,
    ) {
        if id != Some(&self.scroll) {
            return;
        }
        let current = translation.y;
        let top = self.target.y - content_bounds.y;
        let bottom = self.target.y + self.target.height - content_bounds.y;
        let next = if top < current {
            top
        } else if bottom > current + bounds.height {
            bottom - bounds.height
        } else {
            current
        };
        state.scroll_to(
            iced::advanced::widget::operation::scrollable::AbsoluteOffset {
                x: None,
                y: Some(next.max(0.0)),
            },
        );
    }

    fn traverse(
        &mut self,
        operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<()>),
    ) {
        operate(self);
    }
}

#[cfg(feature = "web-audio-cpal")]
fn audio_action_gain(
    current: AudioGainSettings,
    action: widgets::audio_gain::Action,
) -> Option<AudioGainSettings> {
    match action {
        widgets::audio_gain::Action::Focus => None,
        widgets::audio_gain::Action::SetVolume(volume) => Some(current.with_volume(volume)),
        widgets::audio_gain::Action::ToggleMuted => Some(current.with_muted(!current.muted)),
    }
}

#[cfg(feature = "web-audio-cpal")]
fn audio_preference_gain(settings: &AudioSettings, target: &AudioTarget) -> AudioGainSettings {
    match target {
        AudioTarget::Master => settings.master,
        AudioTarget::Session {
            server, profile, ..
        } => settings
            .session(server, profile)
            .map_or_else(AudioGainSettings::default, |row| row.gain),
        AudioTarget::Package {
            server,
            profile,
            owner,
            name,
            ..
        } => settings
            .session(server, profile)
            .and_then(|session| session.package(owner, name))
            .map_or_else(AudioGainSettings::default, |package| package.gain),
    }
}

#[cfg(feature = "web-audio-cpal")]
fn audio_action_changes_preference(
    settings: &AudioSettings,
    target: &AudioTarget,
    desired: AudioGainSettings,
    action: widgets::audio_gain::Action,
) -> bool {
    let stored = audio_preference_gain(settings, target);
    match action {
        widgets::audio_gain::Action::SetVolume(_) => stored.volume != desired.volume,
        widgets::audio_gain::Action::ToggleMuted => stored.muted != desired.muted,
        widgets::audio_gain::Action::Focus => false,
    }
}

#[cfg(feature = "web-audio-cpal")]
fn update_audio_preference(
    settings: &mut AudioSettings,
    target: &AudioTarget,
    gain: AudioGainSettings,
) {
    match target {
        AudioTarget::Master => settings.master = gain,
        AudioTarget::Session {
            server, profile, ..
        } => settings.session_mut(server, profile).gain = gain,
        AudioTarget::Package {
            server,
            profile,
            owner,
            name,
            ..
        } => {
            settings
                .session_mut(server, profile)
                .package_mut(owner, name)
                .gain = gain;
        }
    }
}

#[cfg(feature = "web-audio-cpal")]
static AUDIO_SETTINGS_WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(feature = "web-audio-cpal")]
fn persist_audio_preferences(settings: &AudioSettings) -> Result<(), String> {
    let _guard = AUDIO_SETTINGS_WRITES
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    smudgy_core::models::settings::merge_audio_settings(settings).map_err(|error| error.to_string())
}

#[cfg(feature = "web-audio-cpal")]
fn persist_updated_audio_preferences(
    current: &AudioSettings,
    target: &AudioTarget,
    gain: AudioGainSettings,
    action: widgets::audio_gain::Action,
    persist: impl FnOnce(&AudioSettings) -> Result<(), String>,
) -> Result<(AudioSettings, AudioGainSettings), String> {
    let mut next = current.clone();
    let mut stored = audio_preference_gain(&next, target);
    match action {
        widgets::audio_gain::Action::SetVolume(_) => stored.volume = gain.volume,
        widgets::audio_gain::Action::ToggleMuted => stored.muted = gain.muted,
        widgets::audio_gain::Action::Focus => unreachable!("focus never persists audio policy"),
    }
    update_audio_preference(&mut next, target, stored);
    next.normalize();
    persist(&next)?;
    Ok((next, stored))
}

#[cfg(feature = "web-audio-cpal")]
fn audio_widget_is_current(
    expected: Option<iced::widget::Id>,
    supplied: &iced::widget::Id,
) -> bool {
    expected.as_ref() == Some(supplied)
}

#[cfg(feature = "web-audio-cpal")]
fn audio_package_action_is_current(row: &session_store::AudioPackageRow, supplied: u64) -> bool {
    row.action_key == supplied
}

#[cfg(feature = "web-audio-cpal")]
fn audio_package_row_is_visible(row: &session_store::AudioPackageRow) -> bool {
    row.audio_used
}

#[cfg(feature = "web-audio-cpal")]
fn exact_audio_target_gain(smudgy: &Smudgy, target: &AudioTarget) -> Option<AudioGainSettings> {
    match target {
        AudioTarget::Master => Some(
            if matches!(smudgy.audio_status, AudioBootStatus::Unavailable(_)) {
                smudgy.audio_panel.preferences.master
            } else {
                smudgy.audio_panel.master_live
            },
        ),
        AudioTarget::Session {
            id,
            server,
            profile,
        } => {
            let session = smudgy.sessions.get(*id)?;
            if session.server_name != server.as_ref() || session.profile_name != profile.as_ref() {
                return None;
            }
            Some(session.audio_gain())
        }
        AudioTarget::Package {
            id,
            server,
            profile,
            owner,
            name,
            action_key,
        } => {
            let session = smudgy.sessions.get(*id)?;
            if session.server_name != server.as_ref() || session.profile_name != profile.as_ref() {
                return None;
            }
            let row = session
                .audio_packages()
                .iter()
                .find(|row| !row.trusted && row.owner == *owner && row.name == *name)?;
            if !audio_package_action_is_current(row, *action_key) {
                return None;
            }
            Some(row.gain)
        }
    }
}

#[cfg(feature = "web-audio-cpal")]
fn current_audio_widget_id(
    smudgy: &Smudgy,
    window_id: window::Id,
    target: &AudioTarget,
) -> Option<iced::widget::Id> {
    match target {
        AudioTarget::Master => Some(audio_master_id(window_id)),
        AudioTarget::Session {
            id,
            server,
            profile,
        } => {
            let session = smudgy.sessions.get(*id)?;
            (session.server_name == server.as_ref() && session.profile_name == profile.as_ref())
                .then(|| audio_session_id(window_id, *id))
        }
        AudioTarget::Package {
            id,
            server,
            profile,
            owner,
            name,
            action_key,
        } => {
            let session = smudgy.sessions.get(*id)?;
            if session.server_name != server.as_ref() || session.profile_name != profile.as_ref() {
                return None;
            }
            let row = session.audio_packages().iter().find(|row| {
                !row.trusted
                    && row.owner.eq_ignore_ascii_case(owner)
                    && row.name.eq_ignore_ascii_case(name)
            })?;
            if !audio_package_action_is_current(row, *action_key) {
                return None;
            }
            Some(audio_package_id(window_id, *id, row.ui_key))
        }
    }
}

#[cfg(feature = "web-audio-cpal")]
fn set_preference_only_live_row(
    smudgy: &mut Smudgy,
    target: &AudioTarget,
    gain: AudioGainSettings,
) {
    match target {
        AudioTarget::Master => smudgy.audio_panel.master_live = gain,
        AudioTarget::Session { id, .. } => {
            if let Some(session) = smudgy.sessions.get_mut(*id) {
                session.set_audio_preference_only(gain);
            }
        }
        AudioTarget::Package {
            id, owner, name, ..
        } => {
            if let Some(session) = smudgy.sessions.get_mut(*id) {
                let _ = session.set_package_audio_preference_only(owner, name, gain);
            }
        }
    }
}

#[cfg(feature = "web-audio-cpal")]
fn apply_live_audio_gain(
    smudgy: &mut Smudgy,
    target: &AudioTarget,
    desired: AudioGainSettings,
    action: widgets::audio_gain::Action,
) -> Result<AudioGainSettings, application_audio::ApplicationAudioControlError> {
    match target {
        AudioTarget::Master => {
            let state = match action {
                widgets::audio_gain::Action::SetVolume(_) => {
                    smudgy.sessions.set_master_linear(desired.linear())?
                }
                widgets::audio_gain::Action::ToggleMuted => {
                    smudgy.sessions.set_master_muted(desired.muted)?
                }
                widgets::audio_gain::Action::Focus => unreachable!(),
            };
            let gain = AudioGainSettings {
                volume: (state.linear() * 100.0).round().clamp(0.0, 100.0) as u8,
                muted: state.is_muted(),
            };
            smudgy.audio_panel.master_live = gain;
            Ok(gain)
        }
        AudioTarget::Session {
            id,
            server,
            profile,
        } => {
            let session = smudgy
                .sessions
                .get_mut(*id)
                .ok_or(application_audio::ApplicationAudioControlError::UnknownSession)?;
            if session.server_name != server.as_ref() || session.profile_name != profile.as_ref() {
                return Err(application_audio::ApplicationAudioControlError::StaleSession);
            }
            session.set_audio_state(desired)?;
            Ok(desired)
        }
        AudioTarget::Package {
            id,
            server,
            profile,
            owner,
            name,
            action_key,
        } => {
            let session = smudgy
                .sessions
                .get_mut(*id)
                .ok_or(application_audio::ApplicationAudioControlError::UnknownSession)?;
            if session.server_name != server.as_ref() || session.profile_name != profile.as_ref() {
                return Err(application_audio::ApplicationAudioControlError::StaleSession);
            }
            if !session.audio_packages().iter().any(|row| {
                !row.trusted
                    && row.owner.eq_ignore_ascii_case(owner)
                    && row.name.eq_ignore_ascii_case(name)
                    && audio_package_action_is_current(row, *action_key)
            }) {
                return Err(application_audio::ApplicationAudioControlError::StalePackage);
            }
            session.set_package_audio_state(owner, name, desired)?;
            Ok(desired)
        }
    }
}

#[cfg(feature = "web-audio-cpal")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioAnnouncement {
    TargetClosed,
    AppliedSaveFailed,
    ChangeFailed,
    PreferenceSaved,
    PreferenceSaveFailed,
}

#[cfg(feature = "web-audio-cpal")]
fn localized_audio_announcement(
    translator: smudgy_i18n::Translator,
    announcement: AudioAnnouncement,
) -> (String, iced_runtime::window::AnnouncementPriority) {
    use iced_runtime::window::AnnouncementPriority::{Assertive, Polite};

    match announcement {
        AudioAnnouncement::TargetClosed => (
            translator.translate("audio-notice-target-closed"),
            Assertive,
        ),
        AudioAnnouncement::AppliedSaveFailed => (
            translator.translate("audio-announcement-applied-save-failed"),
            Assertive,
        ),
        AudioAnnouncement::ChangeFailed => {
            (translator.translate("audio-announcement-failed"), Assertive)
        }
        AudioAnnouncement::PreferenceSaved => (
            translator.translate("audio-notice-preference-saved"),
            Polite,
        ),
        AudioAnnouncement::PreferenceSaveFailed => (
            translator.translate("audio-announcement-preference-save-failed"),
            Assertive,
        ),
    }
}

#[cfg(feature = "web-audio-cpal")]
fn audio_control_feedback(
    smudgy: &mut Smudgy,
    window_id: window::Id,
    notice: String,
    announcement: AudioAnnouncement,
) -> Task<Message> {
    smudgy.audio_panel.notice = Some(notice);
    let (text, priority) =
        i18n::with_translator(|translator| localized_audio_announcement(translator, announcement));

    window::announce(window_id, text, priority)
}

#[cfg(feature = "web-audio-cpal")]
fn schedule_audio_preferences_persist(
    smudgy: &mut Smudgy,
    window_id: window::Id,
    preference_only: bool,
) -> Task<Message> {
    smudgy.audio_panel.persistence_generation =
        smudgy.audio_panel.persistence_generation.wrapping_add(1);
    let generation = smudgy.audio_panel.persistence_generation;
    smudgy.audio_panel.persistence_dirty = true;
    Task::perform(
        async move {
            tokio::time::sleep(Duration::from_millis(180)).await;
            (generation, window_id, preference_only)
        },
        |(generation, window_id, preference_only)| Message::PersistAudioPreferences {
            generation,
            window_id,
            preference_only,
        },
    )
}

#[cfg(feature = "web-audio-cpal")]
fn handle_audio_control(
    smudgy: &mut Smudgy,
    window_id: window::Id,
    target: AudioTarget,
    widget_id: iced::widget::Id,
    action: widgets::audio_gain::Action,
) -> Task<Message> {
    if !audio_settings_pane_open(smudgy, window_id) {
        return Task::none();
    }
    if !audio_widget_is_current(
        current_audio_widget_id(smudgy, window_id, &target),
        &widget_id,
    ) {
        return audio_control_feedback(
            smudgy,
            window_id,
            i18n::t!("audio-notice-target-closed"),
            AudioAnnouncement::TargetClosed,
        );
    }
    if action == widgets::audio_gain::Action::Focus {
        smudgy
            .audio_panel
            .focused_widgets
            .insert(window_id, widget_id.clone());
        return scoped_audio_focus(window_id, widget_id);
    }
    let Some(current) = exact_audio_target_gain(smudgy, &target) else {
        return audio_control_feedback(
            smudgy,
            window_id,
            i18n::t!("audio-notice-target-closed"),
            AudioAnnouncement::TargetClosed,
        );
    };
    let Some(desired) = audio_action_gain(current, action) else {
        return Task::none();
    };
    if desired == current
        && !audio_action_changes_preference(
            &smudgy.audio_panel.preferences,
            &target,
            desired,
            action,
        )
    {
        return Task::none();
    }

    let announcement = match audio_control_route_for_target(smudgy, &target) {
        AudioControlRoute::Physical => {
            match apply_live_audio_gain(smudgy, &target, desired, action) {
                Ok(applied) => {
                    match persist_updated_audio_preferences(
                        &smudgy.audio_panel.preferences,
                        &target,
                        applied,
                        action,
                        |_| Ok(()),
                    ) {
                        Ok((next, _stored)) => {
                            smudgy.audio_panel.preferences = next;
                            return schedule_audio_preferences_persist(smudgy, window_id, false);
                        }
                        Err(error) => {
                            smudgy.audio_panel.notice = Some(i18n::t!(
                                "audio-notice-applied-save-failed",
                                "error" => error
                            ));
                            AudioAnnouncement::AppliedSaveFailed
                        }
                    }
                }
                Err(error) => {
                    if let application_audio::ApplicationAudioControlError::OutputFailed(failure) =
                        &error
                    {
                        return Task::done(Message::AudioOutputTerminated(failure.clone()));
                    }
                    smudgy.audio_panel.notice = Some(i18n::t!(
                        "audio-notice-failed",
                        "error" => error.to_string()
                    ));
                    AudioAnnouncement::ChangeFailed
                }
            }
        }
        AudioControlRoute::PreferenceOnly => {
            match persist_updated_audio_preferences(
                &smudgy.audio_panel.preferences,
                &target,
                desired,
                action,
                |_| Ok(()),
            ) {
                Ok((next, stored)) => {
                    smudgy.audio_panel.preferences = next;
                    set_preference_only_live_row(smudgy, &target, stored);
                    return schedule_audio_preferences_persist(smudgy, window_id, true);
                }
                Err(error) => {
                    smudgy.audio_panel.notice = Some(i18n::t!(
                        "audio-notice-preference-save-failed",
                        "error" => error
                    ));
                    AudioAnnouncement::PreferenceSaveFailed
                }
            }
        }
        AudioControlRoute::Failed => return Task::none(),
    };
    let notice = smudgy
        .audio_panel
        .notice
        .clone()
        .expect("audio control feedback sets a notice");
    audio_control_feedback(smudgy, window_id, notice, announcement)
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
            publish_workspace_snapshot(smudgy, None, None);
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

/// Query while the native window still exists. This catches move/resize then
/// immediate quit, without adding per-pixel move events to the idle subscription.
fn remember_and_close_window(id: window::Id) -> Task<Message> {
    window::position(id).then(move |position| {
        window::size(id).then(move |size| {
            window::scale_factor(id).then(move |scale| {
                window::is_maximized(id).then(move |maximized| {
                    let geometry = workspace::dto::Geometry {
                        x: position.map_or(0.0, |point| point.x),
                        y: position.map_or(0.0, |point| point.y),
                        width: size.width,
                        height: size.height,
                        scale,
                    };
                    #[cfg(windows)]
                    let normal = window::raw_id::<Message>(id)
                        .map(move |raw| workspace::preferences::native_normal_geometry(raw, scale));
                    #[cfg(not(windows))]
                    let normal = Task::done(None);
                    normal.map(move |normal| {
                        let (normal_geometry, maximized) = normal
                            .map_or((None, maximized), |(geometry, maximized)| {
                                (Some(geometry), maximized)
                            });
                        Message::RememberWindowAndClose {
                            id,
                            geometry: geometry.clone(),
                            normal_geometry,
                            maximized,
                        }
                    })
                })
            })
        })
    })
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

/// Serialize a server's footprint (the active server unless explicitly supplied)
/// and hand it to the writer worker as that server's last-session snapshot — how each server
/// comes to hold the most recent arrangement in which it was active. With
/// no active session (or nothing captured for it) the snapshot is settled
/// as taken and nothing is written: the files on disk keep their
/// arrangements, which is exactly what the clean connect view should leave
/// behind.
///
/// `ack` makes the write awaited (the quit flush); it is always resolved —
/// on publish, on every skip, and on every failure path — so a waiter can
/// never hang. Returns whether new bytes were actually published.
fn publish_workspace_snapshot(
    smudgy: &mut Smudgy,
    server: Option<&str>,
    ack: Option<workspace::writer::Ack>,
) -> bool {
    // Snapshots read the layout model at a settled point: flush any rebuild
    // marks first (idempotent, and cheap when already settled).
    for window in smudgy.smudgy_windows.values_mut() {
        window.flush_grid_rebuild();
    }
    let force = ack.is_some();
    let resolve = |ack: Option<workspace::writer::Ack>| {
        if let Some(ack) = ack {
            if let Some(writer) = workspace::writer::global() {
                writer.flush(ack);
            } else {
                let _ = ack.send(());
            }
        }
    };
    if let Some((id, _)) = smudgy.workspace.window_entries().next()
        && let Some(win) = smudgy.smudgy_windows.get(&id)
        && !win.is_fullscreen()
        && let Some(geometry) = smudgy.workspace.geometry_of(id)
    {
        workspace::preferences::remember_window(geometry, win.is_maximized());
    }
    let server = server
        .map(str::to_owned)
        .or_else(|| active_server_name(smudgy));
    let Some((path, snapshot)) = server.and_then(|server| {
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
                smudgy_core::session::runtime::pane::PaneNamespace::Package(pkg) => {
                    dto::Namespace::Package {
                        owner: pkg.owner.to_string(),
                        name: pkg.name.to_string(),
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
        live.windows
            .push(build_live_window(win, &smudgy.sessions, stable_id));
    }
    live
}

fn build_live_window(
    win: &SmudgyWindow,
    sessions: &SessionStore,
    stable_id: u64,
) -> workspace::apply::LiveWindow {
    let layout = win.layout();
    let mut groups = Vec::new();
    for gid in layout.groups_depth_first() {
        let Some(tabs) = layout.tabs(gid) else {
            continue;
        };
        let mut group = Vec::with_capacity(tabs.len());
        for tab in tabs {
            let Some(&pane) = tab.binding() else {
                continue;
            };
            let descriptor = if pane.key == MAIN_PANE_KEY {
                None
            } else {
                sessions
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
    workspace::apply::LiveWindow {
        stable_id,
        empty: win.is_visually_empty(),
        groups,
    }
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

fn rollback_spawned_sessions(sessions: &mut SessionStore, spawned: &mut Vec<SessionId>) {
    for session_id in spawned.drain(..) {
        sessions.shutdown_and_remove(session_id);
    }
}

fn open_layout_spawn_batch(
    sessions: &mut SessionStore,
    spawns: &[workspace::apply::SpawnSlot],
    source: &workspace::TemplateSource,
) -> Result<Vec<SessionId>, ()> {
    let mut spawned = Vec::with_capacity(spawns.len());
    for spawn in spawns {
        match sessions.open_session(spawn.server.clone(), spawn.profile.clone(), spawn.connect) {
            Ok(session_id) => {
                spawned.push(session_id);
                log::info!(
                    "[layouts] spawned {} ({}/{}) for {source}",
                    session_id,
                    spawn.server,
                    spawn.profile
                );
            }
            Err(error) => {
                log::error!(
                    "[layouts] could not spawn {}/{} for {source}: {error}",
                    spawn.server,
                    spawn.profile
                );
                rollback_spawned_sessions(sessions, &mut spawned);
                return Err(());
            }
        }
    }
    Ok(spawned)
}

/// Restore only the newly opened profile; never spawn the other saved sessions.
fn restore_profile_layout(smudgy: &mut Smudgy, session_id: SessionId) -> Task<Message> {
    let live = build_live_workspace(smudgy);
    let Some(session) = live
        .sessions
        .iter()
        .find(|session| session.id == session_id)
    else {
        return Task::none();
    };
    let Some(template) = workspace::last_session::read(&session.server) else {
        return Task::none();
    };
    let template = workspace::apply::for_opened_profile(&template, session);
    let mode = workspace::apply::ApplyMode::User { initiating: None };
    let Ok(plan) = workspace::apply::plan_apply(&template, &live, mode, &HashMap::new()) else {
        return Task::none();
    };
    if !plan.is_executable() || !plan.close_sessions.is_empty() {
        return Task::none();
    }
    if let Err(error) = workspace::apply::validate_conservation(&template, &live, mode, &plan) {
        log::warn!("[workspace] cannot automatically restore profile layout: {error}");
        return Task::none();
    }
    execute_layout_apply(smudgy, &plan)
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
    let mut batch_spawned = Vec::new();
    let plan = if plan.spawns.is_empty() {
        plan
    } else {
        // Spawn through the normal open path with each slot's stored
        // intent (an online slot reconnects exactly as the Connect button
        // would), then re-project so the fresh sessions bind and the plan
        // is validated against the workspace actually being mutated.
        batch_spawned = match open_layout_spawn_batch(&mut smudgy.sessions, &plan.spawns, source) {
            Ok(spawned) => spawned,
            Err(()) => return Task::none(),
        };
        let live = build_live_workspace(smudgy);
        match workspace::apply::plan_apply(&template, &live, mode, answers) {
            Ok(plan) => plan,
            Err(error) => {
                log::info!("[layouts] replan of {source} failed after spawning: {error}");
                rollback_spawned_sessions(&mut smudgy.sessions, &mut batch_spawned);
                return Task::none();
            }
        }
    };
    if !plan.is_executable() {
        log::info!("[layouts] plan for {source} did not settle; not applying");
        rollback_spawned_sessions(&mut smudgy.sessions, &mut batch_spawned);
        return Task::none();
    }
    let live = build_live_workspace(smudgy);
    if let Err(error) = workspace::apply::validate_conservation(&template, &live, mode, &plan) {
        log::info!("[layouts] conservation check refused {source}: {error}");
        rollback_spawned_sessions(&mut smudgy.sessions, &mut batch_spawned);
        return Task::none();
    }
    workspace::preferences::remember_server(server);
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
    #[cfg(feature = "web-audio-cpal")]
    if smudgy.audio_panel.persistence_dirty {
        match persist_audio_preferences(&smudgy.audio_panel.preferences) {
            Ok(()) => smudgy.audio_panel.persistence_dirty = false,
            Err(error) => {
                log::error!("final audio preference flush failed during quit: {error}");
            }
        }
    }
    let (ack, done) = tokio::sync::oneshot::channel();
    // `publish_workspace_snapshot` resolves the ack on every path, so the
    // awaited task below always completes — and the await is bounded
    // besides, so a wedged disk write cannot hold the exit hostage (the
    // last completed write stands; the atomic replace cannot tear).
    let _ = publish_workspace_snapshot(smudgy, None, Some(ack));
    Some(Task::perform(
        workspace::writer::await_ack_bounded(done, workspace::writer::QUIT_FLUSH_TIMEOUT),
        |()| Message::WorkspaceQuitFlushed,
    ))
}

/// A requested Automations context is usable only while its session is still open.
fn automations_context_is_live(smudgy: &Smudgy, context: &AutomationsContext) -> bool {
    smudgy.sessions.get(context.session_id).is_some()
}

fn all_main_windows_are_closing(
    registered: impl IntoIterator<Item = window::Id>,
    opening: impl IntoIterator<Item = window::Id>,
    closing: &HashSet<window::Id>,
) -> bool {
    let mut any = false;
    for id in registered.into_iter().chain(opening) {
        any = true;
        if !closing.contains(&id) {
            return false;
        }
    }
    any
}

fn cancel_opening_automations_window(
    opening: &mut Option<OpeningAutomationsWindow>,
    id: window::Id,
) -> bool {
    if opening.as_ref().is_some_and(|pending| pending.id == id) {
        *opening = None;
        true
    } else {
        false
    }
}

fn take_opening_automations_context(
    opening: &mut Option<OpeningAutomationsWindow>,
    id: window::Id,
) -> Option<AutomationsContext> {
    let pending = opening.take()?;
    if pending.id == id {
        Some(pending.context)
    } else {
        // A cancelled old native open can complete after a replacement was requested. Preserve
        // the newer exact id/context so only its own completion may claim it.
        *opening = Some(pending);
        None
    }
}

fn automations_account_identity(
    account: &CloudAccount,
) -> (u64, bool, Option<smudgy_cloud::Uuid>, Option<String>) {
    let handles = account.handles();
    let snapshot = handles.snapshot.get();
    (
        handles.credentials.generation(),
        snapshot.signed_in,
        snapshot.profile.as_ref().map(|profile| profile.id),
        snapshot.nickname_text(),
    )
}

fn notify_automations_account_changed(smudgy: &mut Smudgy) -> Task<Message> {
    let generation = smudgy.automations_context_generation;
    let Some((id, window)) = smudgy.automations_window.as_mut() else {
        return Task::none();
    };
    let id = *id;
    // Clear private account-scoped state in this daemon turn. A queued notification leaves a gap
    // in which an old authenticated read can arrive after the credential already changed.
    let update = window.update(windows::automations_window::Message::AccountChanged);
    debug_assert!(update.event.is_none());
    update
        .task
        .map(move |message| Message::AutomationsWindowMessage {
            id,
            generation,
            message,
        })
}

/// Install a fresh Automations state in an existing or newly-created OS window. The generation is
/// advanced before any initialization task is mapped; therefore no result minted by the replaced
/// state can enter the new one, even though both states share the same iced window id.
fn install_automations_context(
    smudgy: &mut Smudgy,
    id: window::Id,
    context: AutomationsContext,
) -> Option<Task<Message>> {
    // The request and any dirty-state confirmation cross daemon turns. A session can close during
    // either gap, so check it at the last point before replacing window state.
    if !automations_context_is_live(smudgy, &context) {
        return None;
    }
    smudgy.automations_context_generation = smudgy.automations_context_generation.wrapping_add(1);
    let generation = smudgy.automations_context_generation;
    let window = AutomationsWindow::new_for_profile(
        id,
        context.server_name,
        smudgy.account.handles(),
        context.session_id,
        context.profile_name,
    );
    let task = window.init_with_focus(context.focus);
    smudgy.automations_window = Some((id, window));
    Some(task.map(move |message| Message::AutomationsWindowMessage {
        id,
        generation,
        message,
    }))
}

impl Smudgy {
    fn automations_window_id(&self) -> Option<window::Id> {
        self.automations_window.as_ref().map(|(id, _)| *id)
    }

    fn automations_window(&self, id: window::Id) -> Option<&AutomationsWindow> {
        self.automations_window
            .as_ref()
            .filter(|(current, _)| *current == id)
            .map(|(_, window)| window)
    }

    fn automations_window_mut(&mut self, id: window::Id) -> Option<&mut AutomationsWindow> {
        self.automations_window
            .as_mut()
            .filter(|(current, _)| *current == id)
            .map(|(_, window)| window)
    }
}

fn update_body(smudgy: &mut Smudgy, message: Message) -> Task<Message> {
    // QA forensics (debug builds only): change-gated OS capture-owner
    // sampling. Runs for every message; during a drag the motion message
    // stream gives it per-sample resolution.
    #[cfg(debug_assertions)]
    spike_log_capture_owner();
    match message {
        #[cfg(feature = "web-audio-cpal")]
        Message::OpenAudioSettings => {
            // Reuse an existing settings window (switched to the Audio pane)
            // rather than stacking copies, mirroring `CreateSettingsWindow`.
            if let Some((&id, _)) = smudgy.settings_windows.iter().next() {
                let master = audio_master_id(id);
                smudgy
                    .audio_panel
                    .focused_widgets
                    .insert(id, master.clone());
                Task::batch([
                    window::gain_focus(id),
                    Task::done(Message::SettingsWindowMessage(
                        id,
                        settings_window::Message::TabSelected(settings_window::Tab::Audio),
                    )),
                    scoped_audio_focus(id, master.clone()),
                    reveal_audio_focus(id, master),
                ])
            } else {
                let (_, task) = window::open(secondary_window_settings(Size::new(640.0, 480.0)));
                task.map(Message::NewAudioSettingsWindow)
            }
        }
        #[cfg(feature = "web-audio-cpal")]
        Message::NewAudioSettingsWindow(id) => {
            smudgy
                .settings_windows
                .insert(id, SettingsWindow::new(smudgy.account.handles()));
            let master = audio_master_id(id);
            smudgy
                .audio_panel
                .focused_widgets
                .insert(id, master.clone());
            Task::batch([
                Task::done(Message::SettingsWindowMessage(
                    id,
                    settings_window::Message::TabSelected(settings_window::Tab::Audio),
                )),
                scoped_audio_focus(id, master.clone()),
                reveal_audio_focus(id, master),
            ])
        }
        #[cfg(feature = "web-audio-cpal")]
        Message::AudioOutputTerminated(failure) => {
            let Some(message) = apply_terminal_audio_failure(
                &mut smudgy.audio_status,
                &mut smudgy.audio_panel.notice,
                &mut smudgy.terminal_audio_failure_presented,
                failure,
            ) else {
                return Task::none();
            };
            announce_terminal_audio_failure(
                &smudgy.window_tracker,
                &smudgy.smudgy_windows,
                &mut smudgy.pending_audio_announcement,
                message,
            )
        }
        #[cfg(feature = "web-audio-cpal")]
        Message::AudioPanelTraverse {
            window_id,
            backwards,
        } => {
            if audio_settings_pane_open(smudgy, window_id) {
                let ids = audio_focus_ids(smudgy, window_id);
                if ids.is_empty() {
                    return Task::none();
                }
                let Some((target, direct)) = next_audio_focus_target(
                    &ids,
                    smudgy.audio_panel.focused_widgets.get(&window_id),
                    backwards,
                ) else {
                    return Task::none();
                };
                smudgy
                    .audio_panel
                    .focused_widgets
                    .insert(window_id, target.clone());
                Task::batch([
                    if direct {
                        scoped_audio_focus(window_id, target.clone())
                    } else {
                        scoped_audio_step(window_id, backwards)
                    },
                    reveal_audio_focus(window_id, target),
                ])
            } else {
                Task::none()
            }
        }
        #[cfg(feature = "web-audio-cpal")]
        Message::AudioControl {
            window_id,
            target,
            widget_id,
            action,
        } => handle_audio_control(smudgy, window_id, target, widget_id, action),
        #[cfg(feature = "web-audio-cpal")]
        Message::PersistAudioPreferences {
            generation,
            window_id,
            preference_only,
        } => {
            if generation != smudgy.audio_panel.persistence_generation {
                return Task::none();
            }
            let settings = smudgy.audio_panel.preferences.clone();
            Task::perform(
                async move {
                    let result =
                        tokio::task::spawn_blocking(move || persist_audio_preferences(&settings))
                            .await
                            .unwrap_or_else(|error| {
                                Err(format!("audio settings writer stopped: {error}"))
                            });
                    (generation, window_id, preference_only, result)
                },
                |(generation, window_id, preference_only, result)| {
                    Message::AudioPreferencesPersisted {
                        generation,
                        window_id,
                        preference_only,
                        result,
                    }
                },
            )
        }
        #[cfg(feature = "web-audio-cpal")]
        Message::AudioPreferencesPersisted {
            generation,
            window_id,
            preference_only,
            result,
        } => {
            if generation != smudgy.audio_panel.persistence_generation {
                return Task::none();
            }
            match result {
                Ok(()) => {
                    smudgy.audio_panel.persistence_dirty = false;
                    if preference_only {
                        audio_control_feedback(
                            smudgy,
                            window_id,
                            i18n::t!("audio-notice-preference-saved"),
                            AudioAnnouncement::PreferenceSaved,
                        )
                    } else {
                        // A routine applied-and-saved change is the expected
                        // outcome; the updated row already shows it, so it
                        // earns no status line. Clear any stale failure text.
                        smudgy.audio_panel.notice = None;
                        Task::none()
                    }
                }
                Err(error) => {
                    smudgy.audio_panel.persistence_dirty = true;
                    if preference_only {
                        audio_control_feedback(
                            smudgy,
                            window_id,
                            i18n::t!(
                                "audio-notice-preference-save-failed",
                                "error" => error
                            ),
                            AudioAnnouncement::PreferenceSaveFailed,
                        )
                    } else {
                        audio_control_feedback(
                            smudgy,
                            window_id,
                            i18n::t!("audio-notice-applied-save-failed", "error" => error),
                            AudioAnnouncement::AppliedSaveFailed,
                        )
                    }
                }
            }
        }
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
        Message::RequestCloseWindow(id) => {
            if let Some(window) = smudgy.automations_window_mut(id) {
                // A close request supersedes any queued session switch.
                window.cancel_pending_context_switch();
                // Route the close in this daemon turn so a newer open request cannot overtake it.
                return update_body(
                    smudgy,
                    Message::AutomationsWindowMessage {
                        id,
                        generation: smudgy.automations_context_generation,
                        message: windows::automations_window::Message::RequestClose,
                    },
                );
            }
            if cancel_opening_automations_window(&mut smudgy.automations_window_opening, id) {
                // The native request can overtake `NewAutomationsWindow`. Cancel the exact open;
                // its late completion will be unclaimed and cannot resurrect the singleton.
                smudgy.automations_window_opening = None;
                return window::close(id);
            }
            if smudgy.smudgy_windows.contains_key(&id) {
                if !smudgy.closing_windows.insert(id) {
                    return Task::none();
                }
                let is_last_main = all_main_windows_are_closing(
                    smudgy.smudgy_windows.keys().copied(),
                    smudgy.opening_smudgy_windows.iter().copied(),
                    &smudgy.closing_windows,
                );
                if is_last_main {
                    if let Some((automations_id, window)) = smudgy.automations_window.as_mut() {
                        let automations_id = *automations_id;
                        window.cancel_pending_context_switch();
                        smudgy.main_window_close_after_automations = Some(id);
                        let generation = smudgy.automations_context_generation;
                        return Task::batch([
                            window::gain_focus(automations_id),
                            update_body(
                                smudgy,
                                Message::AutomationsWindowMessage {
                                    id: automations_id,
                                    generation,
                                    message: windows::automations_window::Message::RequestClose,
                                },
                            ),
                        ]);
                    }
                    if let Some(opening) = smudgy.automations_window_opening.take() {
                        // No Automations state exists yet, so there is no draft to confirm. Close
                        // the pending native tool window before allowing the last main to exit.
                        return Task::batch([
                            window::close(opening.id),
                            remember_and_close_window(id),
                        ]);
                    }
                }
                return remember_and_close_window(id);
            }
            if smudgy.opening_smudgy_windows.remove(&id) {
                // As with the Automations singleton, a native close may arrive before the open
                // completion registers application state.
                let close = window::close(id);
                if smudgy.smudgy_windows.is_empty()
                    && smudgy.opening_smudgy_windows.is_empty()
                    && smudgy.automations_window.is_none()
                    && smudgy.automations_window_opening.is_none()
                {
                    return Task::batch([close, iced::exit()]);
                }
                return close;
            }
            // Other tool windows retain the default native close behavior; their `CloseWindow`
            // event performs model cleanup below.
            Task::none()
        }
        Message::RememberWindowAndClose {
            id,
            geometry,
            normal_geometry,
            maximized,
        } => {
            if let Some(win) = smudgy.smudgy_windows.get_mut(&id) {
                let fullscreen = win.is_fullscreen();
                win.seed_maximized(maximized);
                let stored_geometry = normal_geometry.as_ref().unwrap_or(&geometry);
                for sample in [
                    workspace::autosave::GeometrySample::Position(Some(iced::Point::new(
                        stored_geometry.x,
                        stored_geometry.y,
                    ))),
                    workspace::autosave::GeometrySample::Size(Size::new(
                        stored_geometry.width,
                        stored_geometry.height,
                    )),
                    workspace::autosave::GeometrySample::Scale(stored_geometry.scale),
                ] {
                    smudgy.workspace.record_sample(None, id, sample);
                }
                if smudgy
                    .workspace
                    .window_entries()
                    .next()
                    .is_some_and(|(first, _)| first == id)
                    && !fullscreen
                {
                    workspace::preferences::remember_window(&geometry, maximized);
                    if let Some(normal) = normal_geometry {
                        workspace::preferences::remember_normal_geometry(normal);
                    }
                }
            }
            // Closing a main window can cascade through detached pane windows.
            // Preserve each affected server before that cascade removes its panes.
            let servers: std::collections::BTreeSet<_> = smudgy
                .smudgy_windows
                .get(&id)
                .into_iter()
                .flat_map(SmudgyWindow::hosted_main_sessions)
                .filter_map(|session| {
                    smudgy
                        .sessions
                        .get(session)
                        .map(|session| session.server_name.clone())
                })
                .collect();
            for server in servers {
                publish_workspace_snapshot(smudgy, Some(&server), None);
            }
            window::close(id)
        }
        Message::CloseWindow(id) => {
            smudgy.window_tracker.remove(id);
            smudgy.closing_windows.remove(&id);
            if smudgy.main_window_close_after_automations == Some(id) {
                smudgy.main_window_close_after_automations = None;
            }
            #[cfg(feature = "web-audio-cpal")]
            smudgy.audio_panel.focused_widgets.remove(&id);
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
            let opening_main_survives = smudgy
                .opening_smudgy_windows
                .iter()
                .any(|opening| !smudgy.closing_windows.contains(opening));
            let quit_flush = if smudgy.smudgy_windows.len() == 1
                && smudgy.smudgy_windows.contains_key(&id)
                && !opening_main_survives
            {
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
                    retire_automations_session_binding(smudgy, *session_id);
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
                if smudgy.smudgy_windows.is_empty() && !opening_main_survives {
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
            } else if smudgy.automations_window_id() == Some(id) {
                smudgy.automations_window = None;
                // A forced/unexpected native close is not confirmation to discard Automations
                // state. Release a last main window that was waiting on that confirmation.
                if let Some(main_id) = smudgy.main_window_close_after_automations.take() {
                    smudgy.closing_windows.remove(&main_id);
                }
                // Drop every in-flight result from the closed window: none may enter a later
                // window that happens to reuse this OS id.
                smudgy.automations_context_generation =
                    smudgy.automations_context_generation.wrapping_add(1);
                Task::none()
            } else if cancel_opening_automations_window(&mut smudgy.automations_window_opening, id)
            {
                Task::none()
            } else if smudgy.opening_smudgy_windows.remove(&id) {
                if smudgy.smudgy_windows.is_empty()
                    && smudgy.opening_smudgy_windows.is_empty()
                    && smudgy.automations_window.is_none()
                    && smudgy.automations_window_opening.is_none()
                {
                    iced::exit()
                } else {
                    Task::none()
                }
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
        Message::Account(msg) => {
            let before = automations_account_identity(&smudgy.account);
            let task = smudgy.account.update(msg).map(Message::Account);
            let after = automations_account_identity(&smudgy.account);
            if before == after {
                task
            } else {
                Task::batch([task, notify_automations_account_changed(smudgy)])
            }
        }
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
                Some(SmudgyWindowEvent::RequestCloseWindow) => Task::batch([
                    task,
                    // Route custom chrome through the same daemon-owned close path as an OS
                    // request. Closing the last main window must first honor Automations guards.
                    update_body(smudgy, Message::RequestCloseWindow(id)),
                ]),
                Some(SmudgyWindowEvent::ConfigurePackage {
                    session_id,
                    specifier,
                }) => {
                    // A session row's "Configure it now.": the package's parameters live in the
                    // Automations window, on this session's own profile.
                    let open = smudgy.sessions.get(session_id).map(|session| {
                        Task::done(Message::CreateAutomationsWindow {
                            server_name: Arc::new(session.server_name.clone()),
                            session_id,
                            profile_name: session.profile_name.clone(),
                            focus: Some(AutomationsFocus::PackageSettings(specifier)),
                        })
                    });
                    Task::batch([Some(task), open].into_iter().flatten())
                }
                Some(SmudgyWindowEvent::CreateNewScriptEditorWindow {
                    server_name,
                    session_id,
                }) => {
                    let open = smudgy.sessions.get(session_id).map(|session| {
                        Task::done(Message::CreateAutomationsWindow {
                            server_name,
                            session_id,
                            profile_name: session.profile_name.clone(),
                            focus: None,
                        })
                    });
                    Task::batch([Some(task), open].into_iter().flatten())
                }
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
                    // Preserve edits made immediately before closing the profile,
                    // before vacating its panes can make them disappear from capture.
                    if let Some(server) = smudgy
                        .sessions
                        .get(session_id)
                        .map(|session| session.server_name.clone())
                    {
                        publish_workspace_snapshot(smudgy, Some(&server), None);
                    }
                    Task::batch([task, close_session(smudgy, session_id)])
                }
                #[cfg(feature = "web-audio-cpal")]
                Some(SmudgyWindowEvent::OpenAudioPanel) => {
                    Task::batch([task, Task::done(Message::OpenAudioSettings)])
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
                Some(SmudgyWindowEvent::PackageReloadScripts { server_name }) => {
                    // The toast's Reload scripts takes the same live-reload path
                    // as the Automations window's ScriptsChanged: every session
                    // on the server reloads (serving the staged versions from
                    // cache, so the reload is near-instant).
                    let reload_tasks = smudgy
                        .sessions
                        .iter()
                        .filter(|(_, session)| session.server_name.as_str() == server_name.as_str())
                        .map(|(session_id, _)| {
                            Task::done(Message::SessionAction(
                                session_id,
                                session_store::Message::Reload,
                            ))
                        });
                    Task::batch([task, Task::batch(reload_tasks)])
                }
                Some(SmudgyWindowEvent::PackageUpdateDismissed {
                    server_name,
                    expected,
                    specifier,
                    version,
                }) => {
                    match smudgy_core::models::shared_packages::set_dismissed_update_version_if_unchanged(
                            &server_name,
                            &expected,
                            &version,
                        ) {
                        Ok(true) => {}
                        Ok(false) => log::info!(
                            "ignored a stale update dismissal for {specifier}: package state changed"
                        ),
                        Err(e) => log::warn!(
                            "failed to record the dismissed update for {specifier}: {e}"
                        ),
                    }
                    task
                }
                Some(SmudgyWindowEvent::PackageUpdatePinned {
                    server_name,
                    expected,
                    specifier,
                    version,
                }) => {
                    match smudgy_core::models::shared_packages::set_update_mode_if_unchanged(
                        &server_name,
                        &expected,
                        smudgy_core::models::shared_packages::UpdateMode::Pinned { version },
                    ) {
                        Ok(smudgy_core::models::shared_packages::Cas::Applied) => {}
                        Ok(smudgy_core::models::shared_packages::Cas::StateChanged) => log::info!(
                            "ignored a stale update pin for {specifier}: package state changed"
                        ),
                        Err(e) => log::warn!("failed to pin {specifier}: {e}"),
                    }
                    task
                }
                Some(SmudgyWindowEvent::PackageUpdateGranted { offer }) => {
                    // Prefetch first, then compare the complete evaluated lock row and commit the
                    // new consent + staged version together. An old toast cannot change a package
                    // the user edited, uninstalled, or reinstalled while it was visible.
                    let handles = smudgy.account.handles();
                    let client = smudgy_cloud::package_api::PackageApiClient::new(
                        handles.base_url.as_str(),
                        handles.credentials.clone(),
                    );
                    let server_name = offer.server_name.clone();
                    let name = offer.name.clone();
                    let stage = Task::perform(
                        package_update_checker::stage_offer(client, *offer),
                        move |result| Message::PackageUpdateStaged {
                            server_name: server_name.clone(),
                            name: name.clone(),
                            result,
                        },
                    );
                    Task::batch([task, stage])
                }
                Some(SmudgyWindowEvent::OpenAutomationsForServer { server_name }) => {
                    // The collapsed toast points at the Automations window, where
                    // the per-package update cards live. Any of the server's
                    // sessions works as the window's session context.
                    let open = smudgy
                        .sessions
                        .iter()
                        .find(|(_, session)| session.server_name.as_str() == server_name.as_str())
                        .map(|(session_id, session)| {
                            Task::done(Message::CreateAutomationsWindow {
                                server_name: Arc::new(server_name.clone()),
                                session_id,
                                profile_name: session.profile_name.clone(),
                                focus: None,
                            })
                        });
                    match open {
                        Some(open) => Task::batch([task, open]),
                        None => task,
                    }
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
                Some(SmudgyWindowEvent::RestoreProfileLayout { session }) => {
                    Task::batch([task, restore_profile_layout(smudgy, session)])
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
                publish_workspace_snapshot(smudgy, None, None);
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
                SessionEvent::ObservedServerChanged => {
                    // A session rewrote its server's observed.json sidecar:
                    // refresh any open Connect modal's copy so the metadata
                    // band tracks the file without a reopen.
                    if let Some(server) = smudgy
                        .sessions
                        .get(session_id)
                        .map(|session| session.server_name.clone())
                    {
                        for window in smudgy.smudgy_windows.values_mut() {
                            window.refresh_connect_observed(&server);
                        }
                    }
                    Task::none()
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
            // The once-per-open package-update check fires on the FIRST readiness
            // only (a reload re-emits `RuntimeReady`), and only now — the load's
            // own lockfile writes (`record_resolution`) are settled, so staging
            // can never race them. The task is built before the session adopts
            // its channel (which flips `has_runtime`), but runs after this
            // update cycle returns.
            let package_check = if runtime_ready
                && smudgy
                    .sessions
                    .get(session_id)
                    .is_some_and(|session| !session.has_runtime())
            {
                start_package_update_check(smudgy, session_id)
            } else {
                Task::none()
            };
            if let Some(session) = smudgy.sessions.get_mut(session_id) {
                let task = session
                    .update(session_store::Message::SessionEvent(event))
                    .map(move |msg| Message::SessionAction(session_id, msg));
                let task = Task::batch([task, package_check]);
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
        Message::PackageCheckCompleted { session_id, report } => {
            let reload_server = report.reload_server.clone();
            if let Some(session) = smudgy.sessions.get(session_id) {
                for notice in &report.notices {
                    session.echo_notice(notice.clone());
                }
            }
            if !report.toasts.is_empty() {
                // Offers surface in the window hosting the session whose open
                // triggered the check; a session torn down mid-check just drops
                // them (the parked outcomes re-surface on the next open).
                if let Some(window) = smudgy
                    .smudgy_windows
                    .values_mut()
                    .find(|window| window.hosts_session(session_id))
                {
                    for toast in report.toasts {
                        window.push_toast(toast);
                    }
                } else {
                    log::info!("package-update toasts dropped: no window hosts the session");
                }
            }
            match reload_server {
                Some(server_name) => Task::batch(
                    smudgy
                        .sessions
                        .iter()
                        .filter(|(_, session)| session.server_name == server_name)
                        .map(|(session_id, _)| {
                            Task::done(Message::SessionAction(
                                session_id,
                                session_store::Message::Reload,
                            ))
                        }),
                ),
                None => Task::none(),
            }
        }
        Message::PackageUpdateStaged {
            server_name,
            name,
            result,
        } => match result {
            Ok(()) => {
                // The staged versions are on disk; a live reload serves them
                // from cache, so it is near-instant.
                let reload_tasks = smudgy
                    .sessions
                    .iter()
                    .filter(|(_, session)| session.server_name.as_str() == server_name.as_str())
                    .map(|(session_id, _)| {
                        Task::done(Message::SessionAction(
                            session_id,
                            session_store::Message::Reload,
                        ))
                    });
                Task::batch(reload_tasks)
            }
            Err(e) => {
                // Prefetch failure or stale package state leaves the complete lock row unmoved.
                // The user GRANTED, though, so the failure must be visible: toast the window
                // hosting one of the server's sessions.
                log::warn!("failed to stage the granted update for {server_name}: {e}");
                let hosting_window = smudgy
                    .sessions
                    .iter()
                    .find(|(_, session)| session.server_name.as_str() == server_name.as_str())
                    .map(|(session_id, _)| session_id)
                    .and_then(|session_id| {
                        smudgy
                            .smudgy_windows
                            .values_mut()
                            .find(|window| window.hosts_session(session_id))
                    });
                if let Some(window) = hosting_window {
                    window.push_toast(components::toast::Toast::StageFailed { server_name, name });
                } else {
                    log::info!("stage-failure toast dropped: no window hosts the server");
                }
                Task::none()
            }
        },
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
            // A client-row link in the session asked for a package's parameters; windows are
            // the daemon's, so it answers here. (A click inside a pane arrives as the hosting
            // window's `ConfigurePackage` event instead; this is the store-routed path.)
            let configure = match &msg {
                session_store::Message::ConfigurePackage(specifier) => {
                    let specifier = specifier.clone();
                    smudgy
                        .sessions
                        .get(session_id)
                        .map_or_else(Task::none, |session| {
                            Task::done(Message::CreateAutomationsWindow {
                                server_name: Arc::new(session.server_name.clone()),
                                session_id,
                                profile_name: session.profile_name.clone(),
                                focus: Some(AutomationsFocus::PackageSettings(specifier)),
                            })
                        })
                }
                _ => Task::none(),
            };
            if let Some(session) = smudgy.sessions.get_mut(session_id) {
                let session_task = session
                    .update(msg)
                    .map(move |msg| Message::SessionAction(session_id, msg));
                Task::batch([session_task, editor_fan_out, bind_task, configure])
            } else {
                log::debug!("Dropping action for closed session {session_id}");
                Task::none()
            }
        }
        Message::CreateSmudgyWindow => {
            let (id, task) = window::open(smudgy_window_settings());
            smudgy.opening_smudgy_windows.insert(id);
            task.map(Message::NewSmudgyWindow)
        }
        Message::NewSmudgyWindow(id) => {
            // Tear-out inserts its window synchronously (it must adopt the
            // transplanted pane before the open task completes), so this may
            // find the entry already present.
            if !smudgy.smudgy_windows.contains_key(&id)
                && !smudgy.opening_smudgy_windows.remove(&id)
            {
                log::warn!("Closing an unclaimed main window completion: {id}");
                return window::close(id);
            }
            smudgy.opening_smudgy_windows.remove(&id);
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
                        let (second_id, open_second) = window::open(smudgy_window_settings());
                        smudgy.opening_smudgy_windows.insert(second_id);
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
            #[cfg(feature = "web-audio-cpal")]
            let audio_announcement =
                announce_pending_audio_failure(&mut smudgy.pending_audio_announcement, id);
            Task::batch([
                spike_task,
                #[cfg(feature = "web-audio-cpal")]
                audio_announcement,
                // Install the Windows native hooks on this window's HWND:
                // the Restart Manager shutdown watcher (installer upgrades
                // over a running smudgy) and the WM_NCHITTEST chrome
                // (native move/resize for the borderless frame).
                window::raw_id::<Message>(id).map(move |raw| {
                    // QA forensics (debug builds only): announce the HWND so
                    // the scripted drag matrix and GetCapture logs can be
                    // correlated to iced window ids.
                    #[cfg(debug_assertions)]
                    log::info!("[pane-drag] window {id:?} hwnd={raw:#x}");
                    Message::HookNativeWindow(id, raw)
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
        Message::HookNativeWindow(id, raw_id) => {
            win_rm::hook_window(raw_id);
            win_chrome::hook_window(id, raw_id);
            Task::none()
        }
        Message::AutomationsWindowMessage {
            id,
            generation,
            message,
        } => {
            if generation != smudgy.automations_context_generation {
                log::debug!(
                    "Dropping Automations message from generation {generation} (current {})",
                    smudgy.automations_context_generation,
                );
                return Task::none();
            }
            let Some(window) = smudgy.automations_window_mut(id) else {
                log::warn!("Received message for unknown window index: {}", id);
                return Task::none();
            };
            let update = window.update(message).map_message(move |message| {
                Message::AutomationsWindowMessage {
                    id,
                    generation,
                    message,
                }
            });

            match update.event {
                Some(AutomationsWindowEvent::UserAutomationsChanged { server_name }) => {
                    let sync_tasks = smudgy
                        .sessions
                        .iter()
                        .filter(|(_, session)| session.server_name.as_str() == server_name.as_str())
                        .map(|(session_id, _)| {
                            Task::done(Message::SessionAction(
                                session_id,
                                session_store::Message::SyncUserAutomations,
                            ))
                        });

                    Task::batch([update.task, Task::batch(sync_tasks)])
                }
                Some(AutomationsWindowEvent::ScriptsChanged { server_name }) => {
                    let reload_tasks = smudgy
                        .sessions
                        .iter()
                        .filter(|(_, session)| session.server_name.as_str() == server_name.as_str())
                        .map(|(session_id, _)| {
                            Task::done(Message::SessionAction(
                                session_id,
                                session_store::Message::Reload,
                            ))
                        });

                    Task::batch([update.task, Task::batch(reload_tasks)])
                }
                Some(AutomationsWindowEvent::SwitchContext {
                    server_name,
                    session_id,
                    profile_name,
                    focus,
                }) => {
                    let old_context_task = update.task;
                    let requested = AutomationsContext {
                        server_name,
                        session_id,
                        profile_name,
                        focus,
                    };
                    let Some(init) = install_automations_context(smudgy, id, requested.clone())
                    else {
                        // Keep the old state, including its drafts. The target session closed
                        // while the switch was queued or awaiting the user's confirmation.
                        log::warn!(
                            "Ignoring Automations switch to closed session {} ({}/{})",
                            requested.session_id,
                            requested.server_name,
                            requested.profile_name,
                        );
                        return old_context_task;
                    };
                    Task::batch([old_context_task, init])
                }
                Some(AutomationsWindowEvent::ContextSwitchCancelled) => update.task,
                Some(AutomationsWindowEvent::CloseCancelled) => {
                    if let Some(main_id) = smudgy.main_window_close_after_automations.take() {
                        smudgy.closing_windows.remove(&main_id);
                    }
                    update.task
                }
                Some(AutomationsWindowEvent::CloseRequested) => {
                    smudgy.automations_window = None;
                    smudgy.window_tracker.remove(id);
                    smudgy.closing_windows.remove(&id);
                    #[cfg(feature = "web-audio-cpal")]
                    smudgy.audio_panel.focused_widgets.remove(&id);
                    // Drop all work mapped by the state that is being discarded.
                    smudgy.automations_context_generation =
                        smudgy.automations_context_generation.wrapping_add(1);
                    let close_main = smudgy
                        .main_window_close_after_automations
                        .take()
                        .map(remember_and_close_window);
                    Task::batch(
                        [Some(update.task), Some(window::close(id)), close_main]
                            .into_iter()
                            .flatten(),
                    )
                }
                None => update.task,
            }
        }
        Message::CreateAutomationsWindow {
            server_name,
            session_id,
            profile_name,
            focus,
        } => {
            let requested = AutomationsContext {
                server_name: server_name.to_string(),
                session_id,
                profile_name,
                focus,
            };
            let last_main_is_closing = all_main_windows_are_closing(
                smudgy.smudgy_windows.keys().copied(),
                smudgy.opening_smudgy_windows.iter().copied(),
                &smudgy.closing_windows,
            );
            if smudgy.main_window_close_after_automations.is_some() || last_main_is_closing {
                // Once application exit owns the terminal Automations prompt, a later open/switch
                // request must not replace it. Keep the exact close confirmation visible; if the
                // user cancels it, a subsequent request can switch normally.
                return smudgy
                    .automations_window_id()
                    .map_or_else(Task::none, window::gain_focus);
            }
            if !automations_context_is_live(smudgy, &requested) {
                log::warn!(
                    "Ignoring Automations request for closed session {} ({}/{})",
                    requested.session_id,
                    requested.server_name,
                    requested.profile_name,
                );
                return smudgy
                    .automations_window_id()
                    .map_or_else(Task::none, window::gain_focus);
            }
            if let Some((id, window)) = smudgy.automations_window.as_mut() {
                let id = *id;
                if window.session_id() == session_id {
                    window.cancel_pending_context_switch();
                    // The window is already on the requested context, so its loads have run:
                    // the navigation goes straight in, with no guard to clear.
                    let generation = smudgy.automations_context_generation;
                    let navigate = requested.focus.map(|focus| {
                        Task::done(Message::AutomationsWindowMessage {
                            id,
                            generation,
                            message: windows::automations_window::Message::Focus(focus),
                        })
                    });
                    return Task::batch(
                        [Some(window::gain_focus(id)), navigate]
                            .into_iter()
                            .flatten(),
                    );
                }
                // The window runs its unsaved-changes guard and answers with
                // `Event::SwitchContext`, which rebuilds it for the requested session.
                let generation = smudgy.automations_context_generation;
                return Task::batch([
                    window::gain_focus(id),
                    Task::done(Message::AutomationsWindowMessage {
                        id,
                        generation,
                        message: windows::automations_window::Message::SwitchContext {
                            server_name: requested.server_name,
                            session_id: requested.session_id,
                            profile_name: requested.profile_name,
                            focus: requested.focus,
                        },
                    }),
                ]);
            }

            // `window::open` completes asynchronously. A second request during that interval updates
            // the target rather than starting another native window; the sole completion consumes
            // whichever exact context was requested most recently.
            if let Some(opening) = smudgy.automations_window_opening.as_mut() {
                opening.context = requested;
                return Task::none();
            }
            let (id, task) = window::open(automations_window_settings());
            smudgy.automations_window_opening = Some(OpeningAutomationsWindow {
                id,
                context: requested,
            });
            task.map(Message::NewAutomationsWindow)
        }
        Message::NewAutomationsWindow(id) => {
            let Some(context) =
                take_opening_automations_context(&mut smudgy.automations_window_opening, id)
            else {
                log::warn!("Closing an unclaimed Automations window completion: {id}");
                return window::close(id);
            };
            if let Some(existing) = smudgy.automations_window_id() {
                log::warn!(
                    "An Automations window already exists; closing duplicate native window {id}"
                );
                return Task::batch([window::close(id), window::gain_focus(existing)]);
            }
            let Some(init) = install_automations_context(smudgy, id, context.clone()) else {
                // `take_opening_automations_context` already unclaimed this native id. Closing it
                // ensures a dead request cannot leave an empty singleton window behind, and a late
                // duplicate close event remains harmless.
                log::warn!(
                    "Closing Automations window {id} because session {} ({}/{}) is no longer live",
                    context.session_id,
                    context.server_name,
                    context.profile_name,
                );
                return window::close(id);
            };
            init
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
                        Task::batch([
                            task,
                            reconcile_area_prefs(smudgy),
                            notify_automations_account_changed(smudgy),
                        ])
                    }
                    Some(SettingsWindowEvent::SignOut { everywhere }) => {
                        let task = smudgy.account.sign_out(everywhere).map(Message::Account);
                        poke_all_mappers(smudgy);
                        Task::batch([task, notify_automations_account_changed(smudgy)])
                    }
                    Some(SettingsWindowEvent::ProfileUpdated(profile)) => {
                        smudgy.account.absorb_profile(*profile);
                        notify_automations_account_changed(smudgy)
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
                        #[cfg(feature = "web-audio-cpal")]
                        {
                            // The settings window opened from a full clone and
                            // does not edit audio. Merge the daemon's latest
                            // authoritative policy so a stale form cannot
                            // overwrite controls changed from the main window.
                            settings.audio = smudgy.audio_panel.preferences.clone();
                        }
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
                        if let Some((_, window)) = smudgy.automations_window.as_mut() {
                            window.sync_code_editor_theme();
                        }
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

fn retire_automations_session_binding(smudgy: &mut Smudgy, session_id: SessionId) {
    if let Some((_, window)) = smudgy.automations_window.as_mut() {
        window.retire_session_binding(session_id);
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
    let removed = smudgy.sessions.shutdown_and_remove(session_id);
    // A repeated close still proves that this id has no live UI binding, so retire a defensive
    // pre-registration subscription even when another teardown path removed the session first.
    retire_automations_session_binding(smudgy, session_id);
    if !removed {
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
/// Build the once-per-open background package-update check for `session_id`'s
/// server: a fresh package client off the shared credential slot plus the context
/// the checker's local-override rule needs. The report routes back through
/// [`Message::PackageCheckCompleted`].
fn start_package_update_check(smudgy: &Smudgy, session_id: SessionId) -> Task<Message> {
    let Some(session) = smudgy.sessions.get(session_id) else {
        return Task::none();
    };
    let handles = smudgy.account.handles();
    let client = smudgy_cloud::package_api::PackageApiClient::new(
        handles.base_url.as_str(),
        handles.credentials.clone(),
    );
    let ctx = package_update_checker::CheckContext {
        server_name: session.server_name.clone(),
        profile_name: session.profile_name.clone(),
    };
    Task::perform(
        package_update_checker::run_session_check(client, ctx),
        move |report| Message::PackageCheckCompleted { session_id, report },
    )
}

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
        | PaneCommand::Scroll {
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
        | PaneCommand::Scroll { session_id, .. }
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
        | PaneCommand::Scroll {
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
        PaneCommand::Scroll {
            session_id,
            key,
            request,
        } => {
            if let Some(session) = smudgy.sessions.get(session_id)
                && !session.scroll_pane(key, request)
            {
                log::warn!("Dropping scroll request for terminal pane {key}");
            }
            Task::none()
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

#[cfg(feature = "web-audio-cpal")]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AudioViewRowKey {
    Master,
    Session(u32),
    Package(u64),
    Trusted(u64),
}

#[cfg(feature = "web-audio-cpal")]
fn audio_session_id(window_id: window::Id, session_id: SessionId) -> iced::widget::Id {
    iced::widget::Id::from(format!("audio-session-{window_id:?}-{session_id}"))
}

#[cfg(feature = "web-audio-cpal")]
fn audio_package_id(
    window_id: window::Id,
    session_id: SessionId,
    row_key: u64,
) -> iced::widget::Id {
    iced::widget::Id::from(format!(
        "audio-package-{window_id:?}-{session_id}-{row_key}"
    ))
}

#[cfg(feature = "web-audio-cpal")]
fn audio_gain_row<'a>(
    window_id: window::Id,
    target: AudioTarget,
    widget_id: iced::widget::Id,
    label: String,
    gain: AudioGainSettings,
    status: Option<String>,
) -> Element<'a, Message> {
    // Muted rows show only "muted" — the remembered volume returns with
    // unmute. A `status` suffix appears only for abnormal routes
    // (preference-only, unconfirmed package scope, failed output); a healthy
    // applied control needs no acknowledgement.
    let row_text = if gain.muted {
        i18n::t!("audio-control-row-muted", "label" => label)
    } else {
        i18n::t!(
            "audio-control-row",
            "label" => label,
            "volume" => gain.volume
        )
    };
    let row_text = match status {
        Some(status) => i18n::t!(
            "audio-control-row-status",
            "row" => row_text,
            "status" => status
        ),
        None => row_text,
    };
    let lower = gain.volume.saturating_sub(widgets::audio_gain::VOLUME_STEP);
    let higher = gain
        .volume
        .saturating_add(widgets::audio_gain::VOLUME_STEP)
        .min(100);
    let message = |action| Message::AudioControl {
        window_id,
        target: target.clone(),
        widget_id: widget_id.clone(),
        action,
    };
    let content = iced::widget::row![
        text(row_text).size(13).width(iced::Length::Fill),
        iced::widget::button(text("−").size(13))
            .on_press(message(widgets::audio_gain::Action::SetVolume(lower))),
        iced::widget::button(text("+").size(13))
            .on_press(message(widgets::audio_gain::Action::SetVolume(higher))),
        iced::widget::button(
            text(if gain.muted {
                i18n::t!("audio-action-unmute")
            } else {
                i18n::t!("audio-action-mute")
            })
            .size(13)
        )
        .on_press(message(widgets::audio_gain::Action::ToggleMuted)),
    ]
    .spacing(6)
    .align_y(iced::Alignment::Center);
    let focus_color = prefs::app_theme().styles.general.accent;
    widgets::audio_gain::AudioGain::new(
        iced::widget::container(content)
            .width(iced::Length::Fill)
            .padding([3, 6]),
        widget_id.clone(),
        gain.volume,
        move |action| Message::AudioControl {
            window_id,
            target: target.clone(),
            widget_id: widget_id.clone(),
            action,
        },
    )
    .focus_color(focus_color)
    .into()
}

#[cfg(feature = "web-audio-cpal")]
fn audio_session_label(session_id: SessionId, server: &str, profile: &str) -> String {
    i18n::t!(
        "audio-session-label",
        "id" => u32::from(session_id),
        "server" => serde_json::to_string(server)
            .expect("a string identity is always JSON-serializable"),
        "profile" => serde_json::to_string(profile)
            .expect("a string identity is always JSON-serializable")
    )
}

/// The Settings window's Audio pane. Composed by the daemon (not by
/// `SettingsWindow::view`) because every row renders from daemon-owned audio
/// state: the boot status, the live/remembered gains, and the session store.
#[cfg(feature = "web-audio-cpal")]
fn audio_settings_pane<'a>(smudgy: &'a Smudgy, window_id: window::Id) -> Element<'a, Message> {
    let mut panel = iced::widget::column![
        text(i18n::t!("audio-settings-title")).size(20),
        text(i18n::t!("audio-panel-keyboard-help")).size(12),
    ]
    .spacing(8);
    if let Some(status) = smudgy.audio_status.banner() {
        panel = panel.push(text(status).size(12));
    }
    let master_preference_only = matches!(smudgy.audio_status, AudioBootStatus::Unavailable(_));
    let master_status = match smudgy.audio_status {
        AudioBootStatus::Physical => None,
        AudioBootStatus::Failed(_) => Some(i18n::t!("audio-ack-failed")),
        AudioBootStatus::Unavailable(_) => Some(i18n::t!("audio-ack-preference")),
    };
    let mut rows: Vec<(AudioViewRowKey, Element<'a, Message>)> = Vec::new();
    rows.push((
        AudioViewRowKey::Master,
        audio_gain_row(
            window_id,
            AudioTarget::Master,
            audio_master_id(window_id),
            i18n::t!("audio-master-label"),
            if master_preference_only {
                smudgy.audio_panel.preferences.master
            } else {
                smudgy.audio_panel.master_live
            },
            master_status,
        ),
    ));
    for (session_id, session) in smudgy.sessions.iter() {
        let server: Arc<str> = Arc::from(session.server_name.as_str());
        let profile: Arc<str> = Arc::from(session.profile_name.as_str());
        let target = AudioTarget::Session {
            id: session_id,
            server: Arc::clone(&server),
            profile: Arc::clone(&profile),
        };
        let session_status = match audio_control_route_for_target(smudgy, &target) {
            AudioControlRoute::Physical => None,
            AudioControlRoute::PreferenceOnly => Some(i18n::t!("audio-ack-preference")),
            AudioControlRoute::Failed => Some(i18n::t!("audio-ack-failed")),
        };
        rows.push((
            AudioViewRowKey::Session(u32::from(session_id)),
            audio_gain_row(
                window_id,
                target,
                audio_session_id(window_id, session_id),
                audio_session_label(session_id, &session.server_name, &session.profile_name),
                session.audio_gain(),
                session_status,
            ),
        ));
        for package in session
            .audio_packages()
            .iter()
            .filter(|package| audio_package_row_is_visible(package))
        {
            let label = format!("  {}/{}", package.owner, package.name);
            if package.trusted {
                rows.push((
                    AudioViewRowKey::Trusted(package.ui_key),
                    iced::widget::container(
                        text(i18n::t!("audio-trusted-row", "label" => label)).size(13),
                    )
                    .padding([3, 6])
                    .into(),
                ));
            } else {
                let package_target = AudioTarget::Package {
                    id: session_id,
                    server: Arc::clone(&server),
                    profile: Arc::clone(&profile),
                    owner: Arc::clone(&package.owner),
                    name: Arc::clone(&package.name),
                    action_key: package.action_key,
                };
                let package_route = audio_control_route_for_target(smudgy, &package_target);
                rows.push((
                    AudioViewRowKey::Package(package.ui_key),
                    audio_gain_row(
                        window_id,
                        package_target,
                        audio_package_id(window_id, session_id, package.ui_key),
                        label,
                        package.gain,
                        match package_route {
                            AudioControlRoute::Physical if package.applied => None,
                            AudioControlRoute::Physical => {
                                Some(i18n::t!("audio-ack-package-unconfirmed"))
                            }
                            AudioControlRoute::PreferenceOnly => {
                                Some(i18n::t!("audio-ack-preference"))
                            }
                            AudioControlRoute::Failed => Some(i18n::t!("audio-ack-failed")),
                        },
                    ),
                ));
            }
        }
    }
    let controls = iced::widget::keyed_column(rows).spacing(2);
    panel = panel.push(
        iced::widget::scrollable(controls)
            .id(audio_scroll_id(window_id))
            .height(iced::Length::Fill)
            .width(iced::Length::Fill),
    );
    if let Some(notice) = smudgy.audio_panel.notice.as_ref() {
        panel = panel.push(text(notice).size(12));
    }
    iced::widget::container(panel)
        .id(audio_panel_id(window_id))
        .width(iced::Length::Fill)
        .height(iced::Length::Fill)
        .padding(16)
        .into()
}

/// A settings window's whole view: the window's own tabs, except the Audio
/// pane, which renders daemon-owned audio state around the window's nav rail.
fn settings_window_view<'a>(
    smudgy: &'a Smudgy,
    window: &'a SettingsWindow,
    id: window::Id,
) -> Element<'a, Message> {
    #[cfg(not(feature = "web-audio-cpal"))]
    let _ = smudgy;
    #[cfg(feature = "web-audio-cpal")]
    if window.tab() == settings_window::Tab::Audio {
        let nav = window
            .nav()
            .map(move |message| Message::SettingsWindowMessage(id, message));
        let body = iced::widget::row![
            iced::widget::container(nav).padding(12),
            iced::widget::rule::vertical(1),
            audio_settings_pane(smudgy, id),
        ];
        // The root id scopes the pane's focus operations to this window
        // (see `scoped_audio_focus`).
        return iced::widget::container(body)
            .id(audio_window_root_id(id))
            .width(iced::Length::Fill)
            .height(iced::Length::Fill)
            .into();
    }
    window
        .view()
        .map(move |message| Message::SettingsWindowMessage(id, message))
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
        let window_content = window
            .view(&smudgy.sessions, drag)
            .map(move |message| Message::SmudgyWindowMessage(id, message));
        #[cfg(feature = "web-audio-cpal")]
        let window_content: Element<'_, Message> = {
            let mut content = iced::widget::column![];
            if let Some(status) = smudgy.audio_status.banner() {
                content = content.push(
                    iced::widget::container(text(status).size(13))
                        .width(iced::Length::Fill)
                        .padding([6, 12])
                        .style(theme::builtins::container::modal_title_bar),
                );
            }
            content
                .push(window_content)
                .width(iced::Length::Fill)
                .height(iced::Length::Fill)
                .into()
        };
        // The reserved audio chord (Cmd/Ctrl+Shift+A) works from any main
        // window and lands on the Settings window's Audio pane.
        #[cfg(feature = "web-audio-cpal")]
        let window_content: Element<'_, Message> =
            widgets::audio_gain::AudioPanelRoot::new(window_content, || Message::OpenAudioSettings)
                .into();
        let content = center(window_content);
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
    let content: Element<'_, Message> = if let Some(window) = smudgy.automations_window(id) {
        let generation = smudgy.automations_context_generation;
        center(
            window
                .view()
                .map(move |message| Message::AutomationsWindowMessage {
                    id,
                    generation,
                    message,
                }),
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
        center(settings_window_view(smudgy, window, id)).into()
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

    #[test]
    fn automations_window_defers_native_close_requests() {
        assert!(!automations_window_settings().exit_on_close_request);
    }

    #[test]
    fn main_window_defers_native_close_requests_for_terminal_guards() {
        assert!(!smudgy_window_settings().exit_on_close_request);
    }

    #[test]
    fn main_exit_waits_until_every_registered_and_opening_window_is_closing() {
        let first = window::Id::unique();
        let second = window::Id::unique();
        let opening = window::Id::unique();
        let mut closing = HashSet::from([first]);

        assert!(!all_main_windows_are_closing(
            [first, second],
            std::iter::empty(),
            &closing,
        ));
        closing.insert(second);
        assert!(all_main_windows_are_closing(
            [first, second],
            std::iter::empty(),
            &closing,
        ));
        assert!(!all_main_windows_are_closing(
            [first, second],
            [opening],
            &closing,
        ));
        closing.insert(opening);
        assert!(all_main_windows_are_closing(
            [first, second],
            [opening],
            &closing,
        ));
        assert!(!all_main_windows_are_closing(
            std::iter::empty(),
            std::iter::empty(),
            &closing,
        ));
    }

    #[test]
    fn automations_open_completion_requires_its_exact_pending_native_id() {
        let stale_id = window::Id::unique();
        let current_id = window::Id::unique();
        let context = AutomationsContext {
            server_name: "Arctic".to_string(),
            session_id: SessionId::from(7),
            profile_name: "main".to_string(),
            focus: None,
        };
        let mut opening = Some(OpeningAutomationsWindow {
            id: current_id,
            context: context.clone(),
        });

        assert!(take_opening_automations_context(&mut opening, stale_id).is_none());
        assert_eq!(opening.as_ref().map(|pending| pending.id), Some(current_id));
        assert!(!cancel_opening_automations_window(&mut opening, stale_id));
        assert_eq!(
            take_opening_automations_context(&mut opening, current_id),
            Some(context)
        );
        assert!(opening.is_none());
    }

    #[test]
    fn close_before_registration_cancels_only_the_matching_automations_open() {
        let id = window::Id::unique();
        let mut opening = Some(OpeningAutomationsWindow {
            id,
            context: AutomationsContext {
                server_name: "Arctic".to_string(),
                session_id: SessionId::from(7),
                profile_name: "main".to_string(),
                focus: None,
            },
        });

        assert!(cancel_opening_automations_window(&mut opening, id));
        assert!(opening.is_none());
        assert!(take_opening_automations_context(&mut opening, id).is_none());
    }

    #[cfg(feature = "web-audio-cpal")]
    #[derive(Clone, Debug, PartialEq)]
    enum FocusTestMessage {
        Open,
        TabCaptured,
        Gain(widgets::audio_gain::Action),
        Input(String),
        ConflictingHotkey(smudgy_core::session::HotkeyId),
    }

    #[cfg(feature = "web-audio-cpal")]
    type FocusTestElement<'a> = iced::Element<'a, FocusTestMessage, crate::theme::Theme, ()>;

    #[cfg(feature = "web-audio-cpal")]
    fn operate_mounted(
        element: &mut FocusTestElement<'_>,
        tree: &mut iced::advanced::widget::Tree,
        node: &iced::advanced::layout::Node,
        operation: &mut dyn iced::advanced::widget::Operation<()>,
    ) {
        element
            .as_widget_mut()
            .operate(tree, iced::advanced::Layout::new(node), &(), operation);
    }

    #[cfg(feature = "web-audio-cpal")]
    fn update_mounted(
        element: &mut FocusTestElement<'_>,
        tree: &mut iced::advanced::widget::Tree,
        node: &iced::advanced::layout::Node,
        event: &iced::Event,
    ) -> (Vec<FocusTestMessage>, iced::event::Status) {
        let mut messages = Vec::new();
        let mut shell = iced::advanced::Shell::new(&mut messages);
        let mut clipboard = iced::advanced::clipboard::Null;
        element.as_widget_mut().update(
            tree,
            event,
            iced::advanced::Layout::new(node),
            iced::advanced::mouse::Cursor::Unavailable,
            &(),
            &mut clipboard,
            &mut shell,
            &node.bounds(),
        );
        let status = shell.event_status();
        drop(shell);
        (messages, status)
    }

    #[cfg(feature = "web-audio-cpal")]
    struct MountedFocusProbe {
        target: iced::widget::Id,
        focused: Option<bool>,
    }

    #[cfg(feature = "web-audio-cpal")]
    impl iced::advanced::widget::Operation<()> for MountedFocusProbe {
        fn focusable(
            &mut self,
            id: Option<&iced::widget::Id>,
            _bounds: iced::Rectangle,
            state: &mut dyn iced::advanced::widget::operation::Focusable,
        ) {
            if id == Some(&self.target) {
                self.focused = Some(state.is_focused());
            }
        }

        fn traverse(
            &mut self,
            operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<()>),
        ) {
            if self.focused.is_none() {
                operate(self);
            }
        }
    }

    #[cfg(feature = "web-audio-cpal")]
    fn mounted_is_focused(
        element: &mut FocusTestElement<'_>,
        tree: &mut iced::advanced::widget::Tree,
        node: &iced::advanced::layout::Node,
        id: iced::widget::Id,
    ) -> bool {
        let mut operation = MountedFocusProbe {
            target: id,
            focused: None,
        };
        operate_mounted(element, tree, node, &mut operation);
        operation.focused.unwrap_or(false)
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn every_locale_maps_exact_audio_feedback_to_the_frozen_priority() {
        use iced_runtime::window::AnnouncementPriority::{Assertive, Polite};

        let cases = [
            (
                AudioAnnouncement::TargetClosed,
                "audio-notice-target-closed",
                Assertive,
            ),
            (
                AudioAnnouncement::AppliedSaveFailed,
                "audio-announcement-applied-save-failed",
                Assertive,
            ),
            (
                AudioAnnouncement::ChangeFailed,
                "audio-announcement-failed",
                Assertive,
            ),
            (
                AudioAnnouncement::PreferenceSaved,
                "audio-notice-preference-saved",
                Polite,
            ),
            (
                AudioAnnouncement::PreferenceSaveFailed,
                "audio-announcement-preference-save-failed",
                Assertive,
            ),
        ];

        for catalog in smudgy_i18n::available_catalogs() {
            let translator =
                smudgy_i18n::Translator::for_tag(catalog.tag).expect("manifest locale resolves");

            for (announcement, message_id, priority) in cases {
                let (actual_text, actual_priority) =
                    localized_audio_announcement(translator, announcement);
                assert_eq!(actual_text, translator.translate(message_id));
                assert_eq!(actual_priority, priority);
                assert!(
                    !actual_text.starts_with('⟦'),
                    "missing {message_id} in {}",
                    catalog.tag
                );
            }
        }
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn detailed_audio_errors_stay_visible_but_never_enter_announce_actions() {
        use iced_runtime::futures::futures::{FutureExt, StreamExt};
        use iced_runtime::window::AnnouncementPriority::Assertive;

        const RAW_MARKER: &str = "PRIVATE-device-path-7f84";
        let translator = smudgy_i18n::Translator::for_tag("en-US").unwrap();
        let cases = [
            (
                "audio-notice-applied-save-failed",
                AudioAnnouncement::AppliedSaveFailed,
                "Audio changed for this launch but could not be saved.",
            ),
            (
                "audio-notice-failed",
                AudioAnnouncement::ChangeFailed,
                "Audio change failed; nothing was saved.",
            ),
            (
                "audio-notice-preference-save-failed",
                AudioAnnouncement::PreferenceSaveFailed,
                "Audio preference was not saved.",
            ),
        ];

        for (visible_id, announcement, expected_announcement) in cases {
            let mut args = smudgy_i18n::FluentArgs::new();
            args.set("error", RAW_MARKER);
            let visible = translator.translate_with(visible_id, &args);
            assert!(visible.contains(RAW_MARKER));

            let (text, priority) = localized_audio_announcement(translator, announcement);
            assert_eq!(text, expected_announcement);
            assert_eq!(priority, Assertive);
            assert!(!text.contains(RAW_MARKER));

            let id = window::Id::unique();
            let task: Task<Message> = window::announce(id, text, priority);
            let mut stream =
                iced_runtime::task::into_stream(task).expect("announcement effect stream");
            let action = stream
                .next()
                .now_or_never()
                .flatten()
                .expect("announcement action");
            let iced_runtime::Action::Window(iced_runtime::window::Action::Announce(
                actual_id,
                action_text,
                Assertive,
            )) = action
            else {
                panic!("expected assertive window announcement action");
            };
            assert_eq!(actual_id, id);
            assert_eq!(action_text, expected_announcement);
            assert!(!action_text.contains(RAW_MARKER));
        }
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn bounded_volume_noop_does_not_allocate_or_evict_a_policy_row() {
        let mut preferences = AudioSettings::default();
        for index in 0..256 {
            preferences
                .session_mut(&format!("server-{index}"), "profile")
                .gain
                .volume = 50;
        }
        let before = preferences.clone();
        let target = AudioTarget::Session {
            id: SessionId::from(999),
            server: Arc::from("new-server"),
            profile: Arc::from("profile"),
        };

        let current = AudioGainSettings::default();
        let action = widgets::audio_gain::Action::SetVolume(100);
        if let Some(desired) = audio_action_gain(current, action)
            && (desired != current
                || audio_action_changes_preference(&preferences, &target, desired, action))
        {
            update_audio_preference(&mut preferences, &target, desired);
        }

        assert_eq!(preferences, before);
        assert_eq!(preferences.sessions.len(), 256);
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn initializer_task_owns_buffered_terminal_output_receiver() {
        use iced_runtime::futures::futures::StreamExt;

        let (mut sender, receiver) = futures::channel::mpsc::channel(1);
        sender
            .try_send(MixerOutputFailure::BackendFailure)
            .expect("the terminal event is buffered before iced initialization");
        drop(sender);

        let task = audio_output_failure_task(Some(receiver));
        assert_eq!(task.units(), 1);
        let mut stream = iced_runtime::task::into_stream(task).expect("initializer task stream");
        let action = futures::executor::block_on(stream.next())
            .expect("buffered terminal event is delivered immediately");
        assert!(matches!(
            action,
            iced_runtime::Action::Output(Message::AudioOutputTerminated(
                MixerOutputFailure::BackendFailure
            ))
        ));
        assert!(futures::executor::block_on(stream.next()).is_none());
        assert_eq!(audio_output_failure_task(None).units(), 0);
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn terminal_output_uses_generic_message_and_is_presented_once() {
        let boot_failure = Arc::from("failure observed while applying startup policy");
        let mut status = AudioBootStatus::Failed(Arc::clone(&boot_failure));
        let mut notice = None;
        let mut presented = false;
        let message = apply_terminal_audio_failure(
            &mut status,
            &mut notice,
            &mut presented,
            MixerOutputFailure::BackendFailure,
        )
        .expect("the buffered event still presents a failure already seen during boot");
        assert_eq!(message, i18n::t!("audio-notice-output-dead"));
        assert_eq!(notice.as_deref(), Some(message.as_str()));
        assert_eq!(status.banner().as_deref(), Some(message.as_str()));
        assert!(!message.contains("BackendFailure"));

        assert!(
            apply_terminal_audio_failure(
                &mut status,
                &mut notice,
                &mut presented,
                MixerOutputFailure::BackendFailure,
            )
            .is_none(),
            "a duplicate observation cannot create a second notice or announcement"
        );
        assert_eq!(notice.as_deref(), Some(message.as_str()));
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn terminal_output_prefers_the_mru_tracked_main_window() {
        use iced_runtime::futures::futures::{FutureExt, StreamExt};
        use iced_runtime::window::AnnouncementPriority::Assertive;

        let older = window::Id::unique();
        let recent = window::Id::unique();
        let non_main = window::Id::unique();
        let mut tracker = pane_drag::WindowTracker::default();
        tracker.apply(older, pane_drag::TrackEvent::Focused);
        tracker.apply(recent, pane_drag::TrackEvent::Focused);
        tracker.apply(non_main, pane_drag::TrackEvent::Focused);
        let main_windows = BTreeMap::from([(older, ()), (recent, ())]);
        assert_eq!(
            audio_announcement_window(&tracker, &main_windows),
            Some(recent)
        );

        let mut pending = None;
        let message = i18n::t!("audio-notice-output-dead");
        let task =
            announce_terminal_audio_failure(&tracker, &main_windows, &mut pending, message.clone());
        assert!(pending.is_none());
        let mut stream = iced_runtime::task::into_stream(task).expect("announcement task stream");
        let action = stream
            .next()
            .now_or_never()
            .flatten()
            .expect("assertive announcement action");
        assert!(matches!(
            action,
            iced_runtime::Action::Window(iced_runtime::window::Action::Announce(
                id,
                ref text,
                Assertive,
            )) if id == recent && text == &message
        ));
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn terminal_output_before_a_window_is_flushed_when_one_opens() {
        use iced_runtime::futures::futures::{FutureExt, StreamExt};
        use iced_runtime::window::AnnouncementPriority::Assertive;

        let early = "early terminal audio failure".to_string();
        let mut pending = None;
        let task = announce_terminal_audio_failure(
            &pane_drag::WindowTracker::default(),
            &BTreeMap::<window::Id, ()>::new(),
            &mut pending,
            early.clone(),
        );
        assert_eq!(task.units(), 0);
        assert_eq!(pending.as_deref(), Some(early.as_str()));

        let first = window::Id::unique();
        let task = announce_pending_audio_failure(&mut pending, first);
        assert!(pending.is_none());
        let mut stream = iced_runtime::task::into_stream(task).expect("deferred announcement");
        let action = stream
            .next()
            .now_or_never()
            .flatten()
            .expect("deferred assertive announcement action");
        assert!(matches!(
            action,
            iced_runtime::Action::Window(iced_runtime::window::Action::Announce(
                id,
                ref text,
                Assertive,
            )) if id == first && text == &early
        ));
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn terminal_output_uses_an_existing_main_window_before_tracking_arrives() {
        use iced_runtime::futures::futures::{FutureExt, StreamExt};
        use iced_runtime::window::AnnouncementPriority::Assertive;

        let first = window::Id::unique();
        let main_windows = BTreeMap::from([(first, ())]);
        let message = "terminal audio failure during tracking startup".to_string();
        let mut pending = None;
        let task = announce_terminal_audio_failure(
            &pane_drag::WindowTracker::default(),
            &main_windows,
            &mut pending,
            message.clone(),
        );
        assert!(pending.is_none());
        let mut stream = iced_runtime::task::into_stream(task).expect("announcement task");
        let action = stream
            .next()
            .now_or_never()
            .flatten()
            .expect("assertive announcement action");
        assert!(matches!(
            action,
            iced_runtime::Action::Window(iced_runtime::window::Action::Announce(
                id,
                ref text,
                Assertive,
            )) if id == first && text == &message
        ));
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn unavailable_controls_route_to_saved_preference_only() {
        assert_eq!(
            audio_control_route(&AudioBootStatus::Unavailable(Arc::from("no device"))),
            AudioControlRoute::PreferenceOnly
        );
        assert_eq!(
            audio_control_route(&AudioBootStatus::Physical),
            AudioControlRoute::Physical
        );
        assert_eq!(
            audio_control_route_with_local_emulation(&AudioBootStatus::Physical, true),
            AudioControlRoute::PreferenceOnly,
            "a session-local capacity/policy fallback does not downgrade the healthy master"
        );
        assert_eq!(
            audio_control_route_with_local_emulation(&AudioBootStatus::Physical, false),
            AudioControlRoute::Physical
        );
        assert_eq!(
            audio_control_route(&AudioBootStatus::Failed(Arc::from("output dead"))),
            AudioControlRoute::Failed
        );

        let current = AudioSettings::default();
        let desired = AudioGainSettings {
            volume: 45,
            muted: true,
        };
        let persisted = std::cell::Cell::new(false);
        let next = persist_updated_audio_preferences(
            &current,
            &AudioTarget::Master,
            desired,
            widgets::audio_gain::Action::ToggleMuted,
            |candidate| {
                assert_eq!(candidate.master.volume, 100);
                assert!(candidate.master.muted);
                persisted.set(true);
                Ok(())
            },
        )
        .expect("preference-only save succeeds");

        assert!(persisted.get());
        assert_eq!(next.0.master.volume, 100);
        assert!(next.0.master.muted);
        assert_eq!(current.master, AudioGainSettings::default());
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn duplicate_live_identity_persists_only_the_field_each_control_changed() {
        let first = AudioTarget::Session {
            id: SessionId::from(1),
            server: Arc::from("same"),
            profile: Arc::from("profile"),
        };
        let second = AudioTarget::Session {
            id: SessionId::from(2),
            server: Arc::from("same"),
            profile: Arc::from("profile"),
        };
        let (after_volume, _) = persist_updated_audio_preferences(
            &AudioSettings::default(),
            &first,
            AudioGainSettings {
                volume: 50,
                muted: false,
            },
            widgets::audio_gain::Action::SetVolume(50),
            |_| Ok(()),
        )
        .expect("first exact session saves volume");
        let (after_mute, stored) = persist_updated_audio_preferences(
            &after_volume,
            &second,
            AudioGainSettings {
                volume: 100,
                muted: true,
            },
            widgets::audio_gain::Action::ToggleMuted,
            |_| Ok(()),
        )
        .expect("second exact session saves mute without its stale volume");

        assert_eq!(stored.volume, 50);
        assert!(stored.muted);
        assert_eq!(after_mute.session("same", "profile").unwrap().gain, stored);

        let first_live = AudioGainSettings {
            volume: 50,
            muted: false,
        };
        let second_live = AudioGainSettings {
            volume: 100,
            muted: true,
        };
        let action = widgets::audio_gain::Action::SetVolume(100);
        let desired = audio_action_gain(second_live, action).unwrap();
        assert_eq!(desired, second_live, "End is a no-op for B's live gain");
        assert!(
            audio_action_changes_preference(&after_mute, &second, desired, action),
            "End is still meaningful because the shared durable volume is stale"
        );
        let (restored, stored) =
            persist_updated_audio_preferences(&after_mute, &second, desired, action, |_| Ok(()))
                .expect("B restores the shared next-start volume");
        assert_eq!(stored, second_live, "the independent mute field survives");
        assert_eq!(restored.session("same", "profile").unwrap().gain, stored);
        assert_eq!(
            first_live,
            AudioGainSettings {
                volume: 50,
                muted: false,
            },
            "B's exact live control never mutates sibling A"
        );
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn audio_session_labels_distinguish_duplicate_instances_and_delimiter_shaped_identities() {
        let first = audio_session_label(SessionId::from(1), "same", "profile");
        let second = audio_session_label(SessionId::from(2), "same", "profile");
        assert_ne!(
            first, second,
            "display-only SessionId distinguishes instances"
        );

        let left = audio_session_label(SessionId::from(3), "a/b", "c");
        let right = audio_session_label(SessionId::from(3), "a", "b/c");
        assert_ne!(left, right, "server and profile remain separately labelled");
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn stale_package_action_epoch_is_rejected_while_logical_widget_identity_survives_reload() {
        let window_id = iced::window::Id::unique();
        let session_id = SessionId::from(9);
        let retained_widget = audio_package_id(window_id, session_id, 42);
        let replacement = session_store::AudioPackageRow {
            ui_key: 42,
            action_key: 9002,
            owner: Arc::from("owner"),
            name: Arc::from("name"),
            trusted: false,
            audio_used: true,
            gain: AudioGainSettings::default(),
            applied: false,
        };
        let delayed_action_key = 9001;
        let mutated = std::cell::Cell::new(false);
        let saved = std::cell::Cell::new(false);

        if audio_widget_is_current(Some(retained_widget.clone()), &retained_widget)
            && audio_package_action_is_current(&replacement, delayed_action_key)
        {
            mutated.set(true);
            saved.set(true);
        }

        assert!(!mutated.get());
        assert!(!saved.get());
        assert!(audio_widget_is_current(
            Some(retained_widget.clone()),
            &retained_widget
        ));
        assert!(audio_package_action_is_current(&replacement, 9002));
        assert!(audio_package_row_is_visible(&replacement));

        let mut never_used = replacement;
        never_used.audio_used = false;
        assert!(!audio_package_row_is_visible(&never_used));
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn preference_save_failure_leaves_authoritative_cache_unchanged() {
        let current = AudioSettings::default();
        let desired = AudioGainSettings {
            volume: 25,
            muted: false,
        };
        let cached = std::cell::Cell::new(current.master);
        let saved = std::cell::Cell::new(false);

        let result = persist_updated_audio_preferences(
            &current,
            &AudioTarget::Master,
            desired,
            widgets::audio_gain::Action::SetVolume(25),
            |_| Err("disk refused write".to_string()),
        );
        assert!(result.is_err());
        if let Ok(next) = result {
            cached.set(next.0.master);
            saved.set(true);
        }

        assert_eq!(cached.get(), AudioGainSettings::default());
        assert!(!saved.get());
    }

    #[cfg(feature = "web-audio-cpal")]
    struct AudioVisibilityProbe {
        scroll: iced::widget::Id,
        target: iced::widget::Id,
        viewport: Option<iced::Rectangle>,
        translation: iced::Vector,
        target_bounds: Option<iced::Rectangle>,
    }

    #[cfg(feature = "web-audio-cpal")]
    impl iced::advanced::widget::Operation<()> for AudioVisibilityProbe {
        fn scrollable(
            &mut self,
            id: Option<&iced::widget::Id>,
            bounds: iced::Rectangle,
            _content_bounds: iced::Rectangle,
            translation: iced::Vector,
            _state: &mut dyn iced::advanced::widget::operation::Scrollable,
        ) {
            if id == Some(&self.scroll) {
                self.viewport = Some(bounds);
                self.translation = translation;
            }
        }

        fn focusable(
            &mut self,
            id: Option<&iced::widget::Id>,
            bounds: iced::Rectangle,
            _state: &mut dyn iced::advanced::widget::operation::Focusable,
        ) {
            if id == Some(&self.target) {
                self.target_bounds = Some(bounds);
            }
        }

        fn traverse(
            &mut self,
            operate: &mut dyn FnMut(&mut dyn iced::advanced::widget::Operation<()>),
        ) {
            operate(self);
        }
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn mounted_audio_tree_enters_from_text_input_scopes_traversal_and_reveals_focus() {
        use iced::advanced::widget::operation::{Operation as _, Outcome, focusable};

        let root_id = iced::widget::Id::from("test-audio-window-root".to_string());
        let panel_id = iced::widget::Id::from("test-audio-panel".to_string());
        let scroll_id = iced::widget::Id::from("test-audio-scroll".to_string());
        let input_id = iced::widget::Id::from("test-session-input".to_string());
        let modal_input_id = iced::widget::Id::from("test-modal-input".to_string());
        let outside_id = iced::widget::Id::from("test-outside-focus".to_string());
        let row_ids: Vec<_> = (0..8)
            .map(|index| iced::widget::Id::from(format!("test-audio-row-{index}")))
            .collect();

        let mut conflicting_hotkeys = std::collections::HashMap::new();
        conflicting_hotkeys.insert(
            crate::keymap::MaybePhysicalKey::Key(iced::keyboard::Key::Character("a".into())),
            vec![(
                iced::keyboard::Modifiers::COMMAND | iced::keyboard::Modifiers::SHIFT,
                smudgy_core::session::HotkeyId::default(),
            )],
        );
        let input: FocusTestElement<'_> = iced::Element::new(
            widgets::hotkey_matching_input::HotkeyMatchingInput::new(
                &conflicting_hotkeys,
                "",
                "command text",
            )
            .id(input_id.clone())
            .on_input(FocusTestMessage::Input)
            .on_key_pressed(
                iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
                FocusTestMessage::TabCaptured,
            )
            .on_match(FocusTestMessage::ConflictingHotkey),
        );
        let modal_input: FocusTestElement<'_> = iced::widget::TextInput::new("", "modal text")
            .id(modal_input_id.clone())
            .on_input(FocusTestMessage::Input)
            .into();
        let outside: FocusTestElement<'_> = widgets::audio_gain::AudioGain::new(
            iced::widget::Space::new().height(20),
            outside_id.clone(),
            100,
            FocusTestMessage::Gain,
        )
        .into();
        let rows: Vec<FocusTestElement<'_>> = row_ids
            .iter()
            .cloned()
            .map(|id| {
                widgets::audio_gain::AudioGain::new(
                    iced::widget::Space::new()
                        .width(iced::Length::Fill)
                        .height(30),
                    id,
                    100,
                    FocusTestMessage::Gain,
                )
                .into()
            })
            .collect();
        let panel: FocusTestElement<'_> = iced::widget::container(
            iced::widget::scrollable(iced::widget::Column::with_children(rows))
                .id(scroll_id.clone())
                .height(60),
        )
        .id(panel_id.clone())
        .into();
        let scoped: FocusTestElement<'_> =
            iced::widget::container(iced::widget::Column::with_children(vec![
                input,
                modal_input,
                outside,
                panel,
            ]))
            .id(root_id.clone())
            .into();
        let mut mounted: FocusTestElement<'_> =
            widgets::audio_gain::AudioPanelRoot::new(scoped, || FocusTestMessage::Open).into();
        let mut tree = iced::advanced::widget::Tree::new(mounted.as_widget());
        let node = mounted.as_widget_mut().layout(
            &mut tree,
            &(),
            &iced::advanced::layout::Limits::new(iced::Size::ZERO, iced::Size::new(500.0, 180.0)),
        );
        let shortcut = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Character("a".into()),
            modified_key: iced::keyboard::Key::Character("A".into()),
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::KeyA),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::COMMAND | iced::keyboard::Modifiers::SHIFT,
            text: None,
            repeat: false,
        });

        let mut focus_input = iced::advanced::widget::operation::scope(
            root_id.clone(),
            focusable::focus::<()>(input_id.clone()),
        );
        operate_mounted(&mut mounted, &mut tree, &node, &mut focus_input);
        assert!(mounted_is_focused(
            &mut mounted,
            &mut tree,
            &node,
            input_id.clone()
        ));
        let tab = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
            modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Tab),
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Tab),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::empty(),
            text: None,
            repeat: false,
        });
        let (messages, status) = update_mounted(&mut mounted, &mut tree, &node, &tab);
        assert_eq!(messages, vec![FocusTestMessage::TabCaptured]);
        assert_eq!(status, iced::event::Status::Captured);
        assert!(audio_panel_tab_event(tab.clone(), status, iced::window::Id::unique()).is_none());

        let (messages, status) = update_mounted(&mut mounted, &mut tree, &node, &shortcut);
        assert_eq!(messages, vec![FocusTestMessage::Open]);
        assert_eq!(status, iced::event::Status::Captured);
        assert!(mounted_is_focused(
            &mut mounted,
            &mut tree,
            &node,
            input_id.clone()
        ));

        let mut focus_modal = iced::advanced::widget::operation::scope(
            root_id.clone(),
            focusable::focus::<()>(modal_input_id.clone()),
        );
        operate_mounted(&mut mounted, &mut tree, &node, &mut focus_modal);
        let (messages, status) = update_mounted(&mut mounted, &mut tree, &node, &shortcut);
        assert_eq!(messages, vec![FocusTestMessage::Open]);
        assert_eq!(status, iced::event::Status::Captured);
        let typed = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Character("x".into()),
            modified_key: iced::keyboard::Key::Character("x".into()),
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::KeyX),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::empty(),
            text: Some("x".into()),
            repeat: false,
        });
        let (messages, _) = update_mounted(&mut mounted, &mut tree, &node, &typed);
        assert_eq!(
            messages,
            vec![FocusTestMessage::Input("modal textx".to_string())],
            "the reserved chord never selected the stock modal input"
        );

        let mut focus_input_again = iced::advanced::widget::operation::scope(
            root_id.clone(),
            focusable::focus::<()>(input_id.clone()),
        );
        operate_mounted(&mut mounted, &mut tree, &node, &mut focus_input_again);

        let mut enter = iced::advanced::widget::operation::scope(
            root_id.clone(),
            focusable::focus::<()>(row_ids[0].clone()),
        );
        operate_mounted(&mut mounted, &mut tree, &node, &mut enter);
        assert!(!mounted_is_focused(
            &mut mounted,
            &mut tree,
            &node,
            input_id
        ));
        assert!(mounted_is_focused(
            &mut mounted,
            &mut tree,
            &node,
            row_ids[0].clone()
        ));
        assert!(!mounted_is_focused(
            &mut mounted,
            &mut tree,
            &node,
            outside_id
        ));

        let (messages, status) = update_mounted(&mut mounted, &mut tree, &node, &tab);
        assert!(messages.is_empty());
        assert_eq!(status, iced::event::Status::Ignored);
        assert!(matches!(
            audio_panel_tab_event(tab, status, iced::window::Id::unique()),
            Some(Message::AudioPanelTraverse {
                backwards: false,
                ..
            })
        ));

        let mut next = iced::advanced::widget::operation::scope(
            panel_id.clone(),
            focusable::focus_next::<()>(),
        );
        operate_mounted(&mut mounted, &mut tree, &node, &mut next);
        let Outcome::Chain(mut apply_next) = next.finish() else {
            panic!("focus_next must produce its second mounted-tree pass");
        };
        operate_mounted(&mut mounted, &mut tree, &node, apply_next.as_mut());
        assert!(mounted_is_focused(
            &mut mounted,
            &mut tree,
            &node,
            row_ids[1].clone()
        ));

        let space = iced::Event::Keyboard(iced::keyboard::Event::KeyPressed {
            key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Space),
            modified_key: iced::keyboard::Key::Named(iced::keyboard::key::Named::Space),
            physical_key: iced::keyboard::key::Physical::Code(iced::keyboard::key::Code::Space),
            location: iced::keyboard::Location::Standard,
            modifiers: iced::keyboard::Modifiers::empty(),
            text: Some(" ".into()),
            repeat: false,
        });
        let (messages, status) = update_mounted(&mut mounted, &mut tree, &node, &space);
        assert_eq!(
            messages,
            vec![FocusTestMessage::Gain(
                widgets::audio_gain::Action::ToggleMuted
            )]
        );
        assert_eq!(status, iced::event::Status::Captured);

        let mut previous = iced::advanced::widget::operation::scope(
            panel_id.clone(),
            focusable::focus_previous::<()>(),
        );
        operate_mounted(&mut mounted, &mut tree, &node, &mut previous);
        let Outcome::Chain(mut apply_previous) = previous.finish() else {
            panic!("focus_previous must produce its second mounted-tree pass");
        };
        operate_mounted(&mut mounted, &mut tree, &node, apply_previous.as_mut());
        assert!(mounted_is_focused(
            &mut mounted,
            &mut tree,
            &node,
            row_ids[0].clone()
        ));

        let last = row_ids.last().unwrap().clone();
        let mut focus_last = iced::advanced::widget::operation::scope(
            panel_id.clone(),
            focusable::focus::<()>(last.clone()),
        );
        operate_mounted(&mut mounted, &mut tree, &node, &mut focus_last);
        let mut reveal = iced::advanced::widget::operation::scope(
            panel_id,
            RevealAudioControl::new(scroll_id.clone(), last.clone()),
        );
        operate_mounted(&mut mounted, &mut tree, &node, &mut reveal);
        let Outcome::Chain(mut apply_reveal) = reveal.finish() else {
            panic!("reveal must produce its scroll application pass");
        };
        operate_mounted(&mut mounted, &mut tree, &node, apply_reveal.as_mut());

        let mut visibility = AudioVisibilityProbe {
            scroll: scroll_id,
            target: last,
            viewport: None,
            translation: iced::Vector::ZERO,
            target_bounds: None,
        };
        operate_mounted(&mut mounted, &mut tree, &node, &mut visibility);
        let viewport = visibility.viewport.unwrap();
        let target = visibility.target_bounds.unwrap();
        let visible_top = target.y - visibility.translation.y;
        let visible_bottom = target.y + target.height - visibility.translation.y;
        assert!(visible_top >= viewport.y - f32::EPSILON);
        assert!(visible_bottom <= viewport.y + viewport.height + f32::EPSILON);

        // A row can disappear between key events (session close or package
        // replacement). The stored bookkeeping id is then absent from the
        // mounted tree. Production must directly focus and reveal the same
        // deterministic boundary row instead of issuing focus_next from no
        // actual panel focus and recording a different row.
        let removed = iced::widget::Id::from("removed-audio-row".to_string());
        let (recovered, direct) = next_audio_focus_target(&row_ids, Some(&removed), false).unwrap();
        assert!(direct);
        assert_eq!(recovered, row_ids[0]);
        let mut recover = iced::advanced::widget::operation::scope(
            root_id,
            focusable::focus::<()>(recovered.clone()),
        );
        operate_mounted(&mut mounted, &mut tree, &node, &mut recover);
        assert!(mounted_is_focused(
            &mut mounted,
            &mut tree,
            &node,
            recovered
        ));
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn layout_batch_keeps_terminals_usable_when_next_session_exceeds_physical_capacity() {
        let _core_runtime_lock = application_audio::lock_core_runtime_test();
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test Tokio runtime");
        let _tokio_guard = tokio.enter();
        let (application, probe) = application_audio::test_application_audio_with_core_runtime();
        let _renderer = application_audio::TestAudioRenderer::start(probe);
        let controller = application.controller();
        // Leave one of the mixer's 32 slots for the first real SessionStore
        // open. These staged sessions deliberately own no core runtime.
        for id in 100..131 {
            controller
                .begin_session(SessionId::from(id))
                .expect("filler registration")
                .commit();
        }
        let (ui_commands, _ui_events) = smudgy_core::session::ui_command::channel();
        let mut sessions = SessionStore::with_ui_commands_and_audio(
            crate::cloud_account::test_handles(),
            ui_commands,
            controller,
        );

        let spawns = [
            workspace::apply::SpawnSlot {
                slot: 1,
                server: "missing-layout-server".to_string(),
                profile: "first-profile".to_string(),
                connect: false,
            },
            workspace::apply::SpawnSlot {
                slot: 2,
                server: "missing-layout-server".to_string(),
                profile: "second-profile".to_string(),
                connect: false,
            },
        ];
        open_layout_spawn_batch(
            &mut sessions,
            &spawns,
            &workspace::TemplateSource::Named("test".to_string()),
        )
        .expect("physical capacity degrades only the overflowing session to silent Web Audio");
        let first = SessionId::from(0);
        let second = SessionId::from(1);
        assert_eq!(
            sessions.iter().count(),
            2,
            "both terminal sessions are published"
        );
        assert!(sessions.uses_physical_audio_for_test(first));
        assert!(!sessions.uses_physical_audio_for_test(second));
        assert!(smudgy_core::session::registry::get_runtime(first).is_some());
        assert!(smudgy_core::session::registry::get_runtime(second).is_some());
        assert_eq!(
            sessions.next_session_id_for_test(),
            Some(SessionId::from(2)),
            "the two successful ids are spent"
        );

        assert!(sessions.shutdown_and_remove(first));
        assert!(sessions.shutdown_and_remove(second));

        let report = application.shutdown();
        for session_id in [first, second] {
            assert!(
                report
                    .sessions
                    .iter()
                    .find(|result| result.session_id == session_id)
                    .copied()
                    .is_some_and(application_audio::SessionAudioCloseResult::is_clean),
                "published session {session_id} did not close cleanly: {report}"
            );
        }
        assert!(report.output.is_clean(), "physical output still shuts down");
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn successful_physical_store_open_publishes_explicit_audio_runtime() {
        let _core_runtime_lock = application_audio::lock_core_runtime_test();
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test Tokio runtime");
        let _tokio_guard = tokio.enter();
        let (application, probe) = application_audio::test_application_audio_with_core_runtime();
        let _renderer = application_audio::TestAudioRenderer::start(probe);
        let (ui_commands, _ui_events) = smudgy_core::session::ui_command::channel();
        let mut sessions = SessionStore::with_ui_commands_and_audio(
            crate::cloud_account::test_handles(),
            ui_commands,
            application.controller(),
        );
        let session_id = sessions
            .open_session(
                "missing-physical-server".to_string(),
                "physical-profile".to_string(),
                false,
            )
            .expect("physical open succeeds");

        assert!(sessions.uses_physical_audio_for_test(session_id));
        assert!(smudgy_core::session::registry::get_runtime(session_id).is_some());
        assert!(sessions.shutdown_and_remove(session_id));

        let report = application.shutdown();
        assert_eq!(report.sessions.len(), 1);
        assert!(report.sessions[0].is_clean());
        assert!(report.is_clean(), "{report}");
    }

    #[cfg(feature = "web-audio-cpal")]
    #[test]
    fn terminal_driver_state_makes_later_sessions_emulated_without_ui_latch() {
        let _core_runtime_lock = application_audio::lock_core_runtime_test();
        let tokio = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test Tokio runtime");
        let _tokio_guard = tokio.enter();
        let (mut application, probe) =
            application_audio::test_application_audio_with_core_runtime();
        let mut failures = application
            .take_output_failure_events()
            .expect("physical application exposes its unique terminal event receiver");
        let _renderer = application_audio::TestAudioRenderer::start(probe.clone());
        let controller = application.controller();
        let (ui_commands, _ui_events) = smudgy_core::session::ui_command::channel();
        let mut sessions = SessionStore::with_ui_commands_and_audio(
            crate::cloud_account::test_handles(),
            ui_commands,
            controller,
        );
        // The application-global receiver remains live without any session.
        assert!(probe.fail_output());
        let failure = futures::executor::block_on(async {
            use futures::StreamExt;
            failures.next().await
        })
        .expect("the global receiver wakes without a session or timer poll");
        assert_eq!(failure, MixerOutputFailure::BackendFailure);

        let later = sessions
            .open_session(
                "missing-dead-output-server".to_string(),
                "emulated-profile".to_string(),
                false,
            )
            .expect("terminal output failure never blocks a later terminal session");

        assert!(smudgy_core::session::registry::get_runtime(later).is_some());
        assert!(!sessions.uses_physical_audio_for_test(later));
        assert!(sessions.shutdown_and_remove(later));
        let report = application.shutdown();
        assert_eq!(report.sessions.len(), 1);
        assert!(
            report.sessions.iter().all(|session| session.is_clean()),
            "{report}"
        );
        assert!(
            report.is_clean(),
            "the retained output death stays visible without turning proven cleanup into a failed exit"
        );
    }

    fn area(n: u128) -> AreaId {
        AreaId(smudgy_cloud::Uuid::from_u128(n))
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
