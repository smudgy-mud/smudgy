//! Settings-tab activation scope and runtime parameter controls.

use super::super::packages::{ProfileChoice, is_secret_string, secret_field_row};
use super::super::param_values::{self, ParamTarget};
use super::*;
use iced::widget::column;
use smudgy_core::models::shared_packages::ParameterScope;

impl AutomationsWindow {
    pub(super) fn parameter_scope_control<'a>(&self, locked: &LockedPackage) -> Elem<'a> {
        let scope = locked.parameter_scope;
        let package_lock = SharedPackageLock {
            packages: self.installed_packages.clone(),
        };
        let config_available = self
            .param_config
            .as_ref()
            .is_some_and(|config| config.specifier == locked.specifier && config.available);
        let scope_editable =
            self.package_state_available() && self.profile_inventory_complete && config_available;
        let scope_controls: Elem<'a> = if scope_editable {
            row![
                radio(
                    crate::i18n::t!("package-parameter-global"),
                    ParameterScope::Global,
                    Some(scope),
                    Message::SetParameterScope,
                ),
                radio(
                    crate::i18n::t!("package-parameter-profile"),
                    ParameterScope::Profile,
                    Some(scope),
                    Message::SetParameterScope,
                ),
            ]
            .spacing(16.0)
            .into()
        } else {
            row![
                text(format!(
                    "{} {}",
                    if scope == ParameterScope::Global {
                        "●"
                    } else {
                        "○"
                    },
                    crate::i18n::t!("package-parameter-global")
                ))
                .size(12.0)
                .style(common::muted),
                text(format!(
                    "{} {}",
                    if scope == ParameterScope::Profile {
                        "●"
                    } else {
                        "○"
                    },
                    crate::i18n::t!("package-parameter-profile")
                ))
                .size(12.0)
                .style(common::muted),
            ]
            .spacing(16.0)
            .into()
        };
        let mut control = column![
            common::section_label(crate::i18n::ts!("package-parameter-values")),
            scope_controls,
        ]
        .spacing(8.0);
        if let Some(error) = self.package_state_error() {
            control = control.push(text(error).size(12.0).style(common::danger));
        } else if !config_available {
            control = control.push(
                text(crate::i18n::t!("package-settings-read-unavailable-generic"))
                    .size(12.0)
                    .style(common::danger),
            );
        } else if !self.profile_inventory_complete {
            control = control.push(
                text(crate::i18n::t!("activation-profile-inventory-error"))
                    .size(12.0)
                    .style(common::danger),
            );
        }
        if scope_editable && scope == ParameterScope::Profile {
            let profile_choices = self
                .profile_names
                .iter()
                .map(|profile| ProfileChoice {
                    key: profile.clone(),
                    label: profile.clone(),
                })
                .collect::<Vec<_>>();
            let selected_profile = profile_choices
                .iter()
                .find(|choice| choice.key == self.parameter_profile)
                .cloned();
            control = control.push(
                row![
                    text(crate::i18n::t!("package-parameter-profile-label"))
                        .size(12.0)
                        .style(common::muted),
                    pick_list(profile_choices, selected_profile, |choice| {
                        Message::SelectParameterProfile(choice.key)
                    },),
                ]
                .spacing(10.0)
                .align_y(Vertical::Center),
            );
            if self.confirm_global_parameter_source {
                control = control.push(
                    container(
                        column![
                            text(crate::i18n::t!("package-global-source-needed")).size(12.0),
                            text(crate::i18n::t!(
                                "package-global-source-selected",
                                "profile" => &self.parameter_profile
                            ))
                            .size(12.0)
                            .style(common::muted),
                            row![
                                button(text(crate::i18n::t!("action-cancel")).size(12.0))
                                    .style(button_style::secondary)
                                    .on_press(Message::CancelGlobalParameterSource),
                                button(
                                    text(crate::i18n::t!("package-use-profile-globally"))
                                        .size(12.0)
                                )
                                .style(button_style::primary)
                                .on_press(Message::ConfirmGlobalParameterSource),
                            ]
                            .spacing(8.0),
                        ]
                        .spacing(6.0),
                    )
                    .padding(10.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
                );
            }
            if let Some(config) = self
                .param_config
                .as_ref()
                .filter(|config| config.specifier == locked.specifier)
            {
                // Completeness comes from the model's cache (`sync_profile_param_status`), never
                // from parameter storage: this runs on every redraw. A profile the cache does not
                // cover is reported the way an unreadable store is, with every required key
                // missing, rather than as ready. Only profiles where the package runs and a
                // required value is missing are listed; a clean package shows no section.
                let status = self
                    .profile_param_status
                    .as_ref()
                    .filter(|status| status.specifier == locked.specifier);
                let required_keys = config
                    .params
                    .iter()
                    .filter(|param| param.required)
                    .map(|param| param.key.clone())
                    .collect::<Vec<_>>();
                let problems = self
                    .profile_names
                    .iter()
                    .filter(|profile| {
                        package_lock.is_effectively_enabled_for(&locked.specifier, profile)
                    })
                    .filter_map(|profile| {
                        let missing = status
                            .and_then(|status| status.missing_for(profile))
                            .unwrap_or(&required_keys);
                        (!missing.is_empty()).then(|| (profile, missing))
                    })
                    .collect::<Vec<_>>();
                if !problems.is_empty() {
                    control = control.push(common::section_label(crate::i18n::ts!(
                        "package-parameter-profile-status"
                    )));
                }
                for (profile, missing) in problems {
                    let selected = profile == &self.parameter_profile;
                    control = control.push(
                        button(
                            row![
                                text(profile.clone()).size(12.0).width(Length::Fill),
                                text(crate::i18n::t!(
                                    "package-parameter-missing",
                                    "params" => missing.join(", ")
                                ))
                                .size(11.0)
                                .style(common::muted),
                            ]
                            .align_y(Vertical::Center),
                        )
                        .width(Length::Fill)
                        .style(if selected {
                            button_style::primary
                        } else {
                            button_style::secondary
                        })
                        .on_press(Message::SelectParameterProfile(profile.clone())),
                    );
                }
            }
        }
        control.into()
    }

    pub(super) fn view_param_config_section<'a>(&'a self, specifier: &str) -> Option<Elem<'a>> {
        let config = self
            .param_config
            .as_ref()
            .filter(|c| c.specifier == specifier)?;

        let unavailable = if !config.available {
            config
                .error
                .clone()
                .or_else(|| Some(crate::i18n::t!("package-settings-read-unavailable-generic")))
        } else if let Some(error) = self.package_state_error() {
            Some(error)
        } else if config.parameter_scope == ParameterScope::Profile
            && !self.profile_inventory_complete
        {
            Some(crate::i18n::t!("activation-profile-inventory-error"))
        } else {
            None
        };
        if let Some(error) = unavailable {
            return Some(
                container(
                    column![
                        text(crate::i18n::t!("package-runtime-settings-help"))
                            .size(12.0)
                            .style(common::muted),
                        text(error).size(12.0).style(common::danger),
                    ]
                    .spacing(10.0),
                )
                .padding(16.0)
                .width(Length::Fill)
                .style(common::card_style)
                .into(),
            );
        }

        let mut form = Column::new().spacing(10.0).push(
            text(crate::i18n::t!("package-runtime-settings-help"))
                .size(12.0)
                .style(common::muted),
        );

        for param in &config.params {
            let state = config.values.get(&param.key);
            let field = if is_secret_string(param) {
                let stored = config.secret_stored.contains(&param.key);
                let placeholder = if stored {
                    crate::i18n::ts!("package-secret-stored-placeholder")
                } else {
                    crate::i18n::ts!("package-secret-placeholder")
                };
                // A stored secret can only be replaced through the box (never revealed), so offer an
                // explicit Clear — the one way to unset it.
                let clear = stored.then(|| Message::ParamConfigClearSecret(param.key.clone()));
                secret_field_row(param, state, ParamTarget::Config, placeholder, clear)
            } else if let Some(state) = state {
                param_values::view(param, state, ParamTarget::Config)
            } else {
                continue;
            };
            form = form.push(field);
        }

        if let Some(error) = &config.error {
            form = form.push(text(error.clone()).size(12.0).style(common::danger));
        } else if config.saved {
            form = form.push(
                text(crate::i18n::t!("package-saved"))
                    .size(12.0)
                    .style(common::accent),
            );
        }

        let mut actions = row![iced::widget::space::horizontal()]
            .spacing(8.0)
            .align_y(Vertical::Center);
        if config.parameter_scope == ParameterScope::Profile && self.profile_names.len() > 1 {
            actions = actions.push(
                button(text(crate::i18n::t!("package-copy-settings")).size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::OpenCopySettings),
            );
        }
        actions = actions.push(
            button(text(crate::i18n::t!("package-save-settings")).size(12.0))
                .style(button_style::primary)
                .on_press(Message::ParamConfigSave),
        );
        form = form.push(actions);

        Some(
            container(form)
                .padding(16.0)
                .width(Length::Fill)
                .style(common::card_style)
                .into(),
        )
    }
}
