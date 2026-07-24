use iced::widget::{Id, Row, button, column, container, operation, text, text_editor};
use iced::{Length, Pixels, Task};
use log::warn;
use crate::i18n::t;

use crate::theme::Element;
use crate::theme::builtins;

// Keep core model imports
use smudgy_core::models::profile::{
    clear_profile_password, contains_password_token, has_profile_password, set_profile_password,
};
use smudgy_core::models::{profile::Profile, server::Server};
use std::collections::HashMap;

mod profile;
mod server;

#[cfg(test)]
mod tests;

use profile::{
    delete_profile_async, handle_submit_profile_form, load_profiles_async, view_profile_form,
    view_server_details_and_profiles,
};
use server::{delete_server_async, handle_submit_server_form, view_server_form, view_server_list};

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
}

// Messages handled internally by this modal's update logic
#[derive(Debug, Clone)]
pub enum Message {
    // Data Loading. (Servers + the first server's profiles are loaded
    // synchronously up front in `State::opening`, so there is no `ServersLoaded`
    // round trip; `ProfilesLoaded` still backs selecting *other* servers.)
    ProfilesLoaded(ServerName, Result<Vec<Profile>, String>),
    // UI Interaction
    SelectServer(ServerName),
    // Handled in `update()` (maps to `Event::CloseModalRequested`); the parent
    // does not yet send this on Esc / background click.
    #[allow(dead_code)]
    CloseRequested, // E.g., from Esc key or background click mapped by parent
    ConnectProfile(ServerName, ProfileName),
    OpenOfflineProfile(ServerName, ProfileName),
    // Server CRUD UI Actions
    RequestCreateServer,
    RequestEditServer(ServerName),
    RequestConfirmDeleteServer(ServerName), // User clicks delete in details view
    ConfirmDeleteServer(ServerName),        // User confirms deletion
    // Server Form Interaction
    UpdateServerFormField(ServerFormField, String),
    ToggleServerCompression(bool),
    ToggleServerTls(bool),
    ToggleServerTlsVerify(bool),
    SubmitServerForm,
    CancelServerForm,
    // Server CRUD Async Results
    ServerCreated(Result<Server, String>),
    ServerUpdated(Result<Server, String>),
    ServerDeleted(Result<ServerName, String>), // Pass back name on success
    // --- Profile CRUD ---
    // UI Actions (act on selected_server)
    RequestCreateProfile,
    RequestEditProfile(ProfileName),
    RequestConfirmDeleteProfile(ProfileName),
    ConfirmDeleteProfile(ProfileName),
    // Form Interaction
    UpdateProfileFormField(ProfileFormField, String),
    UpdateProfileFormSendOnConnect(text_editor::Action),
    SubmitProfileForm,
    CancelProfileForm,
    // Auto-login password ($PASSWORD)
    UpdateProfileFormPassword(String),
    RequestChangeProfilePassword,
    ClearProfilePassword,
    // Async Results
    ProfileCreated(Result<smudgy_core::models::profile::Profile, String>),
    ProfileUpdated(Result<smudgy_core::models::profile::Profile, String>),
    ProfileDeleted(Result<(ServerName, ProfileName), String>), // Need both names for state update
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

/// Temporary storage for server form input.
#[derive(Debug)]
pub struct ServerConfigFormData {
    pub name: String,
    pub host: String,
    pub port: String,
    /// The encoding dropdown's display value; [`server::DEFAULT_ENCODING_CHOICE`]
    /// stands for "no override" (UTF-8, `ServerConfig::encoding = None`).
    pub encoding: String,
    /// Whether inbound compression offers (MCCP2) are accepted.
    pub compression: bool,
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
    Edit(ServerName),          // Stores the original name for the update operation
    ConfirmDelete(ServerName), // Confirmation step before deleting
}

/// Represents the current profile-related action being performed (if any).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileCrudAction {
    Create,                     // Assumes context of state.selected_server
    Edit(ProfileName),          // Assumes context of state.selected_server
    ConfirmDelete(ProfileName), // Confirmation step before deleting
}

// State managed by this modal
pub struct State {
    servers: Vec<Server>,
    profiles: HashMap<ServerName, Vec<Profile>>,
    selected_server: Option<ServerName>,
    is_loading_servers: bool,
    is_loading_profiles: Option<ServerName>,
    // --- Server CRUD State ---
    /// Tracks if we are currently creating or editing a server.
    server_action: Option<ServerCrudAction>,
    /// Holds the temporary data entered into the server form.
    server_form_data: ServerConfigFormData, // Use Default::default()
    /// Holds any error message related to server CRUD operations.
    server_crud_error: Option<String>,
    // --- Profile CRUD State ---
    /// Tracks if we are currently creating or editing a profile.
    profile_action: Option<ProfileCrudAction>,
    /// Holds the temporary data entered into the profile form.
    profile_form_data: ProfileConfigFormData,
    profile_form_send_on_connect_content: text_editor::Content,
    /// Holds any error message related to profile CRUD operations.
    profile_crud_error: Option<String>,
    /// Secure-input buffer for a new auto-login password. Never persisted to disk;
    /// stored in the OS keyring on save. Empty unless the user is entering one.
    profile_form_password: String,
    /// Whether a password is already stored in the keyring for the profile being edited.
    profile_form_password_stored: bool,
    /// Whether to show the password input (`true`) vs the "saved" chip (`false`).
    profile_form_password_editing: bool,
}

// Manual `Debug` (not derived) so the live credential buffer `profile_form_password`
// never reaches a Debug/log surface; the rest is summarized for diagnostics.
impl std::fmt::Debug for State {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("State")
            .field("servers", &self.servers.len())
            .field("selected_server", &self.selected_server)
            .field("server_action", &self.server_action)
            .field("profile_action", &self.profile_action)
            .field("profile_form_password", &"<redacted>")
            .field(
                "profile_form_password_stored",
                &self.profile_form_password_stored,
            )
            .finish_non_exhaustive()
    }
}

impl State {
    /// Builds the modal already populated with the server list and the first
    /// server's profiles, read synchronously. These are small local-disk reads, so
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
        if let Some(first) = servers.first() {
            let name = first.name.clone();
            let mut profiles =
                smudgy_core::models::profile::list_profiles(&name).unwrap_or_else(|e| {
                    warn!("Failed to load profiles for '{name}': {e}");
                    Vec::new()
                });
            profiles.sort_by(|a, b| a.name.cmp(&b.name));
            state.selected_server = Some(name.clone());
            state.profiles.insert(name, profiles);
        }
        state.servers = servers;
        state
    }
}

impl Default for State {
    fn default() -> Self {
        State {
            servers: Vec::new(),
            profiles: HashMap::new(),
            selected_server: None,
            is_loading_servers: false, // Load triggered by update
            is_loading_profiles: None,
            server_action: None,
            server_form_data: ServerConfigFormData::default(),
            server_crud_error: None,
            profile_action: None,
            profile_form_data: ProfileConfigFormData::default(),
            profile_form_send_on_connect_content: text_editor::Content::with_text(""),
            profile_crud_error: None,
            profile_form_password: String::new(),
            profile_form_password_stored: false,
            profile_form_password_editing: false,
        }
    }
}

// --- Auto-login password helpers ---

/// Clears the transient password-form state (buffer + flags). Called whenever a
/// profile form is closed or submitted.
fn reset_password_form(state: &mut State) {
    state.profile_form_password = String::new();
    state.profile_form_password_stored = false;
    state.profile_form_password_editing = false;
}

/// Keeps the OS keyring in sync with a just-saved profile's auto-login text:
/// - `$PASSWORD` present and a new password was typed → store it;
/// - `$PASSWORD` absent → drop any previously-stored password (it can never be
///   used), so removing the token also forgets the secret.
///
/// The buffer is cleared afterward. Best effort — the profile itself already saved,
/// so a keyring failure is logged rather than surfaced.
fn persist_profile_password(
    state: &mut State,
    server_name: &str,
    profile_name: &str,
    send_on_connect: &str,
) {
    let result = if !contains_password_token(send_on_connect) {
        // Token removed → drop any stored password (it can never be used again).
        clear_profile_password(server_name, profile_name)
    } else if state.profile_form_password.trim().is_empty() {
        // Token present but nothing newly typed → keep whatever is already stored.
        // (Matches the submit-gate check, which also trims, so a whitespace-only
        // buffer isn't mistakenly stored as the password.)
        Ok(())
    } else {
        set_profile_password(server_name, profile_name, &state.profile_form_password)
    };
    if let Err(e) = result {
        warn!(
            "Failed to update stored auto-login password for '{server_name}/{profile_name}': {e}"
        );
    }
    reset_password_form(state);
}

// --- Update Logic ---

/// Handles messages specific to the Connect Modal logic.
pub fn update(state: &mut State, message: Message) -> (Task<Message>, Option<Event>) {
    let mut task = Task::none();
    let mut event = None;

    // Clear server CRUD error on most actions unless explicitly set
    if !matches!(
        message,
        Message::SubmitServerForm
            | Message::ServerCreated(_)
            | Message::ServerUpdated(_)
            | Message::ServerDeleted(_)
    ) {
        state.server_crud_error = None;
    }

    match message {
        Message::ProfilesLoaded(server_name, Ok(mut profiles)) => {
            // Add mut for sorting
            if state.is_loading_profiles.as_ref() == Some(&server_name) {
                state.is_loading_profiles = None;
            }
            // Sort profiles by name for consistent display
            profiles.sort_by(|a, b| a.name.cmp(&b.name));
            state.profiles.insert(server_name, profiles);
        }
        Message::ProfilesLoaded(server_name, Err(e)) => {
            if state.is_loading_profiles.as_ref() == Some(&server_name) {
                state.is_loading_profiles = None;
            }
            let err_msg = t!(
                "profiles-error-load",
                "server" => &server_name,
                "error" => e.to_string()
            );
            warn!("{err_msg}");
            state.profile_crud_error = Some(err_msg); // Display error to user
        }
        Message::SelectServer(server_name) => {
            if state.selected_server.as_ref() != Some(&server_name) {
                let server_name_clone = server_name.clone();
                state.selected_server = Some(server_name_clone.clone());
                if !state.profiles.contains_key(&server_name_clone) {
                    state.is_loading_profiles = Some(server_name_clone.clone());
                    task = Task::perform(
                        load_profiles_async(server_name_clone.clone()),
                        move |result| {
                            let name = server_name_clone.clone();
                            Message::ProfilesLoaded(name, result)
                        },
                    );
                }
            }
        }
        Message::CloseRequested => {
            event = Some(Event::CloseModalRequested);
        }
        Message::ConnectProfile(server_name, profile_name) => {
            // Cancel any ongoing server CRUD action if user connects
            state.server_action = None;
            state.server_form_data = ServerConfigFormData::default();
            state.server_crud_error = None;
            event = Some(Event::Connect(server_name, profile_name));
        }
        Message::OpenOfflineProfile(server_name, profile_name) => {
            // Same housekeeping as `ConnectProfile`; the parent opens the session
            // without establishing a connection.
            state.server_action = None;
            state.server_form_data = ServerConfigFormData::default();
            state.server_crud_error = None;
            event = Some(Event::OpenOffline(server_name, profile_name));
        }
        Message::RequestCreateServer => {
            state.server_action = Some(ServerCrudAction::Create);
            state.server_form_data = ServerConfigFormData::default(); // Clear form
            state.server_crud_error = None;
            state.selected_server = None; // De-select server when opening create form
            state.is_loading_profiles = None; // Cancel profile load
            // `+ New Server` is persistent, so it can be pressed while a profile
            // form is open; drop that form so it doesn't resurface on cancel.
            state.profile_action = None;
            task = operation::focus(server_name_input_id());
        }
        Message::RequestEditServer(server_name) => {
            if let Some(server_to_edit) = state.servers.iter().find(|s| s.name == server_name) {
                state.server_action = Some(ServerCrudAction::Edit(server_name.clone()));
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
                    tls: server_to_edit.config.tls,
                    tls_verify: server_to_edit.config.tls_verify,
                };
                state.server_crud_error = None;
                state.selected_server = Some(server_name); // Ensure server remains selected
                state.is_loading_profiles = None; // Cancel profile load
                // Name isn't editable in edit mode; focus the first editable field.
                task = operation::focus(server_host_input_id());
            } else {
                warn!("Error: Requested to edit non-existent server '{server_name}'");
            }
        }
        Message::RequestConfirmDeleteServer(server_name) => {
            state.server_action = Some(ServerCrudAction::ConfirmDelete(server_name));
            state.server_crud_error = None;
            state.profile_action = None; // Ensure profile form is hidden
        }
        Message::ConfirmDeleteServer(server_name) => {
            state.server_crud_error = None;
            task = Task::perform(delete_server_async(server_name), Message::ServerDeleted);
            // The state.server_action remains ConfirmDelete until ServerDeleted result arrives.
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
            }
        }
        Message::ToggleServerCompression(value) => {
            if matches!(
                state.server_action,
                Some(ServerCrudAction::Create) | Some(ServerCrudAction::Edit(_))
            ) {
                state.server_form_data.compression = value;
            }
        }
        Message::ToggleServerTls(value) => {
            if matches!(
                state.server_action,
                Some(ServerCrudAction::Create) | Some(ServerCrudAction::Edit(_))
            ) {
                state.server_form_data.tls = value;
            }
        }
        Message::ToggleServerTlsVerify(value) => {
            if matches!(
                state.server_action,
                Some(ServerCrudAction::Create) | Some(ServerCrudAction::Edit(_))
            ) {
                state.server_form_data.tls_verify = value;
            }
        }
        Message::SubmitServerForm => {
            task = handle_submit_server_form(state);
        }
        Message::CancelServerForm => {
            // Clear action, form data, and error regardless of previous state
            state.server_action = None;
            state.server_form_data = ServerConfigFormData::default();
            state.server_crud_error = None;
            // If a server was selected before opening the form (e.g., for Edit or ConfirmDelete),
            // we don't explicitly re-select it here. The user can click it again in the list.
            // This keeps the cancellation logic simple.
        }
        Message::ServerCreated(result) => {
            match result {
                Ok(new_server) => {
                    state.server_action = None;
                    state.server_form_data = ServerConfigFormData::default();
                    state.server_crud_error = None;

                    // Add to list and sort (optional, but good for UI)
                    state.servers.push(new_server.clone());
                    state.servers.sort_by(|a, b| a.name.cmp(&b.name));

                    // Select the new server and trigger profile load
                    let server_name_clone = new_server.name.clone();
                    state.selected_server = Some(server_name_clone.clone());
                    state.is_loading_profiles = Some(server_name_clone.clone());
                    let load_task =
                        Task::perform(load_profiles_async(server_name_clone.clone()), move |res| {
                            let name = server_name_clone.clone();
                            Message::ProfilesLoaded(name, res)
                        });

                    // "Save & add profile" flows straight into the Add-profile
                    // form for the new server, removing a navigation step on first
                    // connect. `ServerCreated` only fires for the create path, so this
                    // chaining is correct (edit goes through `ServerUpdated`).
                    state.profile_action = Some(ProfileCrudAction::Create);
                    state.profile_form_data = ProfileConfigFormData::default();
                    state.profile_form_send_on_connect_content = text_editor::Content::new();
                    state.profile_crud_error = None;
                    state.profile_form_password = String::new();
                    state.profile_form_password_stored = false;
                    state.profile_form_password_editing = true;

                    task = Task::batch([load_task, operation::focus(profile_name_input_id())]);
                }
                Err(e) => {
                    state.server_crud_error = Some(t!("server-error-create", "error" => e.to_string()));
                }
            }
        }
        Message::ServerUpdated(result) => {
            match result {
                Ok(updated_server) => {
                    state.server_action = None;
                    state.server_form_data = ServerConfigFormData::default();
                    state.server_crud_error = None;

                    // Find and update in the list
                    if let Some(server_in_list) = state
                        .servers
                        .iter_mut()
                        .find(|s| s.name == updated_server.name)
                    {
                        *server_in_list = updated_server.clone();
                    } else {
                        warn!(
                            "Error: Updated server '{}' not found in list after update.",
                            updated_server.name
                        );
                    }
                    state.selected_server = Some(updated_server.name);
                }
                Err(e) => {
                    state.server_crud_error = Some(t!("server-error-update", "error" => e.to_string()));
                }
            }
        }
        Message::ServerDeleted(result) => {
            match result {
                Ok(deleted_name) => {
                    state.server_crud_error = None; // Clear any previous error
                    state.server_action = None; // Ensure action is cleared after successful delete

                    // Remove from server list
                    state.servers.retain(|s| s.name != deleted_name);
                    // Remove from profiles map
                    state.profiles.remove(&deleted_name);

                    // If the deleted server was selected, select the first one or none
                    if state.selected_server.as_ref() == Some(&deleted_name) {
                        if let Some(first_server) = state.servers.first() {
                            let server_name_clone = first_server.name.clone();
                            state.selected_server = Some(server_name_clone.clone());
                            state.is_loading_profiles = Some(server_name_clone.clone());
                            task = Task::perform(
                                load_profiles_async(server_name_clone.clone()),
                                move |res| {
                                    let name = server_name_clone.clone();
                                    Message::ProfilesLoaded(name, res)
                                },
                            );
                        } else {
                            state.selected_server = None;
                            state.is_loading_profiles = None;
                        }
                    }
                    // No need to clear server_action etc. as delete happens outside the form flow
                }
                Err(e) => {
                    // Show error, maybe associate with the server if possible?
                    state.server_crud_error = Some(t!("server-error-delete", "error" => e.to_string()));
                    warn!("Failed to delete server: {e}");
                    // If deletion failed while confirming, reset state back to None
                    // (or maybe back to Edit if that was the origin? Simpler to just reset)
                    if matches!(
                        state.server_action,
                        Some(ServerCrudAction::ConfirmDelete(_))
                    ) {
                        state.server_action = None;
                    }
                }
            }
        }
        Message::RequestCreateProfile => {
            if state.selected_server.is_some() {
                state.profile_action = Some(ProfileCrudAction::Create);
                state.profile_form_data = ProfileConfigFormData::default();
                state.profile_form_send_on_connect_content = text_editor::Content::new();
                state.profile_crud_error = None;
                state.profile_form_password = String::new();
                state.profile_form_password_stored = false;
                state.profile_form_password_editing = true;
                state.server_action = None; // Hide server form
                task = operation::focus(profile_name_input_id());
            } else {
                warn!("Error: Cannot create profile, no server selected.");
            }
        }
        Message::RequestEditProfile(profile_name) => {
            // Ensure a server is selected first
            if let Some(server_name) = &state.selected_server {
                // Find the profile within the selected server's profile list
                if let Some(profile_vec) = state.profiles.get(server_name) {
                    if let Some(profile_to_edit) =
                        profile_vec.iter().find(|p| p.name == profile_name)
                    {
                        state.profile_action = Some(ProfileCrudAction::Edit(profile_name.clone()));
                        state.profile_form_data = ProfileConfigFormData {
                            name: profile_to_edit.name.clone(), // Pre-fill name for context (won't be editable in form)
                            description: profile_to_edit.config.caption.clone(),
                        };
                        state.profile_form_send_on_connect_content =
                            text_editor::Content::with_text(
                                profile_to_edit.config.send_on_connect.as_str(),
                            );
                        state.profile_crud_error = None;
                        state.server_action = None; // Hide server form if it was open
                        // Reflect whether a password is already stored for this
                        // profile: show the "saved" chip if so, the input if not.
                        let stored = has_profile_password(server_name, &profile_name);
                        state.profile_form_password = String::new();
                        state.profile_form_password_stored = stored;
                        state.profile_form_password_editing = !stored;
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
        Message::RequestConfirmDeleteProfile(profile_name) => {
            state.profile_action = Some(ProfileCrudAction::ConfirmDelete(profile_name));
            state.profile_crud_error = None;
        }
        Message::ConfirmDeleteProfile(profile_name) => {
            state.profile_crud_error = None;
            // Let's try calling the async task directly for simplicity.
            if let Some(server_name) = state.selected_server.clone() {
                // Use if let for safety
                state.profile_crud_error = None;
                task = Task::perform(
                    delete_profile_async(server_name, profile_name),
                    Message::ProfileDeleted,
                );
            } else {
                warn!("Error: Cannot delete profile, no server selected during confirmation.");
                state.profile_crud_error = Some(t!("profile-error-delete-no-server"));
                state.profile_action = None;
            }
        }
        Message::UpdateProfileFormField(field, value) => {
            match field {
                ProfileFormField::Name => state.profile_form_data.name = value,
                ProfileFormField::Description => state.profile_form_data.description = value,
            }
            state.profile_crud_error = None;
        }
        Message::UpdateProfileFormSendOnConnect(action) => {
            state.profile_form_send_on_connect_content.perform(action);
        }
        Message::SubmitProfileForm => {
            task = handle_submit_profile_form(state);
        }
        Message::CancelProfileForm => {
            state.profile_action = None;
            state.profile_form_data = ProfileConfigFormData::default();
            state.profile_form_send_on_connect_content = text_editor::Content::new(); // Reset editor content
            state.profile_crud_error = None;
            reset_password_form(state);
        }
        Message::UpdateProfileFormPassword(value) => {
            state.profile_form_password = value;
            state.profile_crud_error = None;
        }
        Message::RequestChangeProfilePassword => {
            // Reveal the input to enter a replacement password.
            state.profile_form_password = String::new();
            state.profile_form_password_editing = true;
            task = operation::focus(profile_password_input_id());
        }
        Message::ClearProfilePassword => {
            // Drop the stored password now (this is the only destructive action and
            // the user asked for it explicitly), then show the empty input again.
            if let (Some(server_name), Some(ProfileCrudAction::Edit(profile_name))) =
                (&state.selected_server, &state.profile_action)
            {
                // Clear the stored secret now — this is an explicit user action.
                let cleared = clear_profile_password(server_name, profile_name);
                if let Err(e) = cleared {
                    state.profile_crud_error =
                        Some(format!("Failed to clear stored password: {e}"));
                }
            }
            state.profile_form_password = String::new();
            state.profile_form_password_stored = false;
            state.profile_form_password_editing = true;
        }
        Message::ProfileCreated(result) => {
            match result {
                Ok(new_profile) => {
                    if let Some(server_name) = state.selected_server.clone() {
                        persist_profile_password(
                            state,
                            &server_name,
                            &new_profile.name,
                            &new_profile.config.send_on_connect,
                        );
                    } else {
                        reset_password_form(state);
                    }
                    state.profile_action = None;
                    state.profile_form_data = ProfileConfigFormData::default();
                    state.profile_crud_error = None;

                    // Need to find the server name this profile belongs to.
                    // This relies on the create action having been initiated with a selected server.
                    // A more robust approach might involve the async task returning the server name.
                    // For now, assume state.selected_server holds the relevant server.
                    if let Some(server_name) = &state.selected_server {
                        if let Some(server_profiles) = state.profiles.get_mut(server_name) {
                            server_profiles.push(new_profile.clone());
                            server_profiles.sort_by(|a, b| a.name.cmp(&b.name)); // Sort by name
                        } else {
                            warn!(
                                "Error: Server '{}' not found in profile map after creating profile '{}'",
                                server_name, new_profile.name
                            );
                        }
                    } else {
                        warn!("Error: No server selected after profile creation finished.")
                    }
                    // Keep the current server selected
                }
                Err(e) => {
                    state.profile_crud_error = Some(t!("profile-error-create", "error" => e.to_string()));
                }
            }
        }
        Message::ProfileUpdated(result) => {
            match result {
                Ok(updated_profile) => {
                    if let Some(server_name) = state.selected_server.clone() {
                        persist_profile_password(
                            state,
                            &server_name,
                            &updated_profile.name,
                            &updated_profile.config.send_on_connect,
                        );
                    } else {
                        reset_password_form(state);
                    }
                    state.profile_action = None;
                    state.profile_form_data = ProfileConfigFormData::default();
                    state.profile_crud_error = None;

                    // Assume state.selected_server holds the relevant server context
                    if let Some(server_name) = &state.selected_server {
                        if let Some(server_profiles) = state.profiles.get_mut(server_name) {
                            if let Some(profile_in_list) = server_profiles
                                .iter_mut()
                                .find(|p| p.name == updated_profile.name)
                            {
                                *profile_in_list = updated_profile.clone();
                                server_profiles.sort_by(|a, b| a.name.cmp(&b.name)); // Sort by name
                            } else {
                                warn!(
                                    "Error: Updated profile '{}' not found in list for server '{}'",
                                    updated_profile.name, server_name
                                );
                            }
                        } else {
                            warn!(
                                "Error: Server '{}' not found in profile map after updating profile '{}'",
                                server_name, updated_profile.name
                            );
                        }
                    } else {
                        warn!("Error: No server selected after profile update finished.")
                    }
                    // Keep the current server selected
                }
                Err(e) => {
                    state.profile_crud_error = Some(t!("profile-error-update", "error" => e.to_string()));
                }
            }
        }
        Message::ProfileDeleted(result) => {
            match result {
                Ok((server_name, deleted_profile_name)) => {
                    state.profile_crud_error = None;
                    state.profile_action = None; // Ensure action is cleared after successful delete

                    // Remove from the map
                    if let Some(server_profiles) = state.profiles.get_mut(&server_name) {
                        server_profiles.retain(|p| p.name != deleted_profile_name);
                    } else {
                        warn!(
                            "Warning: Server '{server_name}' not found in profile map when handling deletion of profile '{deleted_profile_name}'"
                        );
                    }
                    // If the current server is the one affected, we might want to refresh
                    // its view, but no need to change selection unless the server itself was deleted.
                }
                Err(e) => {
                    // Show error, maybe associate with the server if possible?
                    state.profile_crud_error = Some(t!("profile-error-delete", "error" => e.to_string()));
                    warn!("Failed to delete profile: {e}");
                    // Keep the confirmation state active so the user sees the error
                    // Or maybe reset to Edit state? Let's reset to Edit.
                    if let Some(ProfileCrudAction::ConfirmDelete(name)) = &state.profile_action {
                        state.profile_action = Some(ProfileCrudAction::Edit(name.clone()));
                    }
                }
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

    // Determine the content for the main pane based on the state
    let main_pane_content = if let Some(action) = &state.server_action {
        // Show server form if a server action is active
        view_server_form(state, action)
    } else if let Some(action) = &state.profile_action {
        // Show profile form if a profile action is active (Create, Edit, or ConfirmDelete)
        view_profile_form(state, action)
    } else if let Some(server_name) = &state.selected_server {
        // Show server details and profiles if a server is selected
        view_server_details_and_profiles(state, server_name)
    } else {
        // Show placeholder if no server is selected and no form is active
        view_placeholder(state)
    };

    let main_pane = container(main_pane_content)
        .width(Length::Fill)
        .padding(15)
        .into();

    // Combine panes into the modal body
    Row::with_children(vec![server_pane, main_pane]).into()
}
