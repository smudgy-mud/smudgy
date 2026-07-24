//! Profile CRUD: async wrappers, form submission handling, and profile-side views.

use iced::font::Weight;

use iced::widget::{
    Column, Row, TextInput, button, column, container, scrollable,
    space::{horizontal as horizontal_space, vertical as vertical_space},
    text, text_editor,
};
use iced::{Alignment, Font, Length, Padding, Pixels, Task};
use log::warn;
use validator::Validate;
use crate::i18n::{t, ts};

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::Element;
use crate::theme::builtins;

use smudgy_core::models::profile::{Profile, ProfileConfig, contains_password_token};

use super::{
    Message, ProfileCrudAction, ProfileFormField, ProfileName, ServerName, State,
    profile_description_input_id, profile_name_input_id, profile_password_input_id,
    profile_send_on_connect_id,
};

// --- Profile CRUD Async Wrappers ---

pub(super) async fn create_profile_async(
    server_name: String,
    profile_name: String,
    config: smudgy_core::models::profile::ProfileConfig,
) -> Result<smudgy_core::models::profile::Profile, String> {
    smudgy_core::models::profile::create_profile(&server_name, &profile_name, config)
        .map_err(|e| e.to_string())
}

pub(super) async fn update_profile_async(
    server_name: String,
    profile_name: String,
    config: smudgy_core::models::profile::ProfileConfig,
) -> Result<smudgy_core::models::profile::Profile, String> {
    smudgy_core::models::profile::update_profile(&server_name, &profile_name, config)
        .map_err(|e| e.to_string())
}

pub(super) async fn delete_profile_async(
    server_name: String,
    profile_name: String,
) -> Result<(ServerName, ProfileName), String> {
    smudgy_core::models::profile::delete_profile(&server_name, &profile_name)
        .map(|_| (server_name, profile_name)) // Return tuple on success
        .map_err(|e| e.to_string())
}

// --- Profile Loaders ---

pub(super) async fn load_profiles_async(server_name: String) -> Result<Vec<Profile>, String> {
    smudgy_core::models::profile::list_profiles(&server_name).map_err(|e| e.to_string())
}

// --- Update Logic ---

/// Helper function to handle profile form submission.
pub(super) fn handle_submit_profile_form(state: &mut State) -> Task<Message> {
    state.profile_crud_error = None; // Clear previous error

    let server_name = if let Some(name) = state.selected_server.clone() {
        name
    } else {
        warn!("Error: SubmitProfileForm called without a server selected.");
        state.profile_crud_error = Some(t!("profile-error-no-server"));
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

    match state.profile_action.clone() {
        Some(ProfileCrudAction::Create) => {
            let config = ProfileConfig {
                caption: state.profile_form_data.description.trim().to_string(),
                send_on_connect: state.profile_form_send_on_connect_content.text(),
            };
            if let Err(e) = config.validate() {
                state.profile_crud_error = Some(t!("profile-error-config", "error" => e.to_string()));
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
            Task::perform(
                create_profile_async(server_name, profile_name, config),
                Message::ProfileCreated,
            )
        }
        Some(ProfileCrudAction::Edit(original_profile_name)) => {
            let config = ProfileConfig {
                caption: state.profile_form_data.description.trim().to_string(),
                send_on_connect: state.profile_form_send_on_connect_content.text(),
            };
            if let Err(e) = config.validate() {
                state.profile_crud_error = Some(t!("profile-error-config", "error" => e.to_string()));
                return Task::none();
            }
            Task::perform(
                update_profile_async(server_name, original_profile_name, config),
                Message::ProfileUpdated,
            )
        }
        Some(ProfileCrudAction::ConfirmDelete(_)) => {
            warn!("Error: SubmitProfileForm called during ConfirmDelete state.");
            state.profile_crud_error = Some(t!("profile-error-action-delete"));
            Task::none()
        }
        None => {
            warn!("Error: SubmitProfileForm called without a profile action set.");
            state.profile_crud_error = Some(t!("profile-error-action-missing"));
            Task::none()
        }
    }
}

// --- View Logic ---

/// Renders the profile create/edit form, or the delete confirmation.
pub(super) fn view_profile_form<'a>(
    state: &'a State,
    action: &'a ProfileCrudAction,
) -> Element<'a, Message> {
    match action {
        ProfileCrudAction::Create => {
            let name_field = column![
                field_label(t!("profile-name")),
                TextInput::new(ts!("profile-name-placeholder"), &state.profile_form_data.name)
                    .id(profile_name_input_id())
                    .on_input(|val| Message::UpdateProfileFormField(ProfileFormField::Name, val))
                    .on_submit(Message::SubmitProfileForm),
            ]
            .spacing(4);

            let save_button = button(text(t!("profile-create")))
                .style(builtins::button::primary)
                .padding([8, 18])
                .on_press(Message::SubmitProfileForm);
            let cancel_button = button(text(t!("action-cancel")))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::CancelProfileForm);

            Column::new()
                .push(form_title(t!("profile-add"), state))
                .push(name_field)
                .push(description_field(state))
                .push(on_connect_field(state))
                .push(profile_error(state))
                .push(Row::new().push(save_button).push(cancel_button).spacing(10))
                .spacing(15)
                .into()
        }
        ProfileCrudAction::Edit(name) => {
            // Name is the profile key (rename isn't supported by the backend), so
            // it is shown read-only.
            let name_field = column![
                field_label(t!("profile-name")),
                text(name).size(Pixels(16.0)),
            ]
            .spacing(4);

            let save_button = button(text(t!("action-save")))
                .style(builtins::button::primary)
                .padding([8, 18])
                .on_press(Message::SubmitProfileForm);
            let cancel_button = button(text(t!("action-cancel")))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::CancelProfileForm);
            let delete_button = button(text(t!("profile-delete")))
                .style(builtins::button::link)
                .on_press(Message::RequestConfirmDeleteProfile(name.clone()));

            Column::new()
                .push(form_title(t!("profile-edit"), state))
                .push(name_field)
                .push(description_field(state))
                .push(on_connect_field(state))
                .push(profile_error(state))
                .push(Row::new().push(save_button).push(cancel_button).spacing(10))
                .push(vertical_space().height(Pixels(10.0)))
                .push(delete_button)
                .spacing(15)
                .into()
        }
        ProfileCrudAction::ConfirmDelete(name) => {
            let confirmation_text =
                text(t!("profile-confirm-delete", "name" => name)).size(Pixels(16.0));

            let confirm_delete_button = button(text(t!("profile-confirm-delete-action")))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::ConfirmDeleteProfile(name.clone()));
            let cancel_delete_button = button(text(t!("action-cancel")))
                .style(builtins::button::secondary)
                .padding([8, 18])
                .on_press(Message::CancelProfileForm);

            Column::new()
                .push(text(t!("profile-delete")).size(Pixels(22.0)))
                .push(confirmation_text)
                .push(profile_error(state))
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

/// A muted field label rendered above its input.
fn field_label(label: String) -> Element<'static, Message> {
    text(label).size(13).style(builtins::text::muted).into()
}

/// Form title: `{verb} · {server}`, with the owning server name muted.
fn form_title<'a>(verb: String, state: &'a State) -> Element<'a, Message> {
    let server = state.selected_server.as_deref().unwrap_or_default();
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
        TextInput::new(ts!("profile-description-placeholder"), &state.profile_form_data.description)
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
        Row::new()
            .push(text(t!("profile-password-saved")).size(12).style(builtins::text::muted))
            .push(horizontal_space())
            .push(
                button(text(t!("profile-password-change")).size(12))
                    .style(builtins::button::link)
                    .padding([2, 8])
                    .on_press(Message::RequestChangeProfilePassword),
            )
            .push(
                button(text(t!("profile-password-clear")).size(12))
                    .style(builtins::button::link)
                    .padding([2, 8])
                    .on_press(Message::ClearProfilePassword),
            )
            .spacing(8)
            .align_y(Alignment::Center)
            .into()
    } else {
        column![
            field_label(t!("profile-password-label")),
            TextInput::new(ts!("profile-password-placeholder"), &state.profile_form_password)
                .secure(true)
                .id(profile_password_input_id())
                .on_input(Message::UpdateProfileFormPassword)
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
    server_name: &'a ServerName,
) -> Element<'a, Message> {
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
    .on_press(Message::RequestEditServer(server_name.clone()));

    let title_row = Row::new()
        .push(text(server_name).size(Pixels(24.0)))
        .push(horizontal_space())
        .push(edit_action)
        .align_y(Alignment::Center);

    // Address: host : port together, in mono.
    let address: Element<Message> = if let Some(server) = server_details {
        text(format!("{} : {}", server.config.host, server.config.port))
            .font(fonts::GEIST_MONO_VF)
            .size(13)
            .style(builtins::text::muted)
            .into()
    } else {
        text(t!("server-details-missing"))
            .style(builtins::text::danger)
            .into()
    };

    let profiles = state.profiles.get(server_name);
    let is_loading_p = state.is_loading_profiles.as_ref() == Some(server_name);

    let profile_list_content: Element<Message> = match (profiles, is_loading_p) {
        // Render nothing while a (rare) async profile load is in flight — the modal
        // preloads the first server's profiles, so this only happens briefly when
        // switching to another server, and a blank beat reads quieter than a
        // "Loading profiles…" flash.
        (_, true) => Column::new().into(),
        (Some(profiles), false) if profiles.is_empty() => view_empty_profiles(),
        (Some(profiles), false) => profiles
            .iter()
            .fold(Column::new().spacing(10), |col, profile| {
                col.push(profile_row(server_name, profile))
            })
            .into(),
        (None, false) => {
            column![
                text(t!("profiles-load-error")).style(builtins::text::danger)
            ]
            .into()
        }
    };

    let helper = text(t!("profiles-saved-help"))
        .size(12)
        .style(builtins::text::muted);

    let mut content_col = Column::new()
        .push(title_row)
        .push(address)
        .push(vertical_space().height(Pixels(4.0)))
        .push(text(t!("profiles-title")).size(Pixels(18.0)))
        .push(helper)
        // Right padding keeps the rows' trailing `Connect`/`Offline` buttons clear
        // of the overlaid vertical scrollbar that appears once the list overflows.
        .push(
            scrollable(container(profile_list_content).padding(Padding::ZERO.right(14)))
                .height(Length::FillPortion(1)),
        )
        .spacing(12);

    // Footer "+ New Profile" — only when profiles exist; the empty state carries
    // its own primary CTA (one primary action per view).
    if profiles.is_some_and(|p| !p.is_empty()) {
        content_col = content_col.push(
            button(text(t!("profiles-new")))
                .width(Length::Fill)
                .padding([6, 10])
                .style(builtins::button::secondary)
                .on_press(Message::RequestCreateProfile),
        );
    }

    content_col.into()
}

/// A single profile row: bold name + muted description (left, filling), a quiet
/// pencil edit icon, then the primary `Connect` action at the end.
fn profile_row<'a>(server_name: &'a ServerName, profile: &'a Profile) -> Element<'a, Message> {
    let mut name_col = Column::new()
        .push(text(&profile.name).font(Font {
            weight: Weight::Bold,
            ..fonts::GEIST_VF
        }))
        .spacing(2);
    if !profile.config.caption.is_empty() {
        name_col = name_col.push(
            text(&profile.config.caption)
                .size(12)
                .style(builtins::text::muted),
        );
    }

    let edit_icon = button(
        text(bootstrap_icons::PENCIL)
            .font(fonts::BOOTSTRAP_ICONS)
            .size(13),
    )
    .style(builtins::button::link)
    .padding([2, 6])
    .on_press(Message::RequestEditProfile(profile.name.clone()));

    let connect_button = button(text(t!("profile-connect")))
        .style(builtins::button::primary)
        .padding([6, 16])
        .on_press(Message::ConnectProfile(
            server_name.clone(),
            profile.name.clone(),
        ));

    // Open the session without connecting (map editor / automations offline).
    let open_offline_button = button(text(t!("profile-offline")))
        .style(builtins::button::subtle)
        .padding([6, 16])
        .on_press(Message::OpenOfflineProfile(
            server_name.clone(),
            profile.name.clone(),
        ));

    Row::new()
        .push(name_col.width(Length::Fill))
        .push(edit_icon)
        .push(connect_button)
        .push(open_offline_button)
        .spacing(10)
        .align_y(Alignment::Center)
        .into()
}

/// Empty-state for a server with no profiles yet.
fn view_empty_profiles() -> Element<'static, Message> {
    container(
        column![
            text(t!("profiles-empty")).size(Pixels(16.0)),
            button(text(t!("profiles-new")))
                .padding([8, 18])
                .style(builtins::button::primary)
                .on_press(Message::RequestCreateProfile),
        ]
        .spacing(12)
        .align_x(Alignment::Center),
    )
    .width(Length::Fill)
    .padding(20)
    .center_x(Length::Fill)
    .into()
}
