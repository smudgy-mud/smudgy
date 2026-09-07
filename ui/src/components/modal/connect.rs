use crate::i18n::t;
use iced::widget::{Id, Row, button, column, container, operation, text, text_editor};
use iced::{Length, Pixels, Task};
use log::warn;

use crate::theme::Element;
use crate::theme::builtins;

// Keep core model imports
use smudgy_core::models::observed::{ObservedServer, ObservedValue};
use smudgy_core::models::profile::{
    ProfileCas, clear_profile_password_if_unchanged, has_profile_password,
};
use smudgy_core::models::server::{
    ServerCas, link_url_host, load_server, update_server_if_unchanged, with_server_if_unchanged,
};
use smudgy_core::models::{profile::Profile, server::Server};
use std::collections::HashMap;

mod observed;
mod profile;
mod server;

#[cfg(test)]
mod tests;

use profile::{
    handle_delete_profile, handle_submit_profile_form, load_profiles_async, view_profile_form,
    view_server_details_and_profiles,
};
use server::{
    execute_server_operation, handle_submit_server_form, view_server_form, view_server_list,
};

// --- Module-specific types ---

pub type ServerName = String;
pub type ProfileName = String;

// Stable widget ids for the connect-modal form fields. These let each form
// auto-focus its first field when it opens and let `Tab`/`Shift+Tab` walk the
// fields in order (the traversal itself is driven from `smudgy_window` via
// `operation::focus_next`/`focus_previous`). See `server.rs`/`profile.rs`.
pub(super) fn server_name_input_id() -> Id {
    Id::new("connect-server-name")
}
pub(super) fn server_host_input_id() -> Id {
    Id::new("connect-server-host")
}
pub(super) fn server_port_input_id() -> Id {
    Id::new("connect-server-port")
}
pub(super) fn profile_name_input_id() -> Id {
    Id::new("connect-profile-name")
}
pub(super) fn profile_description_input_id() -> Id {
    Id::new("connect-profile-description")
}
pub(super) fn profile_send_on_connect_id() -> Id {
    Id::new("connect-profile-send-on-connect")
}
pub(super) fn profile_password_input_id() -> Id {
    Id::new("connect-profile-password")
}

// Events emitted by this modal back to the main application
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    CloseModalRequested,
    Connect(ServerName, ProfileName),
    /// Open the session without connecting: the runtime, mapper, and automations
    /// come up so the map editor / automations can be used offline.
    OpenOffline(ServerName, ProfileName),
    /// Restore the server's last-session snapshot: spawn its stored slots
    /// per their connection intent and apply the stored arrangement (the
    /// full user-restore flow, owned by the daemon).
    RestoreLastSession(ServerName),
}

// Messages handled internally by this modal's update logic
#[derive(Debug, Clone)]
pub enum Message {
    // Data Loading. (Servers + the first server's profiles are loaded
    // synchronously up front in `State::opening`, so there is no `ServersLoaded`
    // round trip; `ProfilesLoaded` still backs selecting *other* servers.)
    ProfilesLoaded(u64, Server, Result<ServerCas<Vec<Profile>>, String>),
    // UI Interaction
    SelectServer(ServerName),
    // Handled in `update()` (maps to `Event::CloseModalRequested`); the parent
    // does not yet send this on Esc / background click.
    #[allow(dead_code)]
    CloseRequested, // E.g., from Esc key or background click mapped by parent
    ConnectProfile(ServerName, ProfileName),
    OpenOfflineProfile(ServerName, ProfileName),
    RestoreLastSession(ServerName),
    // Server CRUD UI Actions
    RequestCreateServer,
    RequestEditServer(ServerName),
    RequestConfirmDeleteServer(ServerName), // User clicks delete in details view
    ConfirmDeleteServer,                    // User confirms deletion
    // Server Form Interaction
    UpdateServerFormField(ServerFormField, String),
    ToggleServerCompression(bool),
    ToggleServerMccp4Compression(bool),
    ToggleServerTls(bool),
    ToggleServerTlsVerify(bool),
    SubmitServerForm,
    CancelServerForm,
    // Server CRUD Async Results
    ServerOperationFinished(ServerOperationCompletion),
    // --- Image cache (plan D10: per-server management in the edit pane) ---
    ImageCacheUsageLoaded(u64, Server, Result<ServerCas<u64>, String>),
    RequestClearImageCache(Server),
    ImageCacheCleared(u64, Server, Result<ServerCas<()>, String>),
    // --- Profile CRUD ---
    // UI Actions (act on selected_server)
    RequestCreateProfile,
    RequestEditProfile(ProfileName),
    RequestConfirmDeleteProfile,
    ConfirmDeleteProfile,
    // Form Interaction
    UpdateProfileFormField(ProfileFormField, String),
    UpdateProfileFormSendOnConnect(text_editor::Action),
    SubmitProfileForm,
    CancelProfileForm,
    // Auto-login password ($PASSWORD)
    UpdateProfileFormPassword(ProfilePasswordInput),
    RequestChangeProfilePassword,
    ClearProfilePassword,
    // Async Results
    ProfileOperationFinished(ProfileOperationCompletion),
    // --- Observed-metadata band (server-supplied URLs; same trust gate as
    // --- server OSC 8 links, persisting into the same `server.json` grants) ---
    /// An MSSP `ICON` fetch settled: `Some` carries the display handle of the
    /// freshly cached artifact, `None` a refusal/failure (any cached icon
    /// keeps rendering).
    ServerIconFetched(Server, Option<iced::widget::image::Handle>),
    OpenObservedLink(Server, String),
    ObservedLinkGrantHost(bool),
    ObservedLinkGrantServer(bool),
    ObservedLinkProceed,
    ObservedLinkCancel,
}

/// Fields in the server create/edit form.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ServerFormField {
    Name, // Only for Create
    Host,
    Port,
    Encoding,
}

/// Fields in the profile create/edit form.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProfileFormField {
    Name,
    Description,
}

/// A password-field edit that stays redacted when an iced message is formatted for diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub struct ProfilePasswordInput(String);

impl std::fmt::Debug for ProfilePasswordInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("<redacted>")
    }
}

impl From<String> for ProfilePasswordInput {
    fn from(value: String) -> Self {
        Self(value)
    }
}

/// Temporary storage for server form input.
#[derive(Debug)]
pub struct ServerConfigFormData {
    pub name: String,
    pub host: String,
    pub port: String,
    /// The encoding dropdown's display value; [`server::DEFAULT_ENCODING_CHOICE`]
    /// stands for "no override" (UTF-8, `ServerConfig::encoding = None`).
    pub encoding: String,
    /// Whether inbound MCCP2 compression offers are accepted.
    pub compression: bool,
    /// Whether inbound MCCP4 compression offers are accepted.
    pub mccp4_compression: bool,
    /// Connect over TLS.
    pub tls: bool,
    /// When `tls`, verify the server certificate (off = accept any, insecure).
    pub tls_verify: bool,
}

impl Default for ServerConfigFormData {
    fn default() -> Self {
        Self {
            name: String::new(),
            host: String::new(),
            port: String::new(),
            encoding: server::DEFAULT_ENCODING_CHOICE.to_string(),
            compression: true,
            mccp4_compression: true,
            tls: false,
            tls_verify: true,
        }
    }
}

/// Temporary storage for profile form input. `description` maps to the persisted
/// `ProfileConfig.caption` field (the on-disk name is kept for back-compat).
#[derive(Debug, Default)]
pub struct ProfileConfigFormData {
    pub name: String,
    pub description: String,
}

/// Represents the current server-related action being performed (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerCrudAction {
    Create,
    Edit(Server),
    ConfirmDelete(Server),
}

/// The exact persistence request submitted by one server form. The task owns a clone; state
/// retains this copy solely to authenticate its eventual completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServerOperationEnvelope {
    id: u64,
    action: ServerOperationAction,
    form_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServerOperationAction {
    Create {
        target_name: ServerName,
        config: smudgy_core::models::server::ServerConfig,
    },
    Update {
        expected: Server,
        config: smudgy_core::models::server::ServerConfig,
    },
    Delete {
        expected: Server,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServerOperationKind {
    Create,
    Update,
    Delete,
}

/// Password-free identity echoed by a background task. Matching the complete key prevents a
/// delayed or duplicate completion from consuming a newer pending operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServerOperationKey {
    id: u64,
    target_name: ServerName,
    kind: ServerOperationKind,
}

impl ServerOperationEnvelope {
    fn key(&self) -> ServerOperationKey {
        let (target_name, kind) = match &self.action {
            ServerOperationAction::Create { target_name, .. } => {
                (target_name.clone(), ServerOperationKind::Create)
            }
            ServerOperationAction::Update { expected, .. } => {
                (expected.name.clone(), ServerOperationKind::Update)
            }
            ServerOperationAction::Delete { expected } => {
                (expected.name.clone(), ServerOperationKind::Delete)
            }
        };
        ServerOperationKey {
            id: self.id,
            target_name,
            kind,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum AppliedServerOperation {
    Created(Server),
    Updated(Server),
    Deleted,
}

/// Why a server operation did not apply. The compare-and-swap conflict is typed so its wording is
/// translated where the completion is displayed rather than on the worker that observed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ServerOperationError {
    /// The stored server no longer matched the snapshot the form was opened from.
    StateChanged,
    Failed(String),
}

impl ServerOperationError {
    /// The user-facing text for this failure of `action`, before the per-action wrapper.
    fn localized(&self, action: &ServerOperationAction) -> String {
        match self {
            Self::StateChanged => match action {
                ServerOperationAction::Delete { .. } => t!("server-error-delete-state-changed"),
                ServerOperationAction::Create { .. } | ServerOperationAction::Update { .. } => {
                    t!("server-error-state-changed")
                }
            },
            Self::Failed(error) => error.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ServerOperationCompletion {
    key: ServerOperationKey,
    result: Result<AppliedServerOperation, ServerOperationError>,
}

/// Represents the current profile-related action being performed (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileCrudAction {
    Create { server: Server },
    Edit { server: Server, expected: Profile },
    ConfirmDelete { server: Server, expected: Profile },
}

impl ProfileCrudAction {
    fn server_name(&self) -> &str {
        match self {
            Self::Create { server }
            | Self::Edit { server, .. }
            | Self::ConfirmDelete { server, .. } => &server.name,
        }
    }
}

/// The password effect captured with a profile operation. Its custom `Debug` implementation keeps
/// the credential out of messages, state diagnostics, and test-failure output.
#[derive(Clone, PartialEq, Eq)]
pub(super) enum ProfilePasswordAction {
    Keep,
    Set(String),
    Clear,
}

impl std::fmt::Debug for ProfilePasswordAction {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Keep => formatter.write_str("Keep"),
            Self::Set(_) => formatter.write_str("Set(<redacted>)"),
            Self::Clear => formatter.write_str("Clear"),
        }
    }
}

/// The exact persistence request submitted by one profile form. The task owns a clone; state
/// retains this copy solely to authenticate its eventual completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProfileOperationEnvelope {
    id: u64,
    server: Server,
    action: ProfileOperationAction,
    password: ProfilePasswordAction,
    form_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProfileOperationAction {
    Create {
        target_name: ProfileName,
        config: smudgy_core::models::profile::ProfileConfig,
    },
    Update {
        expected: Profile,
        target_name: ProfileName,
        config: smudgy_core::models::profile::ProfileConfig,
    },
    Delete {
        expected: Profile,
        target_name: ProfileName,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProfileOperationKind {
    Create,
    Update,
    Delete,
}

/// Password-free identity echoed by a background task. Matching the complete key prevents a
/// delayed or duplicate completion from consuming a newer pending operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ProfileOperationKey {
    id: u64,
    server_name: ServerName,
    target_name: ProfileName,
    kind: ProfileOperationKind,
}

impl ProfileOperationEnvelope {
    fn key(&self) -> ProfileOperationKey {
        let (target_name, kind) = match &self.action {
            ProfileOperationAction::Create { target_name, .. } => {
                (target_name.clone(), ProfileOperationKind::Create)
            }
            ProfileOperationAction::Update { target_name, .. } => {
                (target_name.clone(), ProfileOperationKind::Update)
            }
            ProfileOperationAction::Delete { target_name, .. } => {
                (target_name.clone(), ProfileOperationKind::Delete)
            }
        };
        ProfileOperationKey {
            id: self.id,
            server_name: self.server.name.clone(),
            target_name,
            kind,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) enum AppliedProfileOperation {
    Created(Profile, Option<ProfilePasswordWarning>),
    Updated(Profile, Option<ProfilePasswordWarning>),
    Deleted,
}

/// A password side effect that failed after the profile configuration had already committed.
#[derive(Debug, Clone)]
pub(super) enum ProfilePasswordWarning {
    StateChanged,
    Failed(String),
}

/// Why a profile operation did not apply. The compare-and-swap conflict is typed so its wording is
/// translated where the completion is displayed rather than on the worker that observed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum ProfileOperationError {
    /// The stored server (for a create) or profile no longer matched the snapshot the form was
    /// opened from.
    StateChanged,
    Failed(String),
}

impl ProfileOperationError {
    /// The user-facing text for this failure of `action`, before the per-action wrapper.
    fn localized(&self, action: &ProfileOperationAction) -> String {
        match self {
            Self::StateChanged => match action {
                ProfileOperationAction::Create { .. } => t!("profile-error-server-state-changed"),
                ProfileOperationAction::Update { .. } => t!("profile-error-state-changed"),
                ProfileOperationAction::Delete { .. } => t!("profile-error-delete-state-changed"),
            },
            Self::Failed(error) => error.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ProfileOperationCompletion {
    key: ProfileOperationKey,
    result: Result<AppliedProfileOperation, ProfileOperationError>,
}

/// One observed-metadata link (MSSP `DISCORD`/`WEBSITE`/`CONTACT`) held at
/// the trust gate, with the confirm dialog's checkbox state. The mirror of
/// the session view's pending link confirm, keyed by server since the modal
/// is not session-bound.
#[derive(Debug, Clone)]
struct ObservedLinkConfirm {
    /// The server whose metadata supplied the URL (and whose grants apply).
    server: Server,
    /// The gated URL, opened verbatim on Proceed.
    url: String,
    /// Safe, middle-elided display copy (invisible characters escaped).
    display: String,
    /// The URL's host (the per-host grant key).
    host: Option<String>,
    grant_host: bool,
    grant_server: bool,
}

// State managed by this modal
pub struct State {
    servers: Vec<Server>,
    /// Per-server observed sidecar (`observed.json`) — the metadata band's
    /// source. Absent entry = nothing observed yet; that server renders
    /// exactly as it did before the feature existed. Loaded with the server
    /// list and refreshed by the daemon's `ObservedServerChanged` nudge.
    observed: HashMap<ServerName, ObservedServer>,
    /// A metadata-band link held at the trust gate — the same per-server
    /// gate server OSC 8 links pass, sharing its `server.json` grant store.
    link_confirm: Option<ObservedLinkConfirm>,
    /// Per-server display handles for the MSSP `ICON` cache (`icon.png` in the
    /// server's directory). Loaded with the server list; a fetch task replaces
    /// an entry when the observed `ICON` value changes. Absent entry = the
    /// title renders iconless, exactly as it did before the feature existed.
    icons: HashMap<ServerName, iced::widget::image::Handle>,
    profiles: HashMap<ServerName, Vec<Profile>>,
    /// Per probed server: the profile names of its last-session snapshot in
    /// slot order, or `None` when no usable snapshot exists. Absent until a
    /// server is selected (each selection probes once per modal opening);
    /// the detail pane offers "Restore last session" only for a `Some`.
    last_sessions: HashMap<ServerName, Option<Vec<String>>>,
    selected_server: Option<ServerName>,
    is_loading_servers: bool,
    is_loading_profiles: Option<ServerName>,
    /// Monotonic identity of the latest profile-list read.
    profile_load_sequence: u64,
    /// Exact server snapshot owned by the only profile-list read allowed to update this modal.
    pending_profile_load: Option<(u64, Server)>,
    // --- Server CRUD State ---
    /// Tracks if we are currently creating or editing a server.
    server_action: Option<ServerCrudAction>,
    /// Holds the temporary data entered into the server form.
    server_form_data: ServerConfigFormData, // Use Default::default()
    /// Holds any error message related to server CRUD operations.
    server_crud_error: Option<String>,
    /// Advances whenever the active server form or its contents change. A completion may
    /// close/reset the form only while this still equals its submit-time snapshot.
    server_form_revision: u64,
    /// Monotonic source for server-operation completion keys.
    server_operation_sequence: u64,
    /// The one server operation allowed in flight. Its immutable envelope authenticates the
    /// completion and prevents a second submit from racing the first.
    pending_server_operation: Option<ServerOperationEnvelope>,
    // --- Profile CRUD State ---
    /// Tracks if we are currently creating or editing a profile.
    profile_action: Option<ProfileCrudAction>,
    /// Holds the temporary data entered into the profile form.
    profile_form_data: ProfileConfigFormData,
    profile_form_send_on_connect_content: text_editor::Content,
    /// Holds any error message related to profile CRUD operations.
    profile_crud_error: Option<String>,
    /// Fatal/session-admission error from the daemon-owned open transaction.
    /// Kept separate from CRUD validation so the Connect surface can remain
    /// intact and retryable when no session or pane was published.
    session_open_error: Option<String>,
    /// Secure-input buffer for a new auto-login password. Never persisted to disk;
    /// stored in the OS keyring on save. Empty unless the user is entering one.
    profile_form_password: String,
    /// Whether a password is already stored in the keyring for the profile being edited.
    profile_form_password_stored: bool,
    /// Whether to show the password input (`true`) vs the "saved" chip (`false`).
    profile_form_password_editing: bool,
    /// Advances whenever the active profile form or its owning server changes. A completion may
    /// close/reset the form only while this still equals its submit-time snapshot.
    profile_form_revision: u64,
    /// Monotonic source for profile-operation completion keys.
    profile_operation_sequence: u64,
    /// The one operation allowed in flight. Its immutable envelope authenticates the completion
    /// and prevents a second submit from racing the first.
    pending_profile_operation: Option<ProfileOperationEnvelope>,
    /// On-disk bytes of the edited server's cached images (`None` while loading).
    /// Refreshed when the edit form opens and after "Clear image cache".
    image_cache_usage: Option<u64>,
    /// Monotonic id of the most recent usage request. `Task::perform` completions are
    /// unordered: a slow pre-clear scan must not overwrite the post-clear figure.
    image_cache_usage_request: u64,
}

// Manual `Debug` (not derived) so the live credential buffer `profile_form_password`
// never reaches a Debug/log surface; the rest is summarized for diagnostics.
impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("servers", &self.servers.len())
            .field("selected_server", &self.selected_server)
            .field("server_action", &self.server_action)
            .field(
                "pending_server_operation",
                &self
                    .pending_server_operation
                    .as_ref()
                    .map(ServerOperationEnvelope::key),
            )
            .field("profile_action", &self.profile_action)
            .field(
                "pending_profile_operation",
                &self
                    .pending_profile_operation
                    .as_ref()
                    .map(ProfileOperationEnvelope::key),
            )
            .field("profile_form_password", &"<redacted>")
            .field(
                "profile_form_password_stored",
                &self.profile_form_password_stored,
            )
            .finish_non_exhaustive()
    }
}

fn initial_server<'a>(servers: &'a [Server], recent: Option<&str>) -> Option<&'a Server> {
    servers
        .iter()
        .find(|server| Some(server.name.as_str()) == recent)
        .or_else(|| servers.first())
}

impl State {
    /// Populates the server list and the most recently used server's profiles,
    /// falling back to the first server. These are small local-disk reads, so
    /// doing them up front lets the modal render fully populated on the very first
    /// frame — no "Loading servers…/profiles…" flash. Reads that fail fall back to
    /// an empty (welcome) state.
    #[must_use]
    pub fn opening() -> Self {
        let mut state = State::default();
        let servers = smudgy_core::models::server::list_servers().unwrap_or_else(|e| {
            warn!("Failed to load servers for the connect modal: {e}");
            Vec::new()
        });
        let recent = crate::workspace::preferences::load().last_server;
        if let Some(first) = initial_server(&servers, recent.as_deref()) {
            let name = first.name.clone();
            let profiles = with_server_if_unchanged(first, |current| {
                smudgy_core::models::profile::list_profiles(&current.name)
            });
            state.selected_server = Some(name.clone());
            state
                .last_sessions
                .insert(name.clone(), load_last_session_profiles(first));
            match profiles {
                Ok(ServerCas::Applied(mut profiles)) => {
                    profiles.sort_by(|left, right| left.name.cmp(&right.name));
                    state.profiles.insert(name, profiles);
                }
                Ok(ServerCas::StateChanged) => warn!(
                    "Server '{name}' changed while the connect modal was opening; its profiles were not cached"
                ),
                Err(error) => warn!("Failed to load profiles for '{name}': {error}"),
            }
        }
        // The observed sidecars are the same class of small local read; a
        // server without one simply has no entry (and no metadata band).
        state.observed = servers
            .iter()
            .filter_map(|server| {
                match with_server_if_unchanged(server, |current| {
                    Ok(smudgy_core::models::observed::load_observed(&current.name))
                }) {
                    Ok(ServerCas::Applied(Some(observed))) => Some((server.name.clone(), observed)),
                    _ => None,
                }
            })
            .collect();
        // Cached icons likewise (small PNGs of the icon pipeline's own
        // re-encode); fetches for changed `ICON` values are spawned by
        // `icon_refresh_task` once the modal is up.
        state.icons = servers
            .iter()
            .filter_map(|server| {
                match with_server_if_unchanged(server, |current| {
                    Ok(crate::images::server_icon::load_cached_icon(&current.name))
                }) {
                    Ok(ServerCas::Applied(Some(handle))) => Some((server.name.clone(), handle)),
                    _ => None,
                }
            })
            .collect();
        state.servers = servers;
        state
    }

    /// Fetch tasks for every server whose observed `ICON` value the cache
    /// wasn't built from — run when the modal opens, and again after a
    /// link-trust grant (which may have just unlocked a held icon). The
    /// pre-checks here only avoid spawning tasks that would refuse; the fetch
    /// re-applies the full policy itself.
    pub(crate) fn icon_refresh_task(&self) -> Task<Message> {
        use crate::images::server_icon;
        let mut tasks = Vec::new();
        for server in &self.servers {
            let Some(icon_value) = self
                .observed
                .get(&server.name)
                .and_then(|observed| match observed.mssp.get("ICON") {
                    Some(ObservedValue::Text(value)) => Some(value.trim().to_string()),
                    _ => None,
                })
                .filter(|value| !value.is_empty())
            else {
                continue;
            };
            if !server_icon::needs_refetch(&server.name, &icon_value) {
                continue;
            }
            if !matches!(
                server_icon::icon_url_policy(&icon_value, &server.config.host, &server.config),
                server_icon::IconUrlPolicy::AutoFetch(_)
            ) {
                continue;
            }
            tasks.push(Task::perform(
                server_icon::fetch_and_cache(server.clone(), icon_value),
                |(server, handle)| Message::ServerIconFetched(server, handle),
            ));
        }
        Task::batch(tasks)
    }

    /// Re-read one server's observed sidecar — the daemon calls this on
    /// `SessionEvent::ObservedServerChanged`, so an open modal's metadata
    /// band tracks a live session's writes without file watching.
    pub fn refresh_observed(&mut self, server: &str) {
        let Some(expected) = self
            .servers
            .iter()
            .find(|candidate| candidate.name == server)
        else {
            self.observed.remove(server);
            return;
        };
        match with_server_if_unchanged(expected, |current| {
            Ok(smudgy_core::models::observed::load_observed(&current.name))
        }) {
            Ok(ServerCas::Applied(Some(observed))) => {
                self.observed.insert(server.to_string(), observed);
            }
            Ok(ServerCas::Applied(None) | ServerCas::StateChanged) | Err(_) => {
                self.observed.remove(server);
            }
        }
    }
}

/// Probe a server's last-session snapshot for the restore affordance: the
/// stored profile names in slot order, or `None` when no usable snapshot
/// exists. A small local read, done synchronously like the rest of the
/// modal's disk reads.
fn load_last_session_profiles(server: &Server) -> Option<Vec<String>> {
    match with_server_if_unchanged(server, |current| {
        let Some(template) = crate::workspace::last_session::read(&current.name) else {
            return Ok(None);
        };
        let profiles = crate::workspace::last_session::profile_names(&template);
        Ok((!profiles.is_empty()).then_some(profiles))
    }) {
        Ok(ServerCas::Applied(profiles)) => profiles,
        Ok(ServerCas::StateChanged) | Err(_) => None,
    }
}

impl Default for State {
    fn default() -> Self {
        State {
            servers: Vec::new(),
            observed: HashMap::new(),
            link_confirm: None,
            icons: HashMap::new(),
            profiles: HashMap::new(),
            last_sessions: HashMap::new(),
            selected_server: None,
            is_loading_servers: false, // Load triggered by update
            is_loading_profiles: None,
            profile_load_sequence: 0,
            pending_profile_load: None,
            server_action: None,
            server_form_data: ServerConfigFormData::default(),
            server_crud_error: None,
            server_form_revision: 0,
            server_operation_sequence: 0,
            pending_server_operation: None,
            profile_action: None,
            profile_form_data: ProfileConfigFormData::default(),
            profile_form_send_on_connect_content: text_editor::Content::with_text(""),
            profile_crud_error: None,
            session_open_error: None,
            profile_form_password: String::new(),
            profile_form_password_stored: false,
            profile_form_password_editing: false,
            profile_form_revision: 0,
            profile_operation_sequence: 0,
            pending_profile_operation: None,
            image_cache_usage: None,
            image_cache_usage_request: 0,
        }
    }
}

impl State {
    pub fn set_session_open_error(&mut self, error: impl Into<String>) {
        self.session_open_error = Some(error.into());
    }

    #[cfg(all(test, feature = "web-audio-cpal"))]
    pub(crate) fn session_open_error_for_test(&self) -> Option<&str> {
        self.session_open_error.as_deref()
    }
}

// --- Profile-list and image-cache helpers (blocking I/O, run via Task::perform) ---

fn start_profile_load(state: &mut State, server: Server) -> Task<Message> {
    state.profile_load_sequence = state.profile_load_sequence.wrapping_add(1);
    if state.profile_load_sequence == 0 {
        state.profile_load_sequence = 1;
    }
    let request = state.profile_load_sequence;
    state.is_loading_profiles = Some(server.name.clone());
    state.pending_profile_load = Some((request, server.clone()));
    Task::perform(load_profiles_async(server.clone()), move |result| {
        Message::ProfilesLoaded(request, server.clone(), result)
    })
}

fn cancel_profile_load(state: &mut State) {
    state.pending_profile_load = None;
    state.is_loading_profiles = None;
}

fn cancel_profile_load_for(state: &mut State, server: &Server) {
    if state
        .pending_profile_load
        .as_ref()
        .is_some_and(|(_, pending)| pending == server)
    {
        cancel_profile_load(state);
    }
}

fn next_image_cache_request(state: &mut State) -> u64 {
    state.image_cache_usage_request = state.image_cache_usage_request.wrapping_add(1);
    if state.image_cache_usage_request == 0 {
        state.image_cache_usage_request = 1;
    }
    state.image_cache_usage_request
}

async fn load_image_cache_usage(server: Server) -> Result<ServerCas<u64>, String> {
    with_server_if_unchanged(&server, |current| {
        Ok(crate::images::server_image_cache_usage_bytes(&current.name))
    })
    .map_err(|error| error.to_string())
}

async fn clear_image_cache_async(server: Server) -> Result<ServerCas<()>, String> {
    with_server_if_unchanged(&server, |current| {
        crate::images::clear_server_image_cache(&current.name);
        Ok(())
    })
    .map_err(|error| error.to_string())
}

/// Open a URL in the system browser, detached; a failure is logged, never
/// fatal. (The session view keeps its own copy — both are one-line wrappers
/// around the same crate call.)
fn open_url_in_browser(url: &str) {
    if let Err(e) = open::that_detached(url) {
        log::error!("Failed to open {url} in the browser: {e}");
    }
}

// --- Auto-login password helpers ---

pub(super) fn advance_server_form_revision(state: &mut State) {
    state.server_form_revision = state.server_form_revision.wrapping_add(1);
}

fn clear_server_form(state: &mut State) {
    state.server_action = None;
    state.server_form_data = ServerConfigFormData::default();
    state.server_crud_error = None;
    advance_server_form_revision(state);
}

pub(super) fn next_server_operation_id(state: &mut State) -> u64 {
    state.server_operation_sequence = state.server_operation_sequence.wrapping_add(1);
    if state.server_operation_sequence == 0 {
        state.server_operation_sequence = 1;
    }
    state.server_operation_sequence
}

#[derive(Debug)]
enum ServerCacheApplication {
    Created(Server),
    Updated(Server),
    Deleted { expected: Server, removed: bool },
    Ignored,
}

fn apply_server_operation_to_cache(
    state: &mut State,
    pending: &ServerOperationEnvelope,
    applied: &AppliedServerOperation,
) -> ServerCacheApplication {
    match (&pending.action, applied) {
        (
            ServerOperationAction::Create { target_name, .. },
            AppliedServerOperation::Created(created),
        ) if created.name == *target_name => {
            if let Some(current) = state
                .servers
                .iter()
                .find(|server| server.name == created.name)
            {
                if current == created {
                    return ServerCacheApplication::Created(created.clone());
                }
                // A same-name server from a different lifetime, or a newer snapshot of this
                // lifetime, wins over this delayed completion.
                return ServerCacheApplication::Ignored;
            }
            state.servers.push(created.clone());
            state
                .servers
                .sort_by(|left, right| left.name.cmp(&right.name));
            ServerCacheApplication::Created(created.clone())
        }
        (
            ServerOperationAction::Update { expected, .. },
            AppliedServerOperation::Updated(updated),
        ) if updated.name == expected.name => {
            let Some(current) = state
                .servers
                .iter_mut()
                .find(|server| server.name == expected.name)
            else {
                return ServerCacheApplication::Ignored;
            };
            if current == expected {
                *current = updated.clone();
                ServerCacheApplication::Updated(updated.clone())
            } else if current == updated {
                // A list refresh may have observed the successful write before its task replied.
                ServerCacheApplication::Updated(updated.clone())
            } else {
                ServerCacheApplication::Ignored
            }
        }
        (ServerOperationAction::Delete { expected }, AppliedServerOperation::Deleted) => {
            let removed = state
                .servers
                .iter()
                .position(|server| server == expected)
                .is_some_and(|index| {
                    state.servers.remove(index);
                    true
                });
            ServerCacheApplication::Deleted {
                expected: expected.clone(),
                removed,
            }
        }
        _ => ServerCacheApplication::Ignored,
    }
}

fn handle_server_operation_completion(
    state: &mut State,
    completion: ServerOperationCompletion,
) -> Task<Message> {
    let Some(pending) = state.pending_server_operation.as_ref() else {
        warn!("Ignoring a server-operation completion because no operation is pending");
        return Task::none();
    };
    if pending.key() != completion.key {
        warn!(
            "Ignoring stale server-operation completion {:?}; pending operation is {:?}",
            completion.key,
            pending.key()
        );
        return Task::none();
    }

    let pending = state
        .pending_server_operation
        .take()
        .expect("the matching pending operation was just observed");
    let form_is_unchanged = state.server_form_revision == pending.form_revision;
    let result = match completion.result {
        Ok(applied) => apply_server_operation_to_cache(state, &pending, &applied),
        Err(error) => {
            let error = error.localized(&pending.action);
            warn!("Server operation {:?} failed: {error}", pending.key());
            if form_is_unchanged {
                state.server_crud_error = Some(match pending.action {
                    ServerOperationAction::Create { .. } => {
                        t!("server-error-create", "error" => &error)
                    }
                    ServerOperationAction::Update { .. } => {
                        t!("server-error-update", "error" => &error)
                    }
                    ServerOperationAction::Delete { .. } => {
                        t!("server-error-delete", "error" => &error)
                    }
                });
            }
            return Task::none();
        }
    };

    match result {
        ServerCacheApplication::Created(created) if form_is_unchanged => {
            clear_server_form(state);
            let name = created.name.clone();
            state.selected_server = Some(name.clone());
            let load_task = start_profile_load(state, created.clone());

            state.profile_action = Some(ProfileCrudAction::Create { server: created });
            state.profile_form_data = ProfileConfigFormData::default();
            state.profile_form_send_on_connect_content = text_editor::Content::new();
            state.profile_crud_error = None;
            state.profile_form_password = String::new();
            state.profile_form_password_stored = false;
            state.profile_form_password_editing = true;
            advance_profile_form_revision(state);
            Task::batch([load_task, operation::focus(profile_name_input_id())])
        }
        ServerCacheApplication::Updated(updated) if form_is_unchanged => {
            clear_server_form(state);
            state.selected_server = Some(updated.name);
            Task::none()
        }
        ServerCacheApplication::Deleted { expected, removed } => {
            if form_is_unchanged {
                clear_server_form(state);
            }
            if !removed {
                // The list already moved on (most importantly, to a same-name replacement).
                return Task::none();
            }

            let name = expected.name.clone();
            cancel_profile_load_for(state, &expected);
            state.profiles.remove(&name);
            state.last_sessions.remove(&name);
            state.observed.remove(&name);
            state.icons.remove(&name);
            if state
                .link_confirm
                .as_ref()
                .is_some_and(|pending| pending.server == expected)
            {
                state.link_confirm = None;
            }
            if state
                .pending_profile_operation
                .as_ref()
                .is_some_and(|profile_operation| profile_operation.server == expected)
            {
                state.pending_profile_operation = None;
            }
            if state
                .profile_action
                .as_ref()
                .is_some_and(|action| match action {
                    ProfileCrudAction::Create { server }
                    | ProfileCrudAction::Edit { server, .. }
                    | ProfileCrudAction::ConfirmDelete { server, .. } => server == &expected,
                })
            {
                clear_profile_form(state);
            }

            if state.selected_server.as_ref() != Some(&name) {
                return Task::none();
            }
            if let Some(first_server) = state.servers.first() {
                let fallback_server = first_server.clone();
                let fallback = fallback_server.name.clone();
                state.selected_server = Some(fallback.clone());
                start_profile_load(state, fallback_server)
            } else {
                state.selected_server = None;
                cancel_profile_load(state);
                Task::none()
            }
        }
        ServerCacheApplication::Ignored => {
            warn!(
                "Server-operation completion {:?} did not match its immutable request or current cache",
                pending.key()
            );
            Task::none()
        }
        ServerCacheApplication::Created(_) | ServerCacheApplication::Updated(_) => Task::none(),
    }
}

/// Clears the transient password-form state (buffer + flags). Called whenever a
/// profile form is closed or submitted.
fn reset_password_form(state: &mut State) {
    state.profile_form_password = String::new();
    state.profile_form_password_stored = false;
    state.profile_form_password_editing = false;
}

pub(super) fn advance_profile_form_revision(state: &mut State) {
    state.profile_form_revision = state.profile_form_revision.wrapping_add(1);
}

fn clear_profile_form(state: &mut State) {
    state.profile_action = None;
    state.profile_form_data = ProfileConfigFormData::default();
    state.profile_form_send_on_connect_content = text_editor::Content::new();
    state.profile_crud_error = None;
    reset_password_form(state);
    advance_profile_form_revision(state);
}

pub(super) fn next_profile_operation_id(state: &mut State) -> u64 {
    state.profile_operation_sequence = state.profile_operation_sequence.wrapping_add(1);
    if state.profile_operation_sequence == 0 {
        state.profile_operation_sequence = 1;
    }
    state.profile_operation_sequence
}

fn handle_profile_operation_completion(state: &mut State, completion: ProfileOperationCompletion) {
    let Some(pending) = state.pending_profile_operation.as_ref() else {
        warn!("Ignoring a profile-operation completion because no operation is pending");
        return;
    };
    if pending.key() != completion.key {
        warn!(
            "Ignoring stale profile-operation completion {:?}; pending operation is {:?}",
            completion.key,
            pending.key()
        );
        return;
    }

    let pending = state
        .pending_profile_operation
        .take()
        .expect("the matching pending operation was just observed");
    let form_is_unchanged = state.profile_form_revision == pending.form_revision;
    match completion.result {
        Ok(applied) => {
            // A profile mutation is newer than any list read started for the same exact server.
            // Cancel that read before updating the cache so its delayed completion cannot replace
            // this result with an older snapshot.
            cancel_profile_load_for(state, &pending.server);
            if !apply_profile_operation_to_cache(state, &pending, &applied) {
                warn!(
                    "Profile-operation completion {:?} did not match its immutable request",
                    pending.key()
                );
            }
            let saved_with_password_warning = match &applied {
                AppliedProfileOperation::Created(profile, Some(warning))
                | AppliedProfileOperation::Updated(profile, Some(warning)) => {
                    let message = match warning {
                        ProfilePasswordWarning::StateChanged => t!(
                            "profile-warning-password-state-changed",
                            "server" => &pending.server.name,
                            "profile" => &profile.name
                        ),
                        ProfilePasswordWarning::Failed(error) => t!(
                            "profile-warning-password-failed",
                            "server" => &pending.server.name,
                            "profile" => &profile.name,
                            "error" => error
                        ),
                    };
                    if form_is_unchanged {
                        // The profile configuration is committed. Keep the form open as an edit
                        // of that exact result, retain any typed password, and let Save retry only
                        // the password side effect plus an idempotent config write.
                        state.profile_action = Some(ProfileCrudAction::Edit {
                            server: pending.server.clone(),
                            expected: profile.clone(),
                        });
                        state.profile_form_data = ProfileConfigFormData {
                            name: profile.name.clone(),
                            description: profile.config.caption.clone(),
                        };
                        state.profile_form_send_on_connect_content =
                            text_editor::Content::with_text(&profile.config.send_on_connect);
                        advance_profile_form_revision(state);
                    }
                    // Always surface a partial commit, even if the user opened a different form
                    // while the operation was pending. The message names the affected profile.
                    state.profile_crud_error = Some(message);
                    true
                }
                _ => false,
            };
            if form_is_unchanged && !saved_with_password_warning {
                clear_profile_form(state);
            }
        }
        Err(error) => {
            let error = error.localized(&pending.action);
            warn!("Profile operation {:?} failed: {error}", pending.key());
            if form_is_unchanged {
                state.profile_crud_error = Some(match pending.action {
                    ProfileOperationAction::Create { .. } => {
                        t!("profile-error-create", "error" => &error)
                    }
                    ProfileOperationAction::Update { .. } => {
                        t!("profile-error-update", "error" => &error)
                    }
                    ProfileOperationAction::Delete { .. } => {
                        t!("profile-error-delete", "error" => &error)
                    }
                });
            }
        }
    }
}

fn apply_profile_operation_to_cache(
    state: &mut State,
    pending: &ProfileOperationEnvelope,
    applied: &AppliedProfileOperation,
) -> bool {
    let profiles = state
        .profiles
        .entry(pending.server.name.clone())
        .or_default();
    match (&pending.action, applied) {
        (
            ProfileOperationAction::Create { target_name, .. },
            AppliedProfileOperation::Created(created, _),
        ) if created.name == *target_name => {
            if let Some(current) = profiles
                .iter_mut()
                .find(|profile| profile.name == created.name)
            {
                // The cached entry is either this creation or a newer snapshot of the same
                // profile; either way it wins over this delayed completion.
                let _ = current;
                return true;
            }
            profiles.push(created.clone());
            profiles.sort_by(|left, right| left.name.cmp(&right.name));
            true
        }
        (
            ProfileOperationAction::Update {
                expected,
                target_name,
                ..
            },
            AppliedProfileOperation::Updated(updated, _),
        ) if updated.name == *target_name && expected.name == *target_name => {
            if let Some(current) = profiles
                .iter_mut()
                .find(|profile| profile.name == updated.name)
                && *current == *expected
            {
                *current = updated.clone();
                profiles.sort_by(|left, right| left.name.cmp(&right.name));
            }
            true
        }
        (
            ProfileOperationAction::Delete {
                expected,
                target_name,
            },
            AppliedProfileOperation::Deleted,
        ) if expected.name == *target_name => {
            profiles.retain(|profile| profile != expected);
            true
        }
        _ => false,
    }
}

// --- Update Logic ---

/// Handles messages specific to the Connect Modal logic.
pub fn update(state: &mut State, message: Message) -> (Task<Message>, Option<Event>) {
    let mut task = Task::none();
    let mut event = None;

    // Clear server CRUD error on most actions unless explicitly set
    if !matches!(
        message,
        Message::SubmitServerForm | Message::ServerOperationFinished(_)
    ) {
        state.server_crud_error = None;
    }

    match message {
        Message::ProfilesLoaded(request, server, result) => {
            if state.pending_profile_load.as_ref() != Some(&(request, server.clone())) {
                warn!(
                    "Ignoring stale profile-list completion for '{}' (request {request})",
                    server.name
                );
                return (task, event);
            }
            cancel_profile_load(state);
            if !state.servers.iter().any(|current| current == &server) {
                warn!(
                    "Ignoring profile-list completion because server '{}' changed in the modal",
                    server.name
                );
                return (task, event);
            }
            match result {
                Ok(ServerCas::Applied(mut profiles)) => {
                    profiles.sort_by(|left, right| left.name.cmp(&right.name));
                    state.profiles.insert(server.name, profiles);
                }
                Ok(ServerCas::StateChanged) => {
                    warn!(
                        "Profile list for '{}' was not applied because the server changed",
                        server.name
                    );
                }
                Err(error) => {
                    let err_msg = t!(
                        "profiles-error-load",
                        "server" => &server.name,
                        "error" => error
                    );
                    warn!("{err_msg}");
                    state.profile_crud_error = Some(err_msg);
                }
            }
        }
        Message::SelectServer(server_name) => {
            if state.selected_server.as_ref() != Some(&server_name) {
                clear_server_form(state);
                clear_profile_form(state);
                let server_name_clone = server_name.clone();
                state.selected_server = Some(server_name_clone.clone());
                if let Some(server) = state
                    .servers
                    .iter()
                    .find(|server| server.name == server_name_clone)
                    .cloned()
                {
                    state
                        .last_sessions
                        .entry(server_name_clone.clone())
                        .or_insert_with(|| load_last_session_profiles(&server));
                    if !state.profiles.contains_key(&server_name_clone) {
                        task = start_profile_load(state, server);
                    }
                }
            }
        }
        Message::CloseRequested => {
            event = Some(Event::CloseModalRequested);
        }
        Message::ConnectProfile(server_name, profile_name) => {
            // Cancel any ongoing server CRUD action if user connects
            clear_server_form(state);
            event = Some(Event::Connect(server_name, profile_name));
        }
        Message::OpenOfflineProfile(server_name, profile_name) => {
            // Same housekeeping as `ConnectProfile`; the parent opens the session
            // without establishing a connection.
            clear_server_form(state);
            event = Some(Event::OpenOffline(server_name, profile_name));
        }
        Message::RestoreLastSession(server_name) => {
            // Same housekeeping as `ConnectProfile`; the parent drives the
            // full user-restore flow from the server's stored snapshot.
            clear_server_form(state);
            event = Some(Event::RestoreLastSession(server_name));
        }
        Message::RequestCreateServer => {
            state.server_action = Some(ServerCrudAction::Create);
            state.server_form_data = ServerConfigFormData::default(); // Clear form
            state.server_crud_error = None;
            advance_server_form_revision(state);
            state.selected_server = None; // De-select server when opening create form
            cancel_profile_load(state);
            // `+ New Server` is persistent, so it can be pressed while a profile
            // form is open; drop that form so it doesn't resurface on cancel.
            clear_profile_form(state);
            task = operation::focus(server_name_input_id());
        }
        Message::RequestEditServer(server_name) => {
            if let Some(server_to_edit) = state
                .servers
                .iter()
                .find(|s| s.name == server_name)
                .cloned()
            {
                clear_profile_form(state);
                state.server_action = Some(ServerCrudAction::Edit(server_to_edit.clone()));
                state.server_form_data = ServerConfigFormData {
                    name: server_to_edit.name.clone(), // Pre-fill name (though not directly editable usually)
                    host: server_to_edit.config.host.clone(),
                    port: server_to_edit.config.port.to_string(),
                    encoding: server_to_edit
                        .config
                        .encoding
                        .clone()
                        .unwrap_or_else(|| server::DEFAULT_ENCODING_CHOICE.to_string()),
                    compression: server_to_edit.config.compression,
                    mccp4_compression: server_to_edit.config.accepts_mccp4_compression(),
                    tls: server_to_edit.config.tls,
                    tls_verify: server_to_edit.config.tls_verify,
                };
                state.server_crud_error = None;
                advance_server_form_revision(state);
                cancel_profile_load(state);
                // The usage figure loads off-thread (it stats cache files).
                state.image_cache_usage = None;
                let request = next_image_cache_request(state);
                let usage_server = server_to_edit.clone();
                let usage_task = Task::perform(
                    load_image_cache_usage(usage_server.clone()),
                    move |result| {
                        Message::ImageCacheUsageLoaded(request, usage_server.clone(), result)
                    },
                );
                state.selected_server = Some(server_name); // Ensure server remains selected
                // Name isn't editable in edit mode; focus the first editable field.
                task = Task::batch([operation::focus(server_host_input_id()), usage_task]);
            } else {
                warn!("Error: Requested to edit non-existent server '{server_name}'");
            }
        }
        Message::RequestConfirmDeleteServer(server_name) => {
            if state.pending_server_operation.is_some() {
                warn!("Ignoring server delete request while an operation is pending");
            } else if let Some(expected) = state
                .servers
                .iter()
                .find(|server| server.name == server_name)
                .cloned()
            {
                state.server_action = Some(ServerCrudAction::ConfirmDelete(expected));
                state.server_crud_error = None;
                advance_server_form_revision(state);
                clear_profile_form(state);
            } else {
                warn!("Ignoring a delete request for missing server '{server_name}'");
            }
        }
        Message::ConfirmDeleteServer => {
            if state.pending_server_operation.is_some() {
                warn!("Ignoring server delete because an operation is already pending");
            } else if let Some(ServerCrudAction::ConfirmDelete(expected)) =
                state.server_action.clone()
            {
                state.server_crud_error = None;
                let envelope = ServerOperationEnvelope {
                    id: next_server_operation_id(state),
                    action: ServerOperationAction::Delete { expected },
                    form_revision: state.server_form_revision,
                };
                state.pending_server_operation = Some(envelope.clone());
                task = Task::perform(
                    execute_server_operation(envelope),
                    Message::ServerOperationFinished,
                );
            } else {
                warn!("Ignoring server delete without an exact confirmation snapshot");
            }
        }
        Message::ImageCacheUsageLoaded(request, server, result) => {
            // Stale replies are dropped: an older request's answer (slow scan racing a
            // post-clear re-read), or one for a form the user has moved on from.
            if request == state.image_cache_usage_request
                && matches!(&state.server_action, Some(ServerCrudAction::Edit(expected)) if expected == &server)
            {
                match result {
                    Ok(ServerCas::Applied(bytes)) => state.image_cache_usage = Some(bytes),
                    Ok(ServerCas::StateChanged) => warn!(
                        "Image-cache usage for '{}' was discarded because the server changed",
                        server.name
                    ),
                    Err(error) => warn!(
                        "Failed to read image-cache usage for '{}': {error}",
                        server.name
                    ),
                }
            }
        }
        Message::RequestClearImageCache(server) => {
            if matches!(&state.server_action, Some(ServerCrudAction::Edit(expected)) if expected == &server)
            {
                state.image_cache_usage = None; // shows the pending state
                let request = next_image_cache_request(state);
                task = Task::perform(clear_image_cache_async(server.clone()), move |result| {
                    Message::ImageCacheCleared(request, server.clone(), result)
                });
            }
        }
        Message::ImageCacheCleared(request, server, result) => {
            if request != state.image_cache_usage_request
                || !matches!(&state.server_action, Some(ServerCrudAction::Edit(expected)) if expected == &server)
            {
                return (task, event);
            }
            match result {
                Ok(ServerCas::Applied(())) => {
                    // Re-read rather than assume zero — a concurrent session may already be
                    // caching again.
                    let usage_request = next_image_cache_request(state);
                    task = Task::perform(load_image_cache_usage(server.clone()), move |result| {
                        Message::ImageCacheUsageLoaded(usage_request, server.clone(), result)
                    });
                }
                Ok(ServerCas::StateChanged) => {
                    state.server_crud_error = Some(t!("server-error-cache-changed"));
                }
                Err(error) => {
                    state.server_crud_error =
                        Some(t!("server-error-cache-clear", "error" => error));
                }
            }
        }
        Message::UpdateServerFormField(field, value) => {
            // Only update if in Create or Edit mode
            if matches!(
                state.server_action,
                Some(ServerCrudAction::Create) | Some(ServerCrudAction::Edit(_))
            ) {
                match field {
                    ServerFormField::Name => state.server_form_data.name = value,
                    ServerFormField::Host => state.server_form_data.host = value,
                    ServerFormField::Port => state.server_form_data.port = value,
                    ServerFormField::Encoding => state.server_form_data.encoding = value,
                }
                state.server_crud_error = None; // Clear error when user types
                advance_server_form_revision(state);
            }
        }
        Message::ToggleServerCompression(value) => {
            if matches!(
                state.server_action,
                Some(ServerCrudAction::Create) | Some(ServerCrudAction::Edit(_))
            ) {
                state.server_form_data.compression = value;
                advance_server_form_revision(state);
            }
        }
        Message::ToggleServerMccp4Compression(value) => {
            if matches!(
                state.server_action,
                Some(ServerCrudAction::Create) | Some(ServerCrudAction::Edit(_))
            ) {
                state.server_form_data.mccp4_compression = value;
                advance_server_form_revision(state);
            }
        }
        Message::ToggleServerTls(value) => {
            if matches!(
                state.server_action,
                Some(ServerCrudAction::Create) | Some(ServerCrudAction::Edit(_))
            ) {
                state.server_form_data.tls = value;
                advance_server_form_revision(state);
            }
        }
        Message::ToggleServerTlsVerify(value) => {
            if matches!(
                state.server_action,
                Some(ServerCrudAction::Create) | Some(ServerCrudAction::Edit(_))
            ) {
                state.server_form_data.tls_verify = value;
                advance_server_form_revision(state);
            }
        }
        Message::SubmitServerForm => {
            task = handle_submit_server_form(state);
        }
        Message::CancelServerForm => {
            // Clear action, form data, and error regardless of previous state
            clear_server_form(state);
            // If a server was selected before opening the form (e.g., for Edit or ConfirmDelete),
            // we don't explicitly re-select it here. The user can click it again in the list.
            // This keeps the cancellation logic simple.
        }
        Message::ServerOperationFinished(completion) => {
            task = handle_server_operation_completion(state, completion);
        }
        Message::RequestCreateProfile => {
            if let Some(server) = state
                .selected_server
                .as_ref()
                .and_then(|selected| state.servers.iter().find(|server| server.name == *selected))
                .cloned()
            {
                state.profile_action = Some(ProfileCrudAction::Create { server });
                state.profile_form_data = ProfileConfigFormData::default();
                state.profile_form_send_on_connect_content = text_editor::Content::new();
                state.profile_crud_error = None;
                state.profile_form_password = String::new();
                state.profile_form_password_stored = false;
                state.profile_form_password_editing = true;
                advance_profile_form_revision(state);
                clear_server_form(state);
                task = operation::focus(profile_name_input_id());
            } else {
                warn!("Error: Cannot create profile, no server selected.");
            }
        }
        Message::RequestEditProfile(profile_name) => {
            // Ensure a server is selected first
            if let Some(server_name) = state.selected_server.clone() {
                let server = state
                    .servers
                    .iter()
                    .find(|server| server.name == server_name)
                    .cloned();
                // Find the profile within the selected server's profile list
                if let (Some(server), Some(profile_vec)) =
                    (server, state.profiles.get(&server_name))
                {
                    if let Some(profile_to_edit) =
                        profile_vec.iter().find(|p| p.name == profile_name).cloned()
                    {
                        state.profile_action = Some(ProfileCrudAction::Edit {
                            server,
                            expected: profile_to_edit.clone(),
                        });
                        state.profile_form_data = ProfileConfigFormData {
                            name: profile_to_edit.name.clone(), // Pre-fill name for context (won't be editable in form)
                            description: profile_to_edit.config.caption.clone(),
                        };
                        state.profile_form_send_on_connect_content =
                            text_editor::Content::with_text(
                                profile_to_edit.config.send_on_connect.as_str(),
                            );
                        state.profile_crud_error = None;
                        clear_server_form(state);
                        // Reflect whether a password is already stored for this
                        // profile: show the "saved" chip if so, the input if not.
                        let stored = has_profile_password(&server_name, &profile_name);
                        state.profile_form_password = String::new();
                        state.profile_form_password_stored = stored;
                        state.profile_form_password_editing = !stored;
                        advance_profile_form_revision(state);
                        // Name isn't editable in edit mode; focus the first editable field.
                        task = operation::focus(profile_description_input_id());
                    } else {
                        warn!(
                            "Error: Requested to edit non-existent profile '{profile_name}' in server '{server_name}'"
                        );
                    }
                } else {
                    warn!(
                        "Error: Profile list not available for server '{server_name}' when trying to edit profile '{profile_name}'"
                    );
                }
            } else {
                warn!("Error: Cannot edit profile, no server selected.");
            }
        }
        Message::RequestConfirmDeleteProfile => {
            if state.pending_profile_operation.is_some() {
                warn!("Ignoring profile delete request while an operation is pending");
            } else if let Some(ProfileCrudAction::Edit { server, expected }) =
                state.profile_action.clone()
            {
                state.profile_action = Some(ProfileCrudAction::ConfirmDelete { server, expected });
                state.profile_crud_error = None;
                advance_profile_form_revision(state);
            }
        }
        Message::ConfirmDeleteProfile => {
            task = handle_delete_profile(state);
        }
        Message::UpdateProfileFormField(field, value) => {
            match field {
                ProfileFormField::Name => state.profile_form_data.name = value,
                ProfileFormField::Description => state.profile_form_data.description = value,
            }
            state.profile_crud_error = None;
            advance_profile_form_revision(state);
        }
        Message::UpdateProfileFormSendOnConnect(action) => {
            state.profile_form_send_on_connect_content.perform(action);
            state.profile_crud_error = None;
            advance_profile_form_revision(state);
        }
        Message::SubmitProfileForm => {
            task = handle_submit_profile_form(state);
        }
        Message::CancelProfileForm => {
            clear_profile_form(state);
        }
        Message::UpdateProfileFormPassword(value) => {
            state.profile_form_password = value.0;
            state.profile_crud_error = None;
            advance_profile_form_revision(state);
        }
        Message::RequestChangeProfilePassword => {
            // Reveal the input to enter a replacement password.
            state.profile_form_password = String::new();
            state.profile_form_password_editing = true;
            advance_profile_form_revision(state);
            task = operation::focus(profile_password_input_id());
        }
        Message::ClearProfilePassword => {
            // Drop the stored password now (this is the only destructive action and
            // the user asked for it explicitly), then show the empty input again.
            if state.pending_profile_operation.is_none()
                && let Some(ProfileCrudAction::Edit { server, expected }) = &state.profile_action
            {
                match clear_profile_password_if_unchanged(&server.name, expected) {
                    Ok(ProfileCas::Applied(())) => {
                        state.profile_form_password = String::new();
                        state.profile_form_password_stored = false;
                        state.profile_form_password_editing = true;
                        advance_profile_form_revision(state);
                    }
                    Ok(ProfileCas::StateChanged) => {
                        state.profile_crud_error = Some(t!("profile-error-password-state-changed"));
                    }
                    Err(error) => {
                        state.profile_crud_error = Some(t!(
                            "profile-error-password-clear",
                            "error" => error.to_string()
                        ));
                    }
                }
            }
        }
        Message::ProfileOperationFinished(completion) => {
            handle_profile_operation_completion(state, completion);
        }
        Message::ServerIconFetched(server, handle) => {
            // `None` (refusal or failure) keeps whatever was cached before.
            if let Some(handle) = handle
                && state.servers.iter().any(|current| current == &server)
            {
                state.icons.insert(server.name, handle);
            }
        }
        Message::OpenObservedLink(server, url) => {
            // The same per-server gate a server OSC 8 link passes: a granted
            // host (or a blanket grant) opens directly, anything else is held
            // for the user's verdict. The URL is server-supplied input.
            if !state.servers.iter().any(|current| current == &server) {
                warn!(
                    "Ignoring an observed-link action because server '{}' changed",
                    server.name
                );
                return (task, event);
            }
            let host = link_url_host(&url);
            let allowed = server.config.allows_server_link(host.as_deref());
            if allowed {
                open_url_in_browser(&url);
            } else {
                state.link_confirm = Some(ObservedLinkConfirm {
                    display: observed::safe_url_display(&url),
                    server,
                    url,
                    host,
                    grant_host: false,
                    grant_server: false,
                });
            }
        }
        Message::ObservedLinkGrantHost(value) => {
            if let Some(pending) = state.link_confirm.as_mut() {
                pending.grant_host = value;
            }
        }
        Message::ObservedLinkGrantServer(value) => {
            if let Some(pending) = state.link_confirm.as_mut() {
                pending.grant_server = value;
            }
        }
        Message::ObservedLinkCancel => {
            state.link_confirm = None;
        }
        Message::ObservedLinkProceed => {
            if let Some(pending) = state.link_confirm.take() {
                if pending.grant_host || pending.grant_server {
                    // Persist by re-reading the on-disk config and applying
                    // only this grant, so a concurrent session's grant isn't
                    // clobbered by a stale whole-config snapshot; fall back
                    // to the modal's copy if the load fails. The in-memory
                    // copy adopts the result so the gate reflects it now.
                    match load_server(&pending.server.name) {
                        Ok(current) => {
                            let mut config = current.config.clone();
                            if pending.grant_server {
                                config.trust_all_links = true;
                            }
                            if pending.grant_host
                                && let Some(host) = &pending.host
                            {
                                config.grant_link_host(host);
                            }
                            match update_server_if_unchanged(&current, config) {
                                Ok(ServerCas::Applied(updated)) => {
                                    if let Some(cached) = state
                                        .servers
                                        .iter_mut()
                                        .find(|server| server.name == updated.name)
                                    {
                                        *cached = updated;
                                    }
                                    // A fresh grant may have unlocked an icon held at the
                                    // same trust gate (icons and links share the store).
                                    task = state.icon_refresh_task();
                                }
                                Ok(ServerCas::StateChanged) => warn!(
                                    "Link-trust grant was not saved because server '{}' changed; the user can retry it",
                                    pending.server.name
                                ),
                                Err(error) => warn!(
                                    "Failed to persist link-trust grants for '{}': {error}",
                                    pending.server.name
                                ),
                            }
                        }
                        Err(error) => warn!(
                            "Failed to load server '{}' before saving link trust: {error}",
                            pending.server.name
                        ),
                    }
                }
                open_url_in_browser(&pending.url);
            }
        }
    }
    (task, event)
}

// --- View Logic ---

/// Renders the right-pane content when no server is selected and no form is open.
/// On first run (no servers) this is the guided welcome; otherwise it is a
/// brief prompt to pick a server from the rail (rarely seen, since loading a
/// non-empty server list auto-selects the first server).
fn view_placeholder(state: &State) -> Element<'_, Message> {
    if state.is_loading_servers {
        return column![text(t!("servers-loading")).style(builtins::text::muted)].into();
    }

    if state.servers.is_empty() {
        // First-run welcome: a guided start, not an instruction fragment.
        column![
            text(t!("servers-get-started")).size(Pixels(22.0)),
            text(t!("servers-get-started-help")).style(builtins::text::muted),
            button(text(t!("servers-add-first")))
                .style(builtins::button::primary)
                .padding([8, 18])
                .on_press(Message::RequestCreateServer),
        ]
        .spacing(15)
        .into()
    } else {
        column![text(t!("servers-select")).style(builtins::text::muted)].into()
    }
}

/// The main view function for the connect modal.
pub fn view(state: &State) -> Element<'_, Message> {
    let server_pane = view_server_list(state);

    // Determine the content for the main pane based on the state. The details
    // view manages its own overflow (a pinned header/footer around an inner
    // profile-list scrollable, which an outer scrollable's unbounded height
    // would collapse); every other pane is a fixed-height form or placeholder
    // that must scroll rather than clip when the modal shrinks.
    let (main_pane_content, scrolls_itself) = if let Some(action) = &state.server_action {
        // Show server form if a server action is active
        (view_server_form(state, action), false)
    } else if let Some(action) = &state.profile_action {
        // Show profile form if a profile action is active (Create, Edit, or ConfirmDelete)
        (view_profile_form(state, action), false)
    } else if let Some(server_name) = &state.selected_server {
        // Show server details and profiles if a server is selected
        (view_server_details_and_profiles(state, server_name), true)
    } else {
        // Show placeholder if no server is selected and no form is active
        (view_placeholder(state), false)
    };
    let main_pane_content: Element<'_, Message> = if scrolls_itself {
        main_pane_content
    } else {
        // Right padding keeps form controls clear of the overlaid scrollbar
        // that appears once the pane overflows.
        iced::widget::scrollable(
            container(main_pane_content).padding(iced::Padding::ZERO.right(14)),
        )
        .height(Length::Fill)
        .into()
    };

    let main_pane = container(main_pane_content)
        .width(Length::Fill)
        .padding(15)
        .into();

    // Combine panes into the modal body
    let panes: Element<'_, Message> = Row::with_children(vec![server_pane, main_pane]).into();
    let body: Element<'_, Message> = match &state.session_open_error {
        Some(error) => column![
            container(text(error).style(builtins::text::danger)).padding([8, 15]),
            panes,
        ]
        .into(),
        None => panes,
    };

    // A metadata link held at the trust gate renders its confirm dialog over
    // the whole modal body, exactly like the session view's link dialog.
    match &state.link_confirm {
        Some(pending) => iced::widget::stack![body, observed::link_confirm_dialog(pending)].into(),
        None => body,
    }
}
