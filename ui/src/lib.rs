#![allow(clippy::pedantic)]
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use crate::session_store::BindTarget;
use chrono::{DateTime, Utc};
use iced::widget::{center, pane_grid, text};
use iced::window;
use iced::window::settings::PlatformSpecific;
use iced::{Point, Rectangle, Size, Subscription, Task};
use smudgy_cloud::cloud_api::{AreaPref, CloudApiClient};
use smudgy_cloud::{AreaId, CloudError, Mapper};
use smudgy_core::models::map_scopes::{MapScopes, ScopeState};
use smudgy_core::models::settings::{MapAreaPref, Settings};
use smudgy_core::session::runtime::pane::{MAIN_PANE_KEY, PaneKey, PanePlacement, SplitDirection};
use smudgy_core::session::{SessionEvent, SessionId, TaggedSessionEvent};

// Core session imports
use windows::automations_window::{AutomationsWindow, Event as AutomationsWindowEvent};
use windows::settings_window::{self, Event as SettingsWindowEvent, SettingsWindow};
use windows::smudgy_window::SmudgyWindow;

mod assets;
mod cloud_account;
mod i18n;
mod pane_drag;
mod pane_layout;
pub mod prefs;
mod session_store;
pub mod terminal_buffer;
mod update;
mod widgets;
mod win_rm;

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
const MAIN_WINDOW_TITLE: &str = match smudgy_core::models::settings::build_channel() {
    smudgy_core::models::settings::BuildChannel::Dev => "smudgy - DEV BUILD",
    smudgy_core::models::settings::BuildChannel::ReleaseCandidate => {
        concat!("smudgy - RELEASE CANDIDATE ", env!("CARGO_PKG_VERSION"))
    }
    smudgy_core::models::settings::BuildChannel::Release => "smudgy",
};

use crate::cloud_account::CloudAccount;
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
    /// All live sessions, window-independent: windows' grids hold pane
    /// references into this store, and session events route here directly.
    sessions: SessionStore,
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
    /// Window origins/sizes/scales/cursors + focus MRU, observed from the
    /// event stream. The cross-window drag layer reconstructs screen-space
    /// geometry from this (iced has no direct "window under this screen
    /// point" query).
    window_tracker: pane_drag::WindowTracker,
    /// The pane drag in flight, if any (the DragController's state).
    /// Recorded at `Picked`; resolved at `Dropped`/`Canceled`; aborted by
    /// pane/session/source-window death mid-drag.
    pane_drag: Option<pane_drag::ActiveDrag>,
    /// Smudgy windows we have asked to close but whose async `CloseWindow`
    /// event has not yet landed. They linger in `smudgy_windows` in the
    /// meantime, so the empty-window sweep must not count them as "remaining"
    /// — otherwise two windows emptied in separate updates can each close and
    /// leave zero windows, exiting the app against the keep-one-alive rule.
    closing_windows: HashSet<window::Id>,
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
    /// A window-geometry observation (moved/resized/rescaled/focused/cursor)
    /// for the tracker feeding the cross-window drag layer.
    WindowTracking(window::Id, pane_drag::TrackEvent),
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

/// Settings for main smudgy windows: borderless, with the toolbar acting as
/// the titlebar (drag area + window controls) and resize grips at the edges.
fn smudgy_window_settings() -> window::Settings {
    window::Settings {
        decorations: false,
        min_size: Some(Size::new(640.0, 400.0)),
        exit_on_close_request: true,
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
    let (_id, open) = window::open(smudgy_window_settings());

    // Seed the hot prefs snapshot before any window renders, and load the
    // per-area enable/disable preferences (migrating a legacy disabled-only
    // file) for fan-out to mappers and cross-device reconcile. `load_settings`
    // also folds in the installer's update-check seed, which overrides the
    // persisted auto-check value while present.
    let settings = smudgy_core::models::settings::load_settings();
    i18n::activate(&settings.locale);
    prefs::apply(&settings);
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

    let sessions = SessionStore::new(account.handles());

    (
        Smudgy {
            account,
            sessions,
            smudgy_windows: BTreeMap::new(),
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
            pane_drag: None,
            closing_windows: HashSet::new(),
        },
        Task::batch([
            open.map(Message::NewSmudgyWindow),
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
        .subscription(subscription)
        .font(assets::fonts::GEIST_VF_BYTES)
        .font(assets::fonts::GEIST_MONO_VF_BYTES)
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
        .default_font(assets::fonts::GEIST_VF)
        .title(|smudgy: &Smudgy, window_id: window::Id| {
            if let Some(window) = smudgy.automations_windows.get(&window_id) {
                format!("smudgy automations - {}", window.server_name())
            } else if let Some(window) = smudgy.map_editor_windows.get(&window_id) {
                window.title()
            } else {
                MAIN_WINDOW_TITLE.to_string()
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
        // Window geometry + cursor tracking for cross-window pane drags.
        // `listen_with` (not `listen`): pane_grid captures the initiating
        // press, and captured events must still reach the tracker. The full
        // filter maps every window move and mouse motion to a message — and
        // every message rebuilds and repaints all windows — so it runs only
        // while a drag is in flight; idle windows use the rare-events filter.
        if smudgy.pane_drag.is_some() {
            iced::event::listen_with(window_tracking_event)
        } else {
            iced::event::listen_with(window_tracking_idle_event)
        },
    ];

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

    Subscription::batch(subs)
}

/// `event::listen_with` filter feeding the window tracker while a pane drag
/// is in flight. Runs for every window (map editors and settings included —
/// drop-target membership is filtered where the tracker is read, not here).
fn window_tracking_event(
    event: iced::Event,
    _status: iced::event::Status,
    window_id: window::Id,
) -> Option<Message> {
    pane_drag::track_event(&event).map(|track| Message::WindowTracking(window_id, track))
}

/// The no-drag counterpart of [`window_tracking_event`]: tracks only the
/// rare geometry facts, so window moves and mouse motion cost nothing.
fn window_tracking_idle_event(
    event: iced::Event,
    _status: iced::event::Status,
    window_id: window::Id,
) -> Option<Message> {
    pane_drag::track_event_idle(&event).map(|track| Message::WindowTracking(window_id, track))
}

fn update(smudgy: &mut Smudgy, message: Message) -> Task<Message> {
    match message {
        Message::WindowTracking(id, event) => {
            smudgy.window_tracker.apply(id, event);
            // The first cursor position tracked for the source window is the
            // drag's deadband reference: tracking starts with the drag, so
            // the pick itself is never observed.
            if let pane_drag::TrackEvent::CursorMoved(position) = event
                && let Some(drag) = &mut smudgy.pane_drag
                && drag.source_window == id
                && drag.pick_cursor.is_none()
            {
                drag.pick_cursor = Some(position);
            }
            Task::none()
        }
        Message::CloseWindow(id) => {
            smudgy.window_tracker.remove(id);
            smudgy.closing_windows.remove(&id);
            // The source window dying mid-drag ends the drag: its grid is
            // gone, so no terminal DragEvent will ever arrive for this pick.
            if smudgy
                .pane_drag
                .is_some_and(|drag| drag.source_window == id)
            {
                smudgy.pane_drag = None;
            }
            if let Some(window) = smudgy.smudgy_windows.remove(&id) {
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
                        PanePlacement {
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
                    Task::batch([purge_task, iced::exit()])
                } else {
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

            match update.event {
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
                Some(SmudgyWindowEvent::PaneDragPicked { pane, slot }) => {
                    // A fresh pick also supersedes any stale record from a
                    // drag that ended without a terminal event (cursor
                    // unavailable at release).
                    smudgy.pane_drag = Some(pane_drag::ActiveDrag {
                        source_window: id,
                        grid_pane: pane,
                        slot,
                        // Seeded by the first cursor position tracked during
                        // the drag (cursor tracking only runs mid-drag).
                        pick_cursor: None,
                    });
                    // Origins go stale while idle (`Moved` is only tracked
                    // mid-drag): re-query every candidate window's position
                    // for the release hit-test. The answers race the drag,
                    // but a human drag outlasts a task round-trip by orders
                    // of magnitude.
                    let mut tasks = vec![task];
                    for &window_id in smudgy.smudgy_windows.keys() {
                        tasks.push(window::position(window_id).map(move |origin| {
                            Message::WindowTracking(
                                window_id,
                                pane_drag::TrackEvent::Origin(origin),
                            )
                        }));
                    }
                    Task::batch(tasks)
                }
                Some(SmudgyWindowEvent::PaneDragEnded) => {
                    smudgy.pane_drag = None;
                    task
                }
                Some(SmudgyWindowEvent::PaneDragCanceled(pane)) => {
                    Task::batch([task, finish_drag_canceled(smudgy, id, pane)])
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
                None => task,
            }
        }
        Message::SessionEvent(TaggedSessionEvent { session_id, event }) => {
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
                _ => Task::none(),
            };
            // Pane lifecycle touches both the store (display state, handled
            // by the session's own update below) and the windows' grids
            // (handled here at the daemon, which owns the window map).
            let pane_lifecycle = match &event {
                SessionEvent::PaneOpened { def, placement } => Some((def.key, Some(*placement))),
                SessionEvent::PaneClosed(key) => Some((*key, None)),
                _ => None,
            };
            if let Some(session) = smudgy.sessions.get_mut(session_id) {
                let task = session
                    .update(session_store::Message::SessionEvent(event))
                    .map(move |msg| Message::SessionAction(session_id, msg));
                let pane_task = match pane_lifecycle {
                    Some((key, Some(placement))) => {
                        place_pane_in_windows(smudgy, session_id, key, placement);
                        Task::none()
                    }
                    Some((key, None)) => remove_pane_from_windows(smudgy, session_id, key),
                    None => Task::none(),
                };
                Task::batch([task, pane_task, scope_task])
            } else {
                // The session was torn down (its store entry goes first) with
                // this event already in flight; dropping the event here is
                // what keeps a dead session from re-entering any grid.
                log::debug!("Dropping event for closed session {session_id}");
                Task::none()
            }
        }
        Message::SessionAction(session_id, msg) => {
            // A close routes through the daemon teardown (store + grids +
            // empty-window rule) rather than the session itself; checking
            // before the store lookup makes a repeated close a clean no-op.
            if matches!(msg, session_store::Message::Close) {
                return close_session(smudgy, session_id);
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
            Task::batch([
                // Install the Restart Manager shutdown hook on this window's
                // HWND so the installer can close smudgy for an in-place
                // upgrade.
                window::raw_id::<Message>(id).map(Message::HookWindowForShutdown),
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

/// Close one session: shut its runtime down and remove it from the store
/// *first* — so events still in flight for the id are dropped at the daemon —
/// then clean its panes out of every window's grid and apply the empty-window
/// rule. A repeat close (double-clicked ✕, a late queued task) is a no-op.
fn close_session(smudgy: &mut Smudgy, session_id: SessionId) -> Task<Message> {
    if !smudgy.sessions.shutdown_and_remove(session_id) {
        return Task::none();
    }
    log::info!("Closed session {session_id}");
    purge_sessions_from_windows(smudgy, &[session_id])
}

/// Remove the dead sessions' panes from every window's grid, repairing each
/// window's active-session state, then close any window the purge emptied —
/// always keeping at least one smudgy window alive (the last one stays open
/// showing the empty connect state).
fn purge_sessions_from_windows(smudgy: &mut Smudgy, dead: &[SessionId]) -> Task<Message> {
    // A dragged pane whose session died mid-drag must never transplant.
    if smudgy
        .pane_drag
        .is_some_and(|drag| dead.contains(&drag.slot.session_id))
    {
        smudgy.pane_drag = None;
    }

    let mut tasks: Vec<Task<Message>> = Vec::new();
    let mut emptied: Vec<window::Id> = Vec::new();

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

/// Place a freshly opened script pane into the window hosting its reference
/// pane — falling back to the window hosting the session's main pane, then
/// any window. (A script splitting against a pane whose window vanished
/// mid-flight lands next to the main pane.)
fn place_pane_in_windows(
    smudgy: &mut Smudgy,
    session_id: SessionId,
    key: PaneKey,
    placement: PanePlacement,
) {
    let target = smudgy
        .smudgy_windows
        .iter()
        .find_map(|(id, window)| {
            window
                .hosts_pane(session_id, placement.reference)
                .then_some(*id)
        })
        .or_else(|| {
            smudgy.smudgy_windows.iter().find_map(|(id, window)| {
                window.hosts_pane(session_id, MAIN_PANE_KEY).then_some(*id)
            })
        })
        .or_else(|| smudgy.smudgy_windows.keys().next().copied());
    match target.and_then(|id| smudgy.smudgy_windows.get_mut(&id)) {
        Some(window) => window.place_session_pane(session_id, key, placement),
        None => log::warn!("No window available to place {key} for session {session_id}"),
    }
}

/// Drop one closed pane's slot from whatever window hosts it, then apply the
/// empty-window rule.
fn remove_pane_from_windows(
    smudgy: &mut Smudgy,
    session_id: SessionId,
    key: PaneKey,
) -> Task<Message> {
    // The dragged pane closing mid-drag (script `pane.close()`) aborts the drag.
    if smudgy
        .pane_drag
        .is_some_and(|drag| drag.slot.session_id == session_id && drag.slot.key == key)
    {
        smudgy.pane_drag = None;
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

/// Resolves a `DragEvent::Canceled` for the drag in flight. pane_grid uses
/// `Canceled` for several distinct outcomes — a plain click (release within
/// the pick deadband), a release over the source window itself (the picked
/// pane, inter-pane spacing, chrome), and a release outside the source
/// window entirely. Only the last is ours to act on: hit-test the other
/// smudgy windows (most-recently-focused first — the best-effort stand-in
/// for z-order) and transplant, or tear the pane out into a new window.
fn finish_drag_canceled(
    smudgy: &mut Smudgy,
    window_id: window::Id,
    pane: pane_grid::Pane,
) -> Task<Message> {
    let Some(drag) = smudgy.pane_drag.take() else {
        return Task::none();
    };
    if drag.source_window != window_id || drag.grid_pane != pane {
        return Task::none();
    }
    // Re-validate against the live world before touching any grid: the
    // session and the slot must both still be where the pick recorded them.
    if smudgy.sessions.get(drag.slot.session_id).is_none() {
        return Task::none();
    }
    let Some(source) = smudgy.smudgy_windows.get(&window_id) else {
        return Task::none();
    };
    if source.pane_slot(pane) != Some(drag.slot) {
        return Task::none();
    }
    let Some(track) = smudgy.window_tracker.get(window_id).copied() else {
        return Task::none();
    };
    let (Some(release), Some(pick)) = (track.cursor, drag.pick_cursor) else {
        return Task::none();
    };
    // Within the pick deadband: a plain title-bar click.
    if release.distance(pick) <= pane_drag::DRAG_DEADBAND {
        return Task::none();
    }
    // Anywhere over the source window (grid, chrome, spacing): the native
    // no-op re-dock.
    if Rectangle::with_size(track.size).contains(release) {
        return Task::none();
    }

    // The release in physical screen space — unknown when the source
    // window's origin is (Wayland has no global positions; cross-window
    // drops then degrade to tear-out, per the plan).
    let screen = track
        .origin
        .map(|origin| pane_drag::screen_point(origin, release, track.scale));

    if let Some(screen) = screen {
        for target_id in smudgy.window_tracker.mru_order() {
            if target_id == window_id || !smudgy.smudgy_windows.contains_key(&target_id) {
                continue;
            }
            let Some(target_track) = smudgy.window_tracker.get(target_id).copied() else {
                continue;
            };
            let Some(local) = pane_drag::window_local(&target_track, screen) else {
                continue;
            };
            return transplant_pane(smudgy, drag, target_id, local, target_track.size);
        }
    }

    tear_out_pane(smudgy, drag, screen)
}

/// Moves the dragged pane into `target_id`'s grid at the window-local drop
/// point. Order matters: validate the target exists, remove from the source,
/// insert into the target — the slot is out of every grid only within this
/// single update. Moving a pane never touches core: pane existence, buffers,
/// and routing are window-agnostic.
fn transplant_pane(
    smudgy: &mut Smudgy,
    drag: pane_drag::ActiveDrag,
    target_id: window::Id,
    local: Point,
    target_size: Size,
) -> Task<Message> {
    let source_id = drag.source_window;
    if !smudgy.smudgy_windows.contains_key(&target_id) {
        return Task::none();
    }
    let Some(source) = smudgy.smudgy_windows.get_mut(&source_id) else {
        return Task::none();
    };
    let emptied = source.remove_pane_slot(drag.slot.session_id, drag.slot.key);
    let repair = source
        .repair_active_session(&smudgy.sessions)
        .map(move |msg| Message::SmudgyWindowMessage(source_id, msg));
    if let Some(target) = smudgy.smudgy_windows.get_mut(&target_id) {
        target.accept_transplant(drag.slot, local, target_size);
    }
    // The drop also moves the user's attention: activate the pane's session
    // in the target window (input focus follows only if its main pane is
    // there, per the activation rules).
    let activate = Task::done(Message::SmudgyWindowMessage(
        target_id,
        windows::smudgy_window::Message::SetActiveSession(drag.slot.session_id),
    ));
    let close = if emptied {
        close_emptied_windows(smudgy, vec![source_id])
    } else {
        Task::none()
    };
    Task::batch([repair, activate, close])
}

/// Tears the dragged pane out into a new smudgy window at the release point.
/// The window entry is inserted synchronously (the open task completes
/// later) so the pane has a grid to live in from this update on;
/// `NewSmudgyWindow` then finds the entry present and only installs the
/// shutdown hook and seeds the tracker.
fn tear_out_pane(
    smudgy: &mut Smudgy,
    drag: pane_drag::ActiveDrag,
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
    let emptied = source.remove_pane_slot(drag.slot.session_id, drag.slot.key);
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
    torn_out.adopt_torn_out_pane(drag.slot);
    smudgy.smudgy_windows.insert(id, torn_out);

    let activate = Task::done(Message::SmudgyWindowMessage(
        id,
        windows::smudgy_window::Message::SetActiveSession(drag.slot.session_id),
    ));
    let close = if emptied {
        close_emptied_windows(smudgy, vec![source_id])
    } else {
        Task::none()
    };
    Task::batch([
        open_task.map(Message::NewSmudgyWindow),
        activate,
        close,
        repair,
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
    if mapper.is_ephemeral(&area_id) || mapper.local_area_ids().contains(&area_id) {
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
        center(
            window
                .view(&smudgy.sessions)
                .map(move |message| Message::SmudgyWindowMessage(id, message)),
        )
        .into()
    } else if let Some(window) = smudgy.automations_windows.get(&id) {
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
        text("No windows open").into()
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
