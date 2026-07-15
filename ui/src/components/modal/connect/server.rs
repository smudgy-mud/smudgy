//! Server CRUD: async wrappers, form submission handling, and server-side views.

use iced::widget::{
    Column, Row, TextInput, button, column, scrollable,
    space::{horizontal as horizontal_space, vertical as vertical_space},
    text,
};
use iced::{Length, Pixels, Task};
use log::warn;
use validator::Validate;

use crate::theme::Element;
use crate::theme::builtins;

use smudgy_core::models::server::{Server, ServerConfig};

use super::{
    Message, ServerCrudAction, ServerFormField, State, server_host_input_id, server_name_input_id,
    server_port_input_id,
};

/// Helper text shown beneath the port field in the server form.
const PORT_HELP: &str = "Usually 23 or 4000.";

// --- Server CRUD Async Wrappers ---

pub(super) async fn create_server_async(
    name: String,
    config: ServerConfig,
) -> Result<Server, String> {
    smudgy_core::models::server::create_server(&name, config) // Pass name explicitly
        .map_err(|e| e.to_string())
}

pub(super) async fn update_server_async(
    name: String,
    config: ServerConfig,
) -> Result<Server, String> {
    smudgy_core::models::server::update_server(&name, config).map_err(|e| e.to_string())
}

pub(super) async fn delete_server_async(name: String) -> Result<String, String> {
    smudgy_core::models::server::delete_server(&name)
        .map(|_| name) // Return the name on success for state update
        .map_err(|e| e.to_string())
}

// --- Update Logic ---

/// Helper function to handle server form submission.
pub(super) fn handle_submit_server_form(state: &mut State) -> Task<Message> {
    state.server_crud_error = None; // Clear previous error

    match state.server_action.clone() {
        // Clone needed for async task
        Some(ServerCrudAction::Create) => {
            let port = match state.server_form_data.port.trim().parse::<u16>() {
                Ok(p) => p,
                Err(_) => {
                    state.server_crud_error =
                        Some("Invalid port number. Must be between 1 and 65535.".to_string());
                    return Task::none();
                }
            };
            let config = ServerConfig::new(state.server_form_data.host.trim().to_string(), port);
            if let Err(e) = config.validate() {
                state.server_crud_error = Some(format!("Configuration error: {e}"));
                return Task::none();
            }
            let name = state.server_form_data.name.trim().to_string();
            if name.is_empty() {
                state.server_crud_error = Some("Server name cannot be empty.".to_string());
                return Task::none();
            }
            Task::perform(create_server_async(name, config), Message::ServerCreated)
        }
        Some(ServerCrudAction::Edit(original_name)) => {
            let port = match state.server_form_data.port.trim().parse::<u16>() {
                Ok(p) => p,
                Err(_) => {
                    state.server_crud_error =
                        Some("Invalid port number. Must be between 1 and 65535.".to_string());
                    return Task::none();
                }
            };
            // Carry everything the form doesn't edit (the link-trust grants)
            // forward from the existing config, so an address edit never
            // silently revokes them.
            let mut config = state
                .servers
                .iter()
                .find(|s| s.name == *original_name)
                .map_or_else(|| ServerConfig::new(String::new(), 0), |s| s.config.clone());
            config.host = state.server_form_data.host.trim().to_string();
            config.port = port;
            if let Err(e) = config.validate() {
                state.server_crud_error = Some(format!("Configuration error: {e}"));
                return Task::none();
            }
            Task::perform(
                update_server_async(original_name.clone(), config),
                Message::ServerUpdated,
            )
        }
        Some(ServerCrudAction::ConfirmDelete(_)) => {
            warn!("Error: SubmitServerForm called during ConfirmDelete state.");
            state.server_crud_error =
                Some("Unexpected error: Cannot submit while confirming delete.".to_string());
            Task::none()
        }
        None => {
            warn!("Error: SubmitServerForm called without a ServerCrudAction set.");
            state.server_crud_error = Some("Unexpected error: No action in progress.".to_string());
            Task::none()
        }
    }
}

// --- View Logic ---

/// Renders the left rail: a small `Servers` header, the server list, and a
/// persistent `+ New Server` button pinned at the bottom.
pub(super) fn view_server_list(state: &State) -> Element<'_, Message> {
    let server_list_content: Element<Message> = if state.servers.is_empty() {
        if state.is_loading_servers {
            column![text("Loading servers…").style(builtins::text::muted)]
        } else {
            // The no-servers welcome/CTA lives in the right pane; keep the
            // rail itself quiet.
            column![text("No servers yet.").style(builtins::text::muted)]
        }
        .into()
    } else {
        state
            .servers
            .iter()
            .fold(Column::new().spacing(2), |col, server| {
                let is_selected = state.selected_server.as_ref() == Some(&server.name);

                let mut server_button = button(text(&server.name))
                    .width(Length::Fill)
                    .padding([6, 10]);
                server_button = if is_selected {
                    server_button.style(builtins::button::list_item_selected)
                } else {
                    server_button.style(builtins::button::list_item)
                };

                // While a profile form is open the rail selection is inert — the
                // user is mid-edit and switching servers would discard that
                // context. (`+ New Server` below stays live and resets the form.)
                if state.profile_action.is_none() {
                    server_button =
                        server_button.on_press(Message::SelectServer(server.name.clone()));
                }

                col.push(server_button)
            })
            .into()
    };

    column![
        text("Servers").size(12).style(builtins::text::muted),
        scrollable(server_list_content).height(Length::Fill),
        button(text("+ New Server"))
            .width(Length::Fill)
            .padding([6, 10])
            .style(builtins::button::secondary)
            .on_press(Message::RequestCreateServer),
    ]
    .width(Length::Fixed(200.0))
    .spacing(10)
    .padding(15)
    .into()
}

/// Renders the server create/edit form.
pub(super) fn view_server_form<'a>(
    state: &'a State,
    action: &'a ServerCrudAction,
) -> Element<'a, Message> {
    match action {
        ServerCrudAction::Create => {
            // --- Create Form ---
            let name_field = column![
                text("Name").size(13).style(builtins::text::muted),
                TextInput::new("e.g. ArcticMud", &state.server_form_data.name)
                    .id(server_name_input_id())
                    .on_input(|val| Message::UpdateServerFormField(ServerFormField::Name, val))
                    .on_submit(Message::SubmitServerForm),
            ]
            .spacing(4);

            let host_field = column![
                text("Host").size(13).style(builtins::text::muted),
                TextInput::new("mud.example.com", &state.server_form_data.host)
                    .id(server_host_input_id())
                    .on_input(|val| Message::UpdateServerFormField(ServerFormField::Host, val))
                    .on_submit(Message::SubmitServerForm),
            ]
            .spacing(4);

            let port_field = column![
                text("Port").size(13).style(builtins::text::muted),
                TextInput::new("", &state.server_form_data.port)
                    .id(server_port_input_id())
                    .width(Length::Fixed(120.0))
                    .on_input(|val| Message::UpdateServerFormField(ServerFormField::Port, val))
                    .on_submit(Message::SubmitServerForm),
                text(PORT_HELP).size(12).style(builtins::text::muted),
            ]
            .spacing(4);

            let save_button = button(text("Save & add profile"))
                .style(builtins::button::primary)
                .padding([8, 18])
                .on_press(Message::SubmitServerForm);
            let cancel_button = button(text("Cancel"))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::CancelServerForm);

            Column::new()
                .push(text("Add server").size(Pixels(22.0)))
                .push(name_field)
                .push(host_field)
                .push(port_field)
                .push(server_error(state))
                .push(Row::new().push(save_button).push(cancel_button).spacing(10))
                .spacing(15)
                .into()
        }
        ServerCrudAction::Edit(name) => {
            // --- Edit Form — name is the key and stays read-only ---
            let name_field = column![
                text("Name").size(13).style(builtins::text::muted),
                text(name).size(Pixels(16.0)),
            ]
            .spacing(4);

            let host_field = column![
                text("Host").size(13).style(builtins::text::muted),
                TextInput::new("mud.example.com", &state.server_form_data.host)
                    .id(server_host_input_id())
                    .on_input(|val| Message::UpdateServerFormField(ServerFormField::Host, val))
                    .on_submit(Message::SubmitServerForm),
            ]
            .spacing(4);

            let port_field = column![
                text("Port").size(13).style(builtins::text::muted),
                TextInput::new("4000", &state.server_form_data.port)
                    .id(server_port_input_id())
                    .width(Length::Fixed(120.0))
                    .on_input(|val| Message::UpdateServerFormField(ServerFormField::Port, val))
                    .on_submit(Message::SubmitServerForm),
                text(PORT_HELP).size(12).style(builtins::text::muted),
            ]
            .spacing(4);

            let save_button = button(text("Save"))
                .style(builtins::button::primary)
                .padding([8, 18])
                .on_press(Message::SubmitServerForm);
            let cancel_button = button(text("Cancel"))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::CancelServerForm);
            let delete_button = button(text("Delete server"))
                .style(builtins::button::link)
                .on_press(Message::RequestConfirmDeleteServer(name.clone()));

            Column::new()
                .push(text("Edit server").size(Pixels(22.0)))
                .push(name_field)
                .push(host_field)
                .push(port_field)
                .push(server_error(state))
                .push(Row::new().push(save_button).push(cancel_button).spacing(10))
                .push(vertical_space().height(Pixels(10.0)))
                .push(delete_button)
                .spacing(15)
                .into()
        }
        ServerCrudAction::ConfirmDelete(name) => {
            // --- Delete Confirmation ---
            let confirmation_text = text(format!(
                "Are you sure you want to delete the server '{name}'? This cannot be undone."
            ))
            .size(Pixels(16.0));

            let confirm_delete_button = button(text("Yes, delete this server"))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::ConfirmDeleteServer(name.clone()));
            let cancel_delete_button = button(text("Cancel"))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::CancelServerForm);

            Column::new()
                .push(text("Delete server").size(Pixels(22.0)))
                .push(confirmation_text)
                .push(server_error(state))
                .push(
                    Row::new()
                        .push(confirm_delete_button)
                        .push(cancel_delete_button)
                        .spacing(10),
                )
                .spacing(15)
                .into()
        }
    }
}

/// Renders the current server-form error (if any) as danger text, or an empty
/// spacer so the button row doesn't jump when an error appears/clears.
fn server_error(state: &State) -> Element<'_, Message> {
    match &state.server_crud_error {
        Some(error) => text(error).style(builtins::text::danger).into(),
        None => horizontal_space().into(),
    }
}
