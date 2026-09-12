//! About-tab renderers for installed and local packages.

use super::super::InstalledReadmeState;
use super::super::packages::UpdateDelta;
use super::*;
use crate::components::permissions::{consent_can_row, full_access_banner, permission_can_lines};
use iced::widget::column;

impl AutomationsWindow {
    pub(super) fn dep_link_row<'a>(
        &self,
        specifier: &str,
        enabled: bool,
        prefix: &str,
        parent: Option<&str>,
        relation: Option<DepRelation>,
    ) -> Elem<'a> {
        let name = package_display_name(specifier).to_string();
        // A dependency has no enable state of its own — it loads because the package that requires
        // it is enabled — so its rows read "active/inactive". The user-controllable "enabled/
        // disabled" is reserved for the "Required by" parent rows (`relation` is `None`), which
        // are top-level packages the user actually toggles.
        let state = match (relation.is_some(), enabled) {
            (true, true) => crate::i18n::ts!("package-state-active"),
            (true, false) => crate::i18n::ts!("package-state-inactive"),
            (false, true) => crate::i18n::ts!("package-state-enabled"),
            (false, false) => crate::i18n::ts!("package-state-disabled"),
        };
        let mut content = row![
            common::status_dot(if enabled {
                NodeStatus::Ok
            } else {
                NodeStatus::Disabled
            }),
            text(name).size(13.0),
        ]
        .spacing(8.0)
        .align_y(Vertical::Center);
        if let Some(relation) = relation {
            content = content.push(match relation {
                DepRelation::Import => common::dep_tag(),
                DepRelation::Root => common::required_tag(),
                DepRelation::Both => common::dep_req_tag(),
            });
        }
        content = content
            .push(text(prefix.to_string()).size(12.0).style(common::muted))
            .push(iced::widget::space::horizontal())
            .push(text(state).size(11.0).style(common::faint))
            .push(text("\u{203A}").size(14.0).style(common::muted));
        let on_press = parent.map_or_else(
            || Message::SelectInstalledPackage(specifier.to_string()),
            |parent| Message::SelectDependency {
                parent: parent.to_string(),
                spec: specifier.to_string(),
            },
        );
        button(content)
            .style(button_style::list_item)
            .on_press(on_press)
            .width(Length::Fill)
            .into()
    }

    pub(super) fn installed_readme_view(&self) -> Elem<'_> {
        match &self.installed_readme {
            InstalledReadmeState::Loading => container(
                text(crate::i18n::t!("package-readme-loading"))
                    .size(13.0)
                    .style(common::muted),
            )
            .padding(10.0)
            .into(),
            InstalledReadmeState::Loaded(Some(readme)) => {
                let settings = markdown::Settings::with_text_size(
                    13.0,
                    markdown::Style::from_palette(iced::theme::Palette::DARK),
                );
                container(markdown::view(readme.items(), settings).map(Message::OpenReadmeLink))
                    .width(Length::Fill)
                    .into()
            }
            InstalledReadmeState::Loaded(None) => container(
                text(crate::i18n::t!("package-no-readme"))
                    .size(13.0)
                    .style(common::muted),
            )
            .padding(10.0)
            .into(),
            InstalledReadmeState::Failed(error) => container(
                text(crate::i18n::t!(
                    "package-readme-load-failed",
                    "error" => error
                ))
                .size(13.0)
                .style(common::danger),
            )
            .padding(10.0)
            .into(),
        }
    }

    /// The way from an import-reference view to the package's own pane, for a package that is
    /// also installed on its own. The button says it all.
    pub(super) fn dependency_also_installed_note(&self, specifier: &str) -> Elem<'_> {
        row![
            iced::widget::space::horizontal(),
            button(text(crate::i18n::t!("package-open-own-pane")).size(12.0))
                .style(button_style::secondary)
                .on_press(Message::SelectInstalledPackage(specifier.to_string())),
        ]
        .align_y(Vertical::Center)
        .into()
    }

    /// The package's own-pane actions. `kept_by` is the set of enabled packages that require this
    /// one: when it's non-empty, "uninstalling" only removes the standalone install — the package
    /// stays resolved as their dependency — so the uninstall action says exactly that rather than
    /// implying full removal.
    pub(super) fn installed_actions(&self, name: &str, kept_by: &[String]) -> Elem<'_> {
        let mut col = Column::new()
            .spacing(10.0)
            .push(common::section_label(crate::i18n::ts!("package-actions")));

        // Edit a copy (local fork). Ask for the destination name up front: keeping the name makes
        // the local copy canonical for this installed package, while changing it creates a new,
        // independently configurable package. Never invent a suffix or remove the fallback row.
        let mut copy = column![
            text(crate::i18n::t!("package-edit-copy")).size(13.0),
            text(crate::i18n::t!("package-edit-copy-help"))
                .size(11.0)
                .style(common::muted),
        ]
        .spacing(6.0);
        if let Some(copy_name) = self.open_fork_name() {
            let existing = self
                .local_packages
                .iter()
                .find(|candidate| candidate.eq_ignore_ascii_case(copy_name.trim()));
            let name_input = text_input(crate::i18n::ts!("package-name-placeholder"), copy_name);
            let name_input = if self.manage_busy {
                name_input
            } else {
                name_input
                    .on_input(Message::SetForkName)
                    .on_submit(Message::ForkPackage)
            };
            copy = copy.push(
                row![
                    container(text(crate::i18n::t!("package-copy-name")).size(12.0))
                        .width(Length::Fixed(92.0)),
                    name_input,
                ]
                .spacing(10.0)
                .align_y(Vertical::Center),
            );
            let mut actions = row![iced::widget::space::horizontal()].spacing(8.0);
            if let Some(existing) = existing {
                let existing = existing.clone();
                actions = actions.push(
                    button(text(crate::i18n::t!("package-open-local-copy")).size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::SelectOwnedPackage(existing)),
                );
            }
            actions = actions
                .push(
                    button(text(crate::i18n::t!("action-cancel")).size(12.0))
                        .style(button_style::secondary)
                        .on_press_maybe((!self.manage_busy).then_some(Message::CancelForkPackage)),
                )
                .push(
                    button(text(crate::i18n::t!("package-create-copy")).size(12.0))
                        .style(button_style::primary)
                        .on_press_maybe(
                            (!self.manage_busy
                                && self.installed_detail_ready_for_copy()
                                && existing.is_none())
                            .then_some(Message::ForkPackage),
                        ),
                );
            copy = copy.push(actions.align_y(Vertical::Center));
        } else {
            copy = copy.push(
                row![
                    iced::widget::space::horizontal(),
                    button(text(crate::i18n::t!("package-edit-copy")).size(12.0))
                        .style(button_style::secondary)
                        .on_press_maybe(
                            (!self.manage_busy && self.installed_detail_ready_for_copy())
                                .then_some(Message::StartForkPackage),
                        ),
                ]
                .align_y(Vertical::Center),
            );
        }
        col = col.push(copy);

        // Uninstall (base-state → inline confirm). When an enabled package still requires this
        // one, removing the standalone install leaves the package resolved as that dependent's
        // dependency, so the label + confirm describe a standalone removal rather than a full
        // uninstall. The "Required by …" section above already names who keeps it.
        let survives = !kept_by.is_empty();
        let mut uninstall = Column::new().spacing(6.0);
        if survives {
            let kept_names = kept_by
                .iter()
                .map(|s| package_display_name(s).to_string())
                .collect::<Vec<_>>()
                .join(", ");
            uninstall = uninstall
                .push(text(crate::i18n::t!("package-remove-standalone")).size(13.0))
                .push(
                    text(crate::i18n::t!(
                        "package-remove-standalone-help",
                        "name" => name,
                        "packages" => kept_names
                    ))
                    .size(11.0)
                    .style(common::muted),
                );
        }
        if self.confirm_uninstall {
            let breaks = &self.uninstall_breaks;
            let orphans = &self.uninstall_orphans;
            // Forced: packages that `require` this one would break without it, so they're removed too
            // (`script/REQUIRED-PACKAGES.md`). Not a choice — keeping them would leave them depending
            // on a missing package.
            if !breaks.is_empty() {
                let names = breaks
                    .iter()
                    .map(|s| package_display_name(s).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                uninstall = uninstall.push(
                    container(
                        text(crate::i18n::t!(
                            "package-removal-required",
                            "name" => name,
                            "packages" => names,
                            "count" => breaks.len() as i64
                        ))
                        .size(12.0)
                        .style(common::warning),
                    )
                    .padding(8.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
                );
            }
            // apt-style orphan prompt: auto-installed required roots nothing else would need once
            // this (and any forced removals) are gone — offered, never silent.
            if !orphans.is_empty() {
                let names = orphans
                    .iter()
                    .map(|s| package_display_name(s).to_string())
                    .collect::<Vec<_>>()
                    .join(", ");
                uninstall = uninstall.push(
                    container(
                        text(crate::i18n::t!(
                            "package-remove-orphans",
                            "packages" => names
                        ))
                        .size(12.0)
                        .style(common::muted),
                    )
                    .padding(8.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
                );
            }
            let confirm_label = if !breaks.is_empty() {
                crate::i18n::t!("package-remove-all")
            } else if survives {
                crate::i18n::t!("package-remove")
            } else if orphans.is_empty() {
                crate::i18n::t!("package-uninstall")
            } else {
                crate::i18n::t!("package-remove-all")
            };
            let mut buttons = row![
                text(if !breaks.is_empty() {
                    crate::i18n::ts!("package-remove-together-question")
                } else if survives {
                    crate::i18n::ts!("package-remove-standalone-question")
                } else if orphans.is_empty() {
                    crate::i18n::ts!("package-uninstall-question")
                } else {
                    crate::i18n::ts!("package-remove-together-question")
                })
                .size(12.0),
                iced::widget::space::horizontal(),
                button(text(crate::i18n::t!("action-cancel")).size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::CancelUninstall),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center);
            // "Keep them" applies only to the offered orphans; the forced breaks always go.
            if !orphans.is_empty() && !survives {
                buttons = buttons.push(
                    button(text(crate::i18n::t!("package-keep-orphans")).size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::UninstallKeepOrphans),
                );
            }
            buttons = buttons.push(
                button(text(confirm_label).size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::ConfirmUninstall),
            );
            uninstall = uninstall.push(buttons);
        } else {
            uninstall = uninstall.push(
                row![
                    iced::widget::space::horizontal(),
                    button(
                        text(if survives {
                            crate::i18n::t!("package-remove-standalone-ellipsis")
                        } else {
                            crate::i18n::t!("package-uninstall-name", "name" => name)
                        })
                        .size(12.0)
                    )
                    .style(button_style::secondary)
                    .on_press(Message::RequestUninstall),
                ]
                .align_y(Vertical::Center),
            );
        }
        col = col.push(uninstall);
        col.into()
    }

    /// The update re-prompt card: the new version's added asks beyond the consented baseline.
    pub(super) fn view_update_delta<'a>(&self, delta: &'a UpdateDelta) -> Elem<'a> {
        // A version-floor hold-back is informational: no grant can load the held-back version
        // (only updating smudgy, or pinning an older version, would), so the card explains the
        // floor and offers only dismissal.
        if let Some(reason) = &delta.needs_smudgy {
            let col = Column::new()
                .spacing(8.0)
                .push(
                    row![
                        common::status_dot(NodeStatus::Warning),
                        text(crate::i18n::t!(
                            "package-update-held-newer",
                            "name" => &delta.name
                        ))
                        .size(14.0),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                )
                .push(
                    // No "you're running vX" claim: the lockfile's last-resolved version can
                    // be stale (a floored pin, or a smudgy downgrade since it last loaded),
                    // so the card states only what is certainly true. The reason carries its
                    // own remedy.
                    text(crate::i18n::t!(
                        "package-version-held-reason",
                        "version" => &delta.version,
                        "reason" => reason
                    ))
                    .size(12.0)
                    .style(common::muted),
                )
                .push(
                    row![
                        iced::widget::space::horizontal(),
                        button(text(crate::i18n::t!("package-ok")).size(12.0))
                            .style(button_style::secondary)
                            .on_press(Message::DismissUpdate),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                );
            return container(col)
                .padding(14.0)
                .width(Length::Fill)
                .style(common::card_style)
                .into();
        }
        if delta.requirements_changed {
            let col = Column::new()
                .spacing(8.0)
                .push(
                    row![
                        common::status_dot(NodeStatus::Warning),
                        text(crate::i18n::t!(
                            "package-update-requirements-changed",
                            "name" => &delta.name
                        ))
                        .size(14.0),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                )
                .push(
                    text(crate::i18n::t!(
                        "package-update-requirements-help",
                        "version" => &delta.version
                    ))
                    .size(12.0)
                    .style(common::muted),
                )
                .push(
                    row![
                        iced::widget::space::horizontal(),
                        button(text(crate::i18n::t!("package-keep-current-version")).size(12.0))
                            .style(button_style::secondary)
                            .on_press(Message::DismissUpdate),
                        button(text(crate::i18n::t!("package-review-update")).size(12.0))
                            .style(button_style::primary)
                            .on_press(Message::GrantUpdate),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                );
            return container(col)
                .padding(14.0)
                .width(Length::Fill)
                .style(common::card_style)
                .into();
        }
        let mut col = Column::new()
            .spacing(8.0)
            .push(
                row![
                    common::status_dot(NodeStatus::Warning),
                    text(crate::i18n::t!(
                        "package-update-blocked-permissions",
                        "name" => &delta.name
                    ))
                    .size(14.0),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            )
            .push(
                text(match &delta.current_version {
                    Some(current) => crate::i18n::t!(
                        "package-update-current-held",
                        "current" => current,
                        "next" => &delta.version
                    ),
                    None => crate::i18n::t!(
                        "package-update-held-load",
                        "version" => &delta.version
                    ),
                })
                .size(12.0)
                .style(common::muted),
            );
        // An update whose ADDED asks include a sandbox escape is a bigger decision than "more
        // hosts" — the banner makes granting it a deliberate trust call, not a reflex.
        if let Some(banner) = full_access_banner(&delta.added) {
            col = col.push(banner);
        }
        let mut lines = Column::new().spacing(4.0);
        for line in permission_can_lines(&delta.added) {
            lines = lines.push(consent_can_row(&line));
        }
        col = col.push(lines);
        col = col.push(
            row![
                iced::widget::space::horizontal(),
                button(text(crate::i18n::t!("package-keep-current-version")).size(12.0))
                    .style(button_style::secondary)
                    .on_press(Message::DismissUpdate),
                button(text(crate::i18n::t!("package-grant-update")).size(12.0))
                    .style(button_style::primary)
                    .on_press(Message::GrantUpdate),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        );
        container(col)
            .padding(14.0)
            .width(Length::Fill)
            .style(common::card_style)
            .into()
    }
}
