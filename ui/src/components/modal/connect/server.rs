//! Server CRUD: async wrappers, form submission handling, and server-side views.

use std::fmt;

use iced::widget::{
    Row, TextInput, button, checkbox, column, pick_list, space::horizontal as horizontal_space,
    text,
};
use iced::{Alignment, Length, Pixels, Task};
use log::warn;
use validator::Validate;

use crate::i18n::{t, ts};
use crate::theme::Element;
use crate::theme::builtins;

use smudgy_core::models::server::{ServerCas, ServerConfig, parse_wss_address};

use super::{
    AppliedServerOperation, Message, ServerCrudAction, ServerFormField, ServerOperationAction,
    ServerOperationCompletion, ServerOperationEnvelope, ServerOperationError, State,
    next_server_operation_id, server_host_input_id, server_name_input_id, server_port_input_id,
    server_wss_input_id,
};

/// The encoding dropdown's "no override" entry — UTF-8, i.e.
/// `ServerConfig::encoding = None`.
pub(super) const DEFAULT_ENCODING_CHOICE: &str = "Default (UTF-8)";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct EncodingChoice(&'static str);

impl fmt::Display for EncodingChoice {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.0 == DEFAULT_ENCODING_CHOICE {
            formatter.write_str(ts!("encoding-default"))
        } else {
            formatter.write_str(self.0)
        }
    }
}

/// The curated encoding choices: [`DEFAULT_ENCODING_CHOICE`] plus the
/// MUD-relevant Encoding Standard labels (each resolves via
/// `encoding_rs::Encoding::for_label_no_replacement`; the connection falls
/// back to UTF-8 — loudly, in the session view — on anything unresolvable,
/// so a hand-edited `server.json` can hold any label).
const ENCODING_CHOICES: [EncodingChoice; 15] = [
    EncodingChoice(DEFAULT_ENCODING_CHOICE),
    EncodingChoice("windows-1252"), // Latin-1 / Western European superset
    EncodingChoice("iso-8859-2"),
    EncodingChoice("iso-8859-15"),
    EncodingChoice("windows-1250"),
    EncodingChoice("windows-1251"), // Cyrillic
    EncodingChoice("koi8-r"),
    EncodingChoice("koi8-u"),
    EncodingChoice("windows-1256"), // Arabic
    EncodingChoice("big5"),         // Traditional Chinese
    EncodingChoice("gbk"),          // Simplified Chinese
    EncodingChoice("gb18030"),
    EncodingChoice("shift_jis"), // Japanese
    EncodingChoice("euc-jp"),
    EncodingChoice("euc-kr"), // Korean
];

/// Map the dropdown value back to the persisted form (`None` = UTF-8).
fn form_encoding_to_config(choice: &str) -> Option<String> {
    (choice != DEFAULT_ENCODING_CHOICE).then(|| choice.to_string())
}

/// The independent inbound-compression checkboxes for the server form.
fn compression_field(state: &State) -> Element<'_, Message> {
    column![
        checkbox(state.server_form_data.compression)
            .label(t!("server-compression"))
            .on_toggle(Message::ToggleServerCompression)
            .size(16),
        checkbox(state.server_form_data.mccp4_compression)
            .label(t!("server-mccp4-compression"))
            .on_toggle(Message::ToggleServerMccp4Compression)
            .size(16),
    ]
    .spacing(4)
    .into()
}

/// The TLS checkbox, with a nested "Verify certificate" shown only when TLS is on.
fn tls_field(state: &State) -> Element<'_, Message> {
    let secure = checkbox(state.server_form_data.tls)
        .label(t!("server-tls"))
        .on_toggle(Message::ToggleServerTls)
        .size(16);
    let mut col = column![secure].spacing(4);
    if state.server_form_data.tls {
        let verify = checkbox(state.server_form_data.tls_verify)
            .label(t!("server-tls-verify"))
            .on_toggle(Message::ToggleServerTlsVerify)
            .size(16);
        col = col.push(
            column![
                verify,
                text(t!("server-tls-insecure-help"))
                    .size(12)
                    .style(builtins::text::muted),
            ]
            .spacing(2)
            .padding(iced::Padding::default().left(24.0)),
        );
    }
    col.into()
}

/// The `Encoding` field for the server form: a dropdown over
/// [`ENCODING_CHOICES`]. A config label outside the curated list (a hand-edited
/// `server.json`) shows as no selection; it persists untouched unless re-picked.
fn encoding_field(state: &State) -> Element<'_, Message> {
    let selected = ENCODING_CHOICES
        .iter()
        .copied()
        .find(|choice| choice.0 == state.server_form_data.encoding);
    column![
        text(t!("encoding-label"))
            .size(13)
            .style(builtins::text::muted),
        pick_list(&ENCODING_CHOICES[..], selected, |val: EncodingChoice| {
            Message::UpdateServerFormField(ServerFormField::Encoding, val.0.to_string())
        })
        .width(Length::Fixed(220.0)),
        text(t!("encoding-help"))
            .size(12)
            .style(builtins::text::muted),
    ]
    .spacing(4)
    .into()
}

// --- Server CRUD Async Wrappers ---

pub(super) async fn execute_server_operation(
    envelope: ServerOperationEnvelope,
) -> ServerOperationCompletion {
    let key = envelope.key();
    let failed = |error: anyhow::Error| ServerOperationError::Failed(error.to_string());
    let result = match envelope.action {
        ServerOperationAction::Create {
            target_name,
            config,
        } => smudgy_core::models::server::create_server(&target_name, config)
            .map(AppliedServerOperation::Created)
            .map_err(failed),
        ServerOperationAction::Update { expected, config } => {
            match smudgy_core::models::server::update_server_if_unchanged(&expected, config) {
                Ok(ServerCas::Applied(server)) => Ok(AppliedServerOperation::Updated(server)),
                Ok(ServerCas::StateChanged) => Err(ServerOperationError::StateChanged),
                Err(error) => Err(failed(error)),
            }
        }
        ServerOperationAction::Delete { expected } => {
            match smudgy_core::models::server::delete_server_if_unchanged_and_then(
                &expected,
                || {
                    // Memory is cleared first, aborting same-process fetches before the retired
                    // name-keyed disk namespace is removed. The Core helper keeps the lifecycle lock
                    // across this callback, so no same-name replacement can appear in between.
                    crate::images::clear_server_image_cache(&expected.name);
                },
            ) {
                Ok(ServerCas::Applied(())) => Ok(AppliedServerOperation::Deleted),
                Ok(ServerCas::StateChanged) => Err(ServerOperationError::StateChanged),
                Err(error) => Err(failed(error)),
            }
        }
    };
    ServerOperationCompletion { key, result }
}

/// `1234567` → `"1.2 MB"` — decimal units, one decimal, matching user expectations for
/// disk usage figures.
pub(super) fn format_bytes(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let bytes = bytes as f64;
    // Thresholds sit at the rounding boundary so 999_960 B is "1.0 MB", never "1000.0 kB".
    if bytes >= 999_950_000.0 {
        format!("{:.1} GB", bytes / 1_000_000_000.0)
    } else if bytes >= 999_950.0 {
        format!("{:.1} MB", bytes / 1_000_000.0)
    } else if bytes >= 999.95 {
        format!("{:.1} kB", bytes / 1_000.0)
    } else {
        format!("{bytes:.0} B")
    }
}

// --- Update Logic ---

fn form_address(state: &State) -> Result<(String, u16, Option<String>), String> {
    let wss_url = state.server_form_data.wss_url.trim();
    if !wss_url.is_empty() {
        return parse_wss_address(wss_url)
            .map(|(host, port)| (host, port, Some(wss_url.to_owned())))
            .map_err(|error| error.to_string());
    }
    let port = state
        .server_form_data
        .port
        .trim()
        .parse::<u16>()
        .map_err(|_| t!("server-error-port"))?;
    Ok((state.server_form_data.host.trim().to_owned(), port, None))
}

fn wss_field(state: &State) -> Element<'_, Message> {
    column![
        text(t!("server-wss-url"))
            .size(13)
            .style(builtins::text::muted),
        TextInput::new(
            "wss://example.org/ws/telnet/",
            &state.server_form_data.wss_url
        )
        .id(server_wss_input_id())
        .on_input(|value| Message::UpdateServerFormField(ServerFormField::WssUrl, value))
        .on_submit(Message::SubmitServerForm),
        text(t!("server-wss-help"))
            .size(12)
            .style(builtins::text::muted),
    ]
    .spacing(4)
    .into()
}

/// Helper function to handle server form submission.
pub(super) fn handle_submit_server_form(state: &mut State) -> Task<Message> {
    if state.pending_server_operation.is_some() {
        warn!("Ignoring server-form submit because an operation is already pending");
        return Task::none();
    }
    state.server_crud_error = None; // Clear previous error

    match state.server_action.clone() {
        // Clone needed for async task
        Some(ServerCrudAction::Create) => {
            let (host, port, wss_url) = match form_address(state) {
                Ok(address) => address,
                Err(error) => {
                    state.server_crud_error = Some(error);
                    return Task::none();
                }
            };
            let mut config = ServerConfig::new(host, port);
            config.wss_url = wss_url;
            config.encoding = form_encoding_to_config(&state.server_form_data.encoding);
            config.compression = state.server_form_data.compression;
            config.mccp4_compression = Some(state.server_form_data.mccp4_compression);
            config.tls = config.wss_url.is_some() || state.server_form_data.tls;
            config.tls_verify = config.wss_url.is_some() || state.server_form_data.tls_verify;
            if let Err(e) = config
                .validate()
                .map_err(anyhow::Error::from)
                .and_then(|()| config.validate_transport())
            {
                state.server_crud_error = Some(t!("server-error-config", "error" => e.to_string()));
                return Task::none();
            }
            let name = state.server_form_data.name.trim().to_string();
            if name.is_empty() {
                state.server_crud_error = Some(t!("server-error-name-empty"));
                return Task::none();
            }
            if name.contains(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
                state.server_crud_error = Some(t!("server-error-name-format"));
                return Task::none();
            }
            let envelope = ServerOperationEnvelope {
                id: next_server_operation_id(state),
                action: ServerOperationAction::Create {
                    target_name: name,
                    config,
                },
                form_revision: state.server_form_revision,
            };
            state.pending_server_operation = Some(envelope.clone());
            Task::perform(
                execute_server_operation(envelope),
                Message::ServerOperationFinished,
            )
        }
        Some(ServerCrudAction::Edit(expected)) => {
            let (host, port, wss_url) = match form_address(state) {
                Ok(address) => address,
                Err(error) => {
                    state.server_crud_error = Some(error);
                    return Task::none();
                }
            };
            // Carry everything the form doesn't edit (the link-trust grants)
            // forward from the existing config, so an address edit never
            // silently revokes them.
            let mut config = expected.config.clone();
            config.host = host;
            config.port = port;
            config.wss_url = wss_url;
            config.encoding = form_encoding_to_config(&state.server_form_data.encoding);
            config.compression = state.server_form_data.compression;
            config.mccp4_compression = Some(state.server_form_data.mccp4_compression);
            config.tls = config.wss_url.is_some() || state.server_form_data.tls;
            config.tls_verify = config.wss_url.is_some() || state.server_form_data.tls_verify;
            if let Err(e) = config
                .validate()
                .map_err(anyhow::Error::from)
                .and_then(|()| config.validate_transport())
            {
                state.server_crud_error = Some(t!("server-error-config", "error" => e.to_string()));
                return Task::none();
            }
            let envelope = ServerOperationEnvelope {
                id: next_server_operation_id(state),
                action: ServerOperationAction::Update { expected, config },
                form_revision: state.server_form_revision,
            };
            state.pending_server_operation = Some(envelope.clone());
            Task::perform(
                execute_server_operation(envelope),
                Message::ServerOperationFinished,
            )
        }
        Some(ServerCrudAction::ConfirmDelete(_)) => {
            warn!("Error: SubmitServerForm called during ConfirmDelete state.");
            state.server_crud_error = Some(t!("server-error-action-delete"));
            Task::none()
        }
        None => {
            warn!("Error: SubmitServerForm called without a ServerCrudAction set.");
            state.server_crud_error = Some(t!("server-error-action-missing"));
            Task::none()
        }
    }
}

// --- View Logic ---

/// Renders the left rail: a small `Servers` header, the server list, and a
/// persistent `+ New Server` button pinned at the bottom.
pub(super) fn view_server_list<'a>(
    state: &'a State,
    servers: Vec<smudgy_ui_shared::connect_model::ServerRow<'a>>,
    new_server: smudgy_ui_shared::connect_model::Intent,
) -> Element<'a, Message> {
    use smudgy_ui_shared::connect_modal::{RailItem, server_rail};
    let rows = servers
        .into_iter()
        .map(|server| {
            // Native-only advertised icons remain an optional label adornment.
            let label: Element<Message> = if let Some(handle) = state.icons.get(server.name) {
                Row::new()
                    .push(
                        iced::widget::image(handle.clone())
                            .width(Pixels(16.0))
                            .height(Pixels(16.0)),
                    )
                    .push(text(server.name))
                    .spacing(8)
                    .align_y(Alignment::Center)
                    .into()
            } else {
                text(server.name).into()
            };
            RailItem {
                label,
                selected: server.selected,
                select: server.select.map(Message::from),
            }
        })
        .collect();
    let heading = t!("servers-title");
    let empty = if state.is_loading_servers {
        t!("servers-loading")
    } else {
        t!("servers-empty")
    };
    let new_label = t!("servers-new");
    server_rail(
        &heading,
        &empty,
        &new_label,
        rows,
        Message::from(new_server),
    )
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
                text(t!("server-name"))
                    .size(13)
                    .style(builtins::text::muted),
                TextInput::new(ts!("server-name-placeholder"), &state.server_form_data.name)
                    .id(server_name_input_id())
                    .on_input(|val| Message::UpdateServerFormField(ServerFormField::Name, val))
                    .on_submit(Message::SubmitServerForm),
            ]
            .spacing(4);

            let host_field: Element<Message> = column![
                text(t!("server-host"))
                    .size(13)
                    .style(builtins::text::muted),
                TextInput::new("mud.example.com", &state.server_form_data.host)
                    .id(server_host_input_id())
                    .on_input(|val| Message::UpdateServerFormField(ServerFormField::Host, val))
                    .on_submit(Message::SubmitServerForm),
            ]
            .spacing(4)
            .into();

            let port_field: Element<Message> = column![
                text(t!("server-port"))
                    .size(13)
                    .style(builtins::text::muted),
                TextInput::new("", &state.server_form_data.port)
                    .id(server_port_input_id())
                    .width(Length::Fixed(120.0))
                    .on_input(|val| Message::UpdateServerFormField(ServerFormField::Port, val))
                    .on_submit(Message::SubmitServerForm),
                text(t!("server-port-help"))
                    .size(12)
                    .style(builtins::text::muted),
            ]
            .spacing(4)
            .into();

            let mut save_button = button(text(t!("server-save-add-profile")))
                .style(builtins::button::primary)
                .padding([8, 18]);
            if state.pending_server_operation.is_none() {
                save_button = save_button.on_press(Message::SubmitServerForm);
            }
            let cancel_button = button(text(t!("action-cancel")))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::CancelServerForm);

            smudgy_ui_shared::connect_modal::form(
                text(t!("server-add")).size(Pixels(22.0)).into(),
                vec![
                    name_field.into(),
                    wss_field(state),
                    if state.server_form_data.wss_url.trim().is_empty() {
                        host_field
                    } else {
                        horizontal_space().into()
                    },
                    if state.server_form_data.wss_url.trim().is_empty() {
                        port_field
                    } else {
                        horizontal_space().into()
                    },
                    encoding_field(state),
                    compression_field(state),
                    if state.server_form_data.wss_url.trim().is_empty() {
                        tls_field(state)
                    } else {
                        horizontal_space().into()
                    },
                    server_error(state),
                ],
                Row::new()
                    .push(save_button)
                    .push(cancel_button)
                    .spacing(10)
                    .into(),
                None,
            )
        }
        ServerCrudAction::Edit(expected) => {
            // --- Edit Form — name is the key and stays read-only ---
            let name = &expected.name;
            let name_field = column![
                text(t!("server-name"))
                    .size(13)
                    .style(builtins::text::muted),
                text(name).size(Pixels(16.0)),
            ]
            .spacing(4);

            let host_field: Element<Message> = column![
                text(t!("server-host"))
                    .size(13)
                    .style(builtins::text::muted),
                TextInput::new("mud.example.com", &state.server_form_data.host)
                    .id(server_host_input_id())
                    .on_input(|val| Message::UpdateServerFormField(ServerFormField::Host, val))
                    .on_submit(Message::SubmitServerForm),
            ]
            .spacing(4)
            .into();

            let port_field: Element<Message> = column![
                text(t!("server-port"))
                    .size(13)
                    .style(builtins::text::muted),
                TextInput::new("4000", &state.server_form_data.port)
                    .id(server_port_input_id())
                    .width(Length::Fixed(120.0))
                    .on_input(|val| Message::UpdateServerFormField(ServerFormField::Port, val))
                    .on_submit(Message::SubmitServerForm),
                text(t!("server-port-help"))
                    .size(12)
                    .style(builtins::text::muted),
            ]
            .spacing(4)
            .into();

            let mut save_button = button(text(t!("action-save")))
                .style(builtins::button::primary)
                .padding([8, 18]);
            if state.pending_server_operation.is_none() {
                save_button = save_button.on_press(Message::SubmitServerForm);
            }
            let cancel_button = button(text(t!("action-cancel")))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::CancelServerForm);
            let mut delete_button = button(text(t!("server-delete"))).style(builtins::button::link);
            if state.pending_server_operation.is_none() {
                delete_button =
                    delete_button.on_press(Message::RequestConfirmDeleteServer(name.clone()));
            }

            // Per-server image cache management (beside the other whole-server action).
            let cache_usage = text(match state.image_cache_usage {
                Some(bytes) => t!("server-cached-images", "size" => format_bytes(bytes)),
                None => t!("server-cached-images-pending"),
            })
            .size(13)
            .style(builtins::text::muted);
            let clear_cache_button = button(text(t!("server-clear-image-cache")))
                .style(builtins::button::link)
                .on_press(Message::RequestClearImageCache(expected.clone()));

            smudgy_ui_shared::connect_modal::form(
                text(t!("server-edit")).size(Pixels(22.0)).into(),
                vec![
                    name_field.into(),
                    wss_field(state),
                    if state.server_form_data.wss_url.trim().is_empty() {
                        host_field
                    } else {
                        horizontal_space().into()
                    },
                    if state.server_form_data.wss_url.trim().is_empty() {
                        port_field
                    } else {
                        horizontal_space().into()
                    },
                    encoding_field(state),
                    compression_field(state),
                    if state.server_form_data.wss_url.trim().is_empty() {
                        tls_field(state)
                    } else {
                        horizontal_space().into()
                    },
                    server_error(state),
                ],
                Row::new()
                    .push(save_button)
                    .push(cancel_button)
                    .spacing(10)
                    .into(),
                Some(
                    column![
                        Row::new()
                            .push(cache_usage)
                            .push(clear_cache_button)
                            .spacing(10)
                            .align_y(iced::Alignment::Center),
                        delete_button,
                    ]
                    .spacing(15)
                    .into(),
                ),
            )
        }
        ServerCrudAction::ConfirmDelete(expected) => {
            // --- Delete Confirmation ---
            let name = &expected.name;
            let confirmation_text =
                text(t!("server-confirm-delete", "name" => name)).size(Pixels(16.0));

            let mut confirm_delete_button = button(text(t!("server-confirm-delete-action")))
                .style(builtins::button::secondary)
                .padding([8, 18]);
            if state.pending_server_operation.is_none() {
                confirm_delete_button =
                    confirm_delete_button.on_press(Message::ConfirmDeleteServer);
            }
            let cancel_delete_button = button(text(t!("action-cancel")))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::CancelServerForm);

            smudgy_ui_shared::connect_modal::form(
                text(t!("server-delete")).size(Pixels(22.0)).into(),
                vec![confirmation_text.into(), server_error(state)],
                Row::new()
                    .push(confirm_delete_button)
                    .push(cancel_delete_button)
                    .spacing(10)
                    .into(),
                None,
            )
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
