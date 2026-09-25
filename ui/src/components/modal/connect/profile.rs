//! Profile CRUD: async wrappers, form submission handling, and profile-side views.

use crate::i18n::{t, ts};
use iced::widget::{
    Column, Row, TextInput, button, column, container, space::horizontal as horizontal_space, text,
    text_editor,
};
use iced::{Alignment, Length, Pixels, Task};
use log::warn;
use validator::Validate;

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::Element;
use crate::theme::builtins;

use smudgy_core::models::profile::{
    Profile, ProfileCas, ProfileConfig, clear_profile_password_if_unchanged,
    contains_password_token, create_profile_if_server_unchanged, delete_profile_if_unchanged,
    set_profile_password_if_unchanged, update_profile_if_unchanged,
};
use smudgy_core::models::server::Server;
use smudgy_core::models::server::{self as server_model, ServerCas};

use super::{
    AppliedProfileOperation, Message, ProfileCrudAction, ProfileFormField, ProfileOperationAction,
    ProfileOperationCompletion, ProfileOperationEnvelope, ProfileOperationError,
    ProfilePasswordAction, ProfilePasswordWarning, ServerName, State, next_profile_operation_id,
    profile_description_input_id, profile_name_input_id, profile_password_input_id,
    profile_send_on_connect_id,
};

// --- Profile CRUD Async Wrappers ---

pub(super) async fn execute_profile_operation(
    envelope: ProfileOperationEnvelope,
) -> ProfileOperationCompletion {
    let key = envelope.key();
    let result = execute_profile_operation_inner(&envelope);
    ProfileOperationCompletion { key, result }
}

fn execute_profile_operation_inner(
    envelope: &ProfileOperationEnvelope,
) -> Result<AppliedProfileOperation, ProfileOperationError> {
    let failed = |error: anyhow::Error| ProfileOperationError::Failed(error.to_string());
    let applied = match &envelope.action {
        ProfileOperationAction::Create {
            target_name,
            config,
        } => {
            let profile = match create_profile_if_server_unchanged(
                &envelope.server,
                target_name,
                config.clone(),
            )
            .map_err(failed)?
            {
                ProfileCas::Applied(profile) => profile,
                ProfileCas::StateChanged => return Err(ProfileOperationError::StateChanged),
            };
            let password_warning = apply_password_action(envelope, &profile);
            AppliedProfileOperation::Created(profile, password_warning)
        }
        ProfileOperationAction::Update {
            expected, config, ..
        } => {
            match update_profile_if_unchanged(&envelope.server.name, expected, config.clone())
                .map_err(failed)?
            {
                ProfileCas::Applied(profile) => {
                    let password_warning = apply_password_action(envelope, &profile);
                    AppliedProfileOperation::Updated(profile, password_warning)
                }
                ProfileCas::StateChanged => return Err(ProfileOperationError::StateChanged),
            }
        }
        ProfileOperationAction::Delete { expected, .. } => {
            match delete_profile_if_unchanged(&envelope.server.name, expected).map_err(failed)? {
                ProfileCas::Applied(()) => AppliedProfileOperation::Deleted,
                ProfileCas::StateChanged => return Err(ProfileOperationError::StateChanged),
            }
        }
    };
    Ok(applied)
}

fn apply_password_action(
    envelope: &ProfileOperationEnvelope,
    profile: &Profile,
) -> Option<ProfilePasswordWarning> {
    let result = match &envelope.password {
        ProfilePasswordAction::Keep => return None,
        ProfilePasswordAction::Set(password) => {
            set_profile_password_if_unchanged(&envelope.server.name, profile, password)
        }
        ProfilePasswordAction::Clear => {
            clear_profile_password_if_unchanged(&envelope.server.name, profile)
        }
    };
    match result {
        Ok(ProfileCas::Applied(())) => None,
        Ok(ProfileCas::StateChanged) => {
            warn!(
                "The profile changed before its captured auto-login password action could be applied"
            );
            Some(ProfilePasswordWarning::StateChanged)
        }
        Err(error) => {
            warn!(
                "Failed to apply the captured auto-login password action for '{}/{}': {error}",
                envelope.server.name, profile.name
            );
            Some(ProfilePasswordWarning::Failed(error.to_string()))
        }
    }
}

// --- Profile Loaders ---

pub(super) async fn load_profiles_async(server: Server) -> Result<ServerCas<Vec<Profile>>, String> {
    server_model::with_server_if_unchanged(&server, |current| {
        smudgy_core::models::profile::list_profiles(&current.name)
    })
    .map_err(|error| error.to_string())
}

// --- Update Logic ---

/// Helper function to handle profile form submission.
pub(super) fn handle_submit_profile_form(state: &mut State) -> Task<Message> {
    state.profile_crud_error = None; // Clear previous error

    if state.pending_profile_operation.is_some() {
        return Task::none();
    }

    let Some(action) = state.profile_action.clone() else {
        warn!("Error: SubmitProfileForm called without a profile action set.");
        state.profile_crud_error = Some(t!("profile-error-action-missing"));
        return Task::none();
    };

    // If the auto-login text uses $PASSWORD, a password must be available — either
    // freshly typed or already stored — before we save (so the token is never sent
    // with nothing behind it).
    if contains_password_token(&state.profile_form_send_on_connect_content.text())
        && state.profile_form_password.trim().is_empty()
        && !state.profile_form_password_stored
    {
        state.profile_crud_error = Some(t!("profile-error-password-required"));
        return Task::none();
    }

    let (server, operation_action) = match action {
        ProfileCrudAction::Create { server } => {
            let config = ProfileConfig {
                caption: state.profile_form_data.description.trim().to_string(),
                send_on_connect: state.profile_form_send_on_connect_content.text(),
            };
            if let Err(e) = config.validate() {
                state.profile_crud_error =
                    Some(t!("profile-error-config", "error" => e.to_string()));
                return Task::none();
            }
            let profile_name = state.profile_form_data.name.trim().to_string();
            if profile_name.is_empty() {
                state.profile_crud_error = Some(t!("profile-error-name-empty"));
                return Task::none();
            }
            if profile_name.contains(|c: char| !c.is_alphanumeric() && c != '_' && c != '-') {
                state.profile_crud_error = Some(t!("profile-error-name-format"));
                return Task::none();
            }
            (
                server,
                ProfileOperationAction::Create {
                    target_name: profile_name,
                    config,
                },
            )
        }
        ProfileCrudAction::Edit { server, expected } => {
            let config = ProfileConfig {
                caption: state.profile_form_data.description.trim().to_string(),
                send_on_connect: state.profile_form_send_on_connect_content.text(),
            };
            if let Err(e) = config.validate() {
                state.profile_crud_error =
                    Some(t!("profile-error-config", "error" => e.to_string()));
                return Task::none();
            }
            let target_name = expected.name.clone();
            (
                server,
                ProfileOperationAction::Update {
                    expected,
                    target_name,
                    config,
                },
            )
        }
        ProfileCrudAction::ConfirmDelete { .. } => {
            warn!("Error: SubmitProfileForm called during ConfirmDelete state.");
            state.profile_crud_error = Some(t!("profile-error-action-delete"));
            return Task::none();
        }
    };

    let send_on_connect = match &operation_action {
        ProfileOperationAction::Create { config, .. }
        | ProfileOperationAction::Update { config, .. } => &config.send_on_connect,
        ProfileOperationAction::Delete { .. } => unreachable!("form submit cannot delete"),
    };
    let password = captured_password_action(state, send_on_connect);
    start_profile_operation(state, server, operation_action, password)
}

pub(super) fn handle_delete_profile(state: &mut State) -> Task<Message> {
    state.profile_crud_error = None;
    if state.pending_profile_operation.is_some() {
        return Task::none();
    }
    let Some(ProfileCrudAction::ConfirmDelete { server, expected }) = state.profile_action.clone()
    else {
        warn!("Error: ConfirmDeleteProfile called without an exact delete target.");
        state.profile_crud_error = Some(t!("profile-error-delete-no-server"));
        return Task::none();
    };
    let target_name = expected.name.clone();
    start_profile_operation(
        state,
        server,
        ProfileOperationAction::Delete {
            expected,
            target_name,
        },
        ProfilePasswordAction::Clear,
    )
}

fn captured_password_action(state: &State, send_on_connect: &str) -> ProfilePasswordAction {
    if !contains_password_token(send_on_connect) {
        ProfilePasswordAction::Clear
    } else if state.profile_form_password.trim().is_empty() {
        ProfilePasswordAction::Keep
    } else {
        ProfilePasswordAction::Set(state.profile_form_password.clone())
    }
}

fn start_profile_operation(
    state: &mut State,
    server: Server,
    action: ProfileOperationAction,
    password: ProfilePasswordAction,
) -> Task<Message> {
    let envelope = ProfileOperationEnvelope {
        id: next_profile_operation_id(state),
        server,
        action,
        password,
        form_revision: state.profile_form_revision,
    };
    state.pending_profile_operation = Some(envelope.clone());
    Task::perform(execute_profile_operation(envelope), |completion| {
        Message::ProfileOperationFinished(completion)
    })
}

// --- View Logic ---

/// Renders the profile create/edit form, or the delete confirmation.
pub(super) fn view_profile_form<'a>(
    state: &'a State,
    action: &'a ProfileCrudAction,
) -> Element<'a, Message> {
    match action {
        ProfileCrudAction::Create { .. } => {
            let name_field = column![
                field_label(t!("profile-name")),
                TextInput::new(
                    ts!("profile-name-placeholder"),
                    &state.profile_form_data.name
                )
                .id(profile_name_input_id())
                .on_input(|val| Message::UpdateProfileFormField(ProfileFormField::Name, val))
                .on_submit(Message::SubmitProfileForm),
            ]
            .spacing(4);

            let save_button = button(text(t!("profile-create")))
                .style(builtins::button::primary)
                .padding([8, 18]);
            let save_button = if state.pending_profile_operation.is_some() {
                save_button
            } else {
                save_button.on_press(Message::SubmitProfileForm)
            };
            let cancel_button = button(text(t!("action-cancel")))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::CancelProfileForm);

            smudgy_ui_shared::connect_modal::form(
                form_title(t!("profile-add"), action.server_name()),
                vec![
                    name_field.into(),
                    description_field(state),
                    on_connect_field(state),
                    profile_error(state),
                ],
                Row::new()
                    .push(save_button)
                    .push(cancel_button)
                    .spacing(10)
                    .into(),
                None,
            )
        }
        ProfileCrudAction::Edit { expected, .. } => {
            // Name is the profile key (rename isn't supported by the backend), so
            // it is shown read-only.
            let name_field = column![
                field_label(t!("profile-name")),
                text(&expected.name).size(Pixels(16.0)),
            ]
            .spacing(4);

            let save_button = button(text(t!("action-save")))
                .style(builtins::button::primary)
                .padding([8, 18]);
            let save_button = if state.pending_profile_operation.is_some() {
                save_button
            } else {
                save_button.on_press(Message::SubmitProfileForm)
            };
            let cancel_button = button(text(t!("action-cancel")))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::CancelProfileForm);
            let delete_button = button(text(t!("profile-delete"))).style(builtins::button::link);
            let delete_button = if state.pending_profile_operation.is_some() {
                delete_button
            } else {
                delete_button.on_press(Message::RequestConfirmDeleteProfile)
            };

            smudgy_ui_shared::connect_modal::form(
                form_title(t!("profile-edit"), action.server_name()),
                vec![
                    name_field.into(),
                    description_field(state),
                    on_connect_field(state),
                    profile_error(state),
                ],
                Row::new()
                    .push(save_button)
                    .push(cancel_button)
                    .spacing(10)
                    .into(),
                Some(delete_button.into()),
            )
        }
        ProfileCrudAction::ConfirmDelete { expected, .. } => {
            let confirmation_text =
                text(t!("profile-confirm-delete", "name" => &expected.name)).size(Pixels(16.0));

            let confirm_delete_button = button(text(t!("profile-confirm-delete-action")))
                .style(builtins::button::secondary)
                .padding([8, 18]);
            let confirm_delete_button = if state.pending_profile_operation.is_some() {
                confirm_delete_button
            } else {
                confirm_delete_button.on_press(Message::ConfirmDeleteProfile)
            };
            let cancel_delete_button = button(text(t!("action-cancel")))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::CancelProfileForm);

            smudgy_ui_shared::connect_modal::form(
                text(t!("profile-delete")).size(Pixels(22.0)).into(),
                vec![confirmation_text.into(), profile_error(state)],
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

/// A muted field label rendered above its input.
fn field_label(label: String) -> Element<'static, Message> {
    text(label).size(13).style(builtins::text::muted).into()
}

/// Form title: `{verb} · {server}`, with the owning server name muted.
fn form_title(verb: String, server: &str) -> Element<'_, Message> {
    Row::new()
        .push(text(verb).size(Pixels(22.0)))
        .push(
            text(format!(" · {server}"))
                .size(Pixels(22.0))
                .style(builtins::text::muted),
        )
        .align_y(Alignment::Center)
        .into()
}

/// The optional description field.
fn description_field(state: &State) -> Element<'_, Message> {
    column![
        field_label(t!("profile-description")),
        TextInput::new(
            ts!("profile-description-placeholder"),
            &state.profile_form_data.description
        )
        .id(profile_description_input_id())
        .on_input(|val| Message::UpdateProfileFormField(ProfileFormField::Description, val))
        .on_submit(Message::SubmitProfileForm),
    ]
    .spacing(4)
    .into()
}

/// The free-form multiline on-connect field plus the plain-text disclosure.
/// Intentionally free-form (no structured user/password) — there is no common
/// login structure across MUDs; the raw command + the warning is the design.
///
/// If the text embeds the `$PASSWORD` token, a password control appears below it:
/// the real password is kept in the OS keyring, not in the auto-login text, and
/// the disclosure is reworded to say so.
fn on_connect_field(state: &State) -> Element<'_, Message> {
    let uses_password = contains_password_token(&state.profile_form_send_on_connect_content.text());

    let notice_text = if uses_password {
        t!("profile-keychain-notice")
    } else {
        t!("profile-plaintext-notice")
    };
    let notice = container(
        Row::new()
            .push(
                text(bootstrap_icons::EXCLAMATION_TRIANGLE)
                    .font(fonts::BOOTSTRAP_ICONS)
                    .size(12),
            )
            .push(text(notice_text).size(12))
            .spacing(8)
            .align_y(Alignment::Center),
    )
    .width(Length::Fill)
    .padding([6, 10])
    .style(builtins::container::notice);

    let mut col = column![
        field_label(t!("profile-on-connect")),
        text_editor(&state.profile_form_send_on_connect_content)
            .id(profile_send_on_connect_id())
            .placeholder(ts!("profile-on-connect-example"))
            // ~7 lines tall (≈20px per line).
            .height(Length::Fixed(140.0))
            .font(fonts::GEIST_MONO_VF)
            .on_action(Message::UpdateProfileFormSendOnConnect),
        notice,
    ]
    .spacing(8);

    if uses_password {
        col = col.push(password_control(state));
    }
    col.into()
}

/// The auto-login password control, shown only when the on-connect text uses
/// `$PASSWORD`: a secure input to set or replace the password, or — once one is
/// stored — a "Password saved" chip with Change / Clear actions.
fn password_control(state: &State) -> Element<'_, Message> {
    if state.profile_form_password_stored && !state.profile_form_password_editing {
        let clear_button = button(text(t!("profile-password-clear")).size(12))
            .style(builtins::button::link)
            .padding([2, 8]);
        let clear_button = if state.pending_profile_operation.is_some() {
            clear_button
        } else {
            clear_button.on_press(Message::ClearProfilePassword)
        };
        Row::new()
            .push(
                text(t!("profile-password-saved"))
                    .size(12)
                    .style(builtins::text::muted),
            )
            .push(horizontal_space())
            .push(
                button(text(t!("profile-password-change")).size(12))
                    .style(builtins::button::link)
                    .padding([2, 8])
                    .on_press(Message::RequestChangeProfilePassword),
            )
            .push(clear_button)
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
    } else {
        column![
            field_label(t!("profile-password-label")),
            TextInput::new(
                ts!("profile-password-placeholder"),
                &state.profile_form_password
            )
            .secure(true)
            .id(profile_password_input_id())
            .on_input(|value| Message::UpdateProfileFormPassword(value.into()))
            .on_submit(Message::SubmitProfileForm),
        ]
        .spacing(4)
        .into()
    }
}

/// Renders the current profile-form error (if any), or an empty spacer.
fn profile_error(state: &State) -> Element<'_, Message> {
    match &state.profile_crud_error {
        Some(error) => text(error).style(builtins::text::danger).into(),
        None => horizontal_space().into(),
    }
}

/// Renders the server detail pane: server name + quiet inline edit, the address
/// in mono, then the `Profiles` section and its list / empty state.
pub(super) fn view_server_details_and_profiles<'a>(
    state: &'a State,
    model: smudgy_ui_shared::connect_model::SelectedServer<'a>,
) -> Element<'a, Message> {
    use smudgy_ui_shared::connect_model::Profiles;

    let server_name = state
        .selected_server
        .as_ref()
        .expect("server details have a selected server");
    let server_details = state.servers.iter().find(|s| s.name == *server_name);

    // Title: server name on the left, a quiet inline "✎ Edit" on the right.
    let edit_action = button(
        Row::new()
            .push(
                text(bootstrap_icons::PENCIL)
                    .font(fonts::BOOTSTRAP_ICONS)
                    .size(13),
            )
            .push(text(t!("server-edit-short")))
            .spacing(6)
            .align_y(Alignment::Center),
    )
    .style(builtins::button::link)
    .padding([2, 8])
    .on_press(Message::from(model.edit));

    // The advertised icon remains a native-only adornment in the common header.
    let icon = state.icons.get(server_name).map(|handle| {
        iced::widget::image(handle.clone())
            .width(Pixels(28.0))
            .height(Pixels(28.0))
            .into()
    });

    // Address: preserve the full WSS path/query when applicable.
    let address: Element<Message> = if let Some(server) = server_details {
        text(
            server
                .config
                .wss_url
                .clone()
                .unwrap_or_else(|| format!("{} : {}", server.config.host, server.config.port)),
        )
        .font(fonts::GEIST_MONO_VF)
        .size(13)
        .style(builtins::text::muted)
        .into()
    } else {
        text(t!("server-details-missing"))
            .style(builtins::text::danger)
            .into()
    };

    let profile_list_content: Element<Message> = match model.profiles {
        // Render nothing while a (rare) async profile load is in flight — the modal
        // preloads the first server's profiles, so this only happens briefly when
        // switching to another server, and a blank beat reads quieter than a
        // "Loading profiles…" flash.
        Profiles::Loading => Column::new().into(),
        Profiles::Ready(profiles) if profiles.is_empty() => view_empty_profiles(),
        Profiles::Ready(profiles) => profiles
            .into_iter()
            .fold(Column::new().spacing(10), |col, profile| {
                col.push(profile_row(server_name, profile))
            })
            .into(),
        Profiles::Unavailable => {
            column![text(t!("profiles-load-error")).style(builtins::text::danger)].into()
        }
    };

    // "Restore last session (Kapusnik, Kapusta)" — offered only while the
    // server's last-session snapshot exists and parses; the label carries
    // its stored profile names in slot order. The button labels itself, so
    // no separate accessible name is needed.
    let restore_last: Option<Element<Message>> = state
        .last_sessions
        .get(server_name)
        .and_then(|probed| probed.as_deref())
        .and_then(crate::workspace::last_session::summary)
        .map(|profiles| {
            button(text(t!("profiles-restore-last", "profiles" => profiles)))
                .width(Length::Fill)
                .padding([6, 10])
                .style(builtins::button::secondary)
                .on_press(Message::RestoreLastSession(server_name.clone()))
                .into()
        });

    let mut summary = Vec::new();
    // Observed metadata (the `observed.json` sidecar), all optional: the
    // game's own name as a subtitle, then the band under the address. A
    // server with no sidecar renders exactly as it always has.
    if let Some(subtitle) = super::observed::game_name_subtitle(state, server_name) {
        summary.push(subtitle);
    }
    summary.push(address);
    if let Some(band) =
        server_details.and_then(|server| super::observed::metadata_band(state, server))
    {
        summary.push(band);
    }
    if let Some(restore_last) = restore_last {
        summary.push(restore_last);
    }
    if let Some(intent) = model.quick_connect {
        summary.push(
            button(text(t!("profile-connect-default")))
                .width(Length::Fill)
                .padding([6, 10])
                .style(builtins::button::secondary)
                .on_press(Message::from(intent))
                .into(),
        );
    }
    smudgy_ui_shared::connect_modal::server_details(
        smudgy_ui_shared::connect_modal::ServerDetails {
            name: model.name,
            icon,
            edit: edit_action.into(),
            summary,
            profiles_title: t!("profiles-title"),
            profiles_help: t!("profiles-saved-help"),
            profiles: profile_list_content,
            // The empty state carries its own primary CTA.
            new_profile: model.new_profile.map(|intent| {
                smudgy_ui_shared::connect_modal::new_profile(
                    t!("profiles-new"),
                    Message::from(intent),
                )
            }),
        },
    )
}

/// A single profile row: bold name + muted description (left, filling), a quiet
/// pencil edit icon, then the primary `Connect` action at the end.
fn profile_row<'a>(
    server_name: &'a ServerName,
    profile: smudgy_ui_shared::connect_model::ProfileRow<'a>,
) -> Element<'a, Message> {
    let smudgy_ui_shared::connect_model::ProfileRow {
        name,
        caption,
        edit,
        connect,
    } = profile;
    let edit_icon = button(
        text(bootstrap_icons::PENCIL)
            .font(fonts::BOOTSTRAP_ICONS)
            .size(13),
    )
    .style(builtins::button::link)
    .padding([2, 6])
    .on_press(Message::from(edit));

    let connect_button = button(text(t!("profile-connect")))
        .style(builtins::button::primary)
        .padding([6, 16])
        .on_press(Message::from(connect));

    // Open the session without connecting (map editor / automations offline).
    let open_offline_button = button(text(t!("profile-offline")))
        .style(builtins::button::subtle)
        .padding([6, 16])
        .on_press(Message::OpenOfflineProfile(
            server_name.clone(),
            name.to_owned(),
        ));

    smudgy_ui_shared::connect_modal::profile_row(
        name,
        caption,
        vec![
            edit_icon.into(),
            connect_button.into(),
            open_offline_button.into(),
        ],
    )
}

/// Empty-state for a server with no profiles yet.
fn view_empty_profiles() -> Element<'static, Message> {
    smudgy_ui_shared::connect_modal::empty_profiles(
        t!("profiles-empty"),
        t!("profiles-new"),
        Message::from(smudgy_ui_shared::connect_model::Intent::NewProfile),
    )
}
