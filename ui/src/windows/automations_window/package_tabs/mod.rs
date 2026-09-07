//! Installed and local package tab containers.

mod about;
mod manifest;
mod permissions;
mod settings;
mod sharing;
mod source;

use std::collections::BTreeMap;

use iced::Length;
use iced::alignment::Vertical;
use iced::widget::{
    Column, Id, button, column, container, markdown, pick_list, radio, row, text, text_input,
};
use smudgy_cloud::DependencyKind;
use smudgy_core::models::profile_activation::ProfileActivation;
use smudgy_core::models::shared_packages::{LockedPackage, SharedPackageLock, UpdateMode};

use crate::assets::fonts;
use crate::theme::builtins::button as button_style;

use super::common;
use super::editors::pane_scroll;
use super::keyboard_control::{KeyboardControl, linear_selection, publish_selection};
use super::model::{NodeStatus, package_display_name};
use super::packages::{
    PublicationStatus, PublishVerdict, installed_package_tab_button, local_package_tab_button,
    metric, publish_output_panel, publish_verdict, rating_metric, star_rate_row,
};
use super::{AutomationsWindow, Elem, InstalledPackageTab, LocalPackageTab, Message, Selection};

impl AutomationsWindow {
    pub(super) fn view_installed_package(&self) -> Elem<'_> {
        let Some(locked) = self.installed_open.as_deref() else {
            return pane_scroll(column![
                text(crate::i18n::t!("package-no-selection")).size(13.0)
            ]);
        };
        let specifier = &locked.specifier;
        let package_lock = SharedPackageLock {
            packages: self.installed_packages.clone(),
        };
        let name = package_display_name(specifier).to_string();
        let viewing_as_dependency = matches!(self.selection, Selection::Dependency { .. });
        // A missing edge means a stale selection while the graph refreshes. Treat it as the more
        // restricted imported-dependency view until its relation is known.
        let dependency_kind = self
            .selected_dependency_kind()
            .or_else(|| viewing_as_dependency.then_some(DependencyKind::Dependency));
        let viewing_as_import = dependency_kind == Some(DependencyKind::Dependency);
        let viewing_as_required = dependency_kind == Some(DependencyKind::Requires);
        let effective = match &self.selection {
            Selection::Dependency { parent, spec } if spec == specifier => {
                self.graph.dep_edge_active(parent, specifier)
            }
            _ => self.graph.effectively_enabled(specifier),
        };
        let controllable = self.graph.controllable(specifier);
        let dep_only = self.graph.is_dep_only(specifier);
        let import_only =
            viewing_as_import || (dep_only && self.graph.required_by(specifier).is_empty());
        let required_only = !controllable && !import_only;
        let managed_only = import_only || required_only;
        let requiring_dependents = self.graph.required_by(specifier);
        let enabled_dependents = self.graph.enabled_dependents(specifier);
        // A dependency-reference view shows a package whose resolved version is dictated by the
        // parent's manifest, not chosen here. The blocked-update callout (and its "Latest
        // (blocked)" metric) prompt to grant/keep a version the user can't actually pick in this
        // context, so suppress them — they belong to the package's own top-level pane.
        let status = if !self.package_state_available() {
            NodeStatus::Error
        } else if effective {
            NodeStatus::Ok
        } else {
            NodeStatus::Disabled
        };

        let tab = if managed_only && self.installed_package_tab == InstalledPackageTab::Settings {
            InstalledPackageTab::About
        } else {
            self.installed_package_tab
        };
        let available_tabs = if !managed_only {
            vec![
                InstalledPackageTab::About,
                InstalledPackageTab::Settings,
                InstalledPackageTab::Source,
                InstalledPackageTab::Permissions,
            ]
        } else {
            vec![
                InstalledPackageTab::About,
                InstalledPackageTab::Source,
                InstalledPackageTab::Permissions,
            ]
        };
        let mut tabs = row![].spacing(16.0);
        for available in &available_tabs {
            let label = match available {
                InstalledPackageTab::About => crate::i18n::ts!("package-tab-about"),
                InstalledPackageTab::Settings => crate::i18n::ts!("package-tab-settings"),
                InstalledPackageTab::Source => crate::i18n::ts!("package-tab-source"),
                InstalledPackageTab::Permissions => crate::i18n::ts!("package-tab-permissions"),
            };
            tabs = tabs.push(installed_package_tab_button(tab, *available, label));
        }
        let current_tab = available_tabs
            .iter()
            .position(|available| *available == tab)
            .unwrap_or(0);
        let tabs_for_keys = available_tabs.clone();
        let id = Id::from(format!("installed-package-tabs:{specifier}"));
        let focus_id = id.clone();
        let tabs: Elem<'_> = KeyboardControl::new(
            tabs,
            id,
            move || Message::FocusColorControl(focus_id.clone()),
            move |key, _repeat| {
                publish_selection(
                    linear_selection(key, current_tab, tabs_for_keys.len()),
                    |index| Message::SelectInstalledPackageTab(tabs_for_keys[index]),
                )
            },
        )
        .focus_color(iced::Color::TRANSPARENT)
        .into();

        let activation_badge = (controllable && !viewing_as_import)
            .then(|| common::badge(self.profile_activation_summary(&locked.activation())));
        let header = self.scene_header(
            Some(status),
            &name,
            Some(specifier.clone()),
            activation_badge,
        );

        // A pending update/pin review takes over the pane while it is open, exactly as an install
        // review takes over Discover: the card is the reason the version controls are unavailable,
        // and nothing is written until Apply. Only this package's own card is shown here.
        if let Some(prompt) = self.consent_prompt_for_open_installed() {
            return pane_scroll(column![header, self.view_consent_prompt(prompt)].spacing(16.0));
        }

        let mut body = column![header, tabs].spacing(16.0);

        // Context banner.
        let banner_text = if viewing_as_required {
            Some(crate::i18n::t!("package-required-managed"))
        } else if viewing_as_import {
            Some(crate::i18n::t!("package-import-managed"))
        } else if required_only {
            Some(crate::i18n::t!("package-required-managed"))
        } else if dep_only {
            Some(crate::i18n::t!("package-dependency-managed"))
        } else if !requiring_dependents.is_empty() {
            let who: Vec<String> = requiring_dependents
                .iter()
                .map(|s| package_display_name(s).to_string())
                .collect();
            Some(crate::i18n::t!(
                "package-direct-and-required",
                "packages" => who.join(", ")
            ))
        } else if controllable && !effective {
            Some(crate::i18n::t!("package-disabled-review"))
        } else {
            None
        };
        if tab == InstalledPackageTab::About
            && let Some(banner) = banner_text
        {
            body = body.push(
                container(text(banner).size(13.0))
                    .width(Length::Fill)
                    .padding(12.0)
                    .style(common::banner_style),
            );
        }

        // Update re-prompt: a newly-resolved version wants more access than was consented. Not
        // shown for a dependency-reference view — its version follows the parent's manifest, so a
        // grant/keep choice here would be meaningless.
        if tab == InstalledPackageTab::About
            && !viewing_as_import
            && let Some(delta) = &self.update_delta
            && delta.specifier == *specifier
        {
            body = body.push(self.view_update_delta(delta));
        }

        // Meta row. "Loaded" is the version the engine actually resolved (the lockfile's
        // last-resolved record) — which, for a held-back package, is the older fitting version,
        // NOT the latest the inspect pane probes. Show the held-back latest separately so the two
        // never look contradictory.
        let loaded = locked
            .last_resolved_version
            .clone()
            .or_else(|| self.graph.resolved.get(specifier).cloned());
        let blocked_latest = self
            .update_delta
            .as_ref()
            .filter(|_| !viewing_as_import)
            .filter(|delta| delta.specifier == *specifier)
            .map(|delta| delta.version.clone());
        let mut meta = row![].spacing(20.0).align_y(Vertical::Center);
        if let Some(detail) = self.installed_detail.as_deref() {
            meta = meta.push(metric(
                crate::i18n::ts!("package-metric-author"),
                &detail.owner_nickname,
            ));
        }
        if let Some(v) = &loaded {
            meta = meta.push(metric(
                crate::i18n::ts!("package-metric-loaded"),
                &format!("v{v}"),
            ));
        }
        if let Some(v) = &blocked_latest {
            meta = meta.push(metric(
                crate::i18n::ts!("package-metric-latest-blocked"),
                &format!("v{v}"),
            ));
        }
        meta = meta.push(metric(
            crate::i18n::ts!("package-metric-update"),
            match &locked.mode {
                UpdateMode::Auto => crate::i18n::ts!("package-update-auto"),
                UpdateMode::Pinned { .. } => crate::i18n::ts!("package-update-pinned"),
            },
        ));
        // Cloud rating + popularity (best-effort metadata; absent for a local/owned package or while
        // the detail is still loading).
        if let Some(rating) = self.installed_rating.as_deref() {
            let star_color = crate::prefs::current().palette.output;
            meta = meta.push(rating_metric(
                rating.avg_rating,
                rating.rating_count,
                star_color,
            ));
            meta = meta.push(metric(
                crate::i18n::ts!("package-metric-installs"),
                &rating.install_count.to_string(),
            ));
        }
        if tab == InstalledPackageTab::About {
            body = body.push(meta);
        }
        // DEFERRED (`script/REQUIRED-PACKAGES.md` "Version contention surfacing"): a small note here
        // when a singleton-registering library is loaded at more than one version — a `requires`
        // root vs an `import`ed-for-helpers copy at a different version. Detecting it needs the
        // per-importer (referrer-aware) resolved versions of *every* installed package, which lives
        // in the engine's resolution (`package_solver.rs`/`package_provider.rs`), not in the UI: the
        // window only caches one resolved version per specifier (`graph.resolved`) and the open
        // package's own `dependencies`, neither of which can witness a second loaded version pulled
        // in by another package. Surfacing it from here would mean resolving every installed
        // package's closure and threading per-version provenance into the UI — out of proportion to
        // a best-effort note — so this is deferred rather than faked. Items 1–5 (the install/consent
        // closure, peer conflict, orphan prompt) are the substantive deliverables.

        // Rate — an account-only write, so the star control shows only when signed in and the
        // package's cloud metadata loaded (i.e. it's a real cloud package, not a local copy).
        if tab == InstalledPackageTab::About && self.signed_in() && self.installed_rating.is_some()
        {
            body = body.push(star_rate_row(Message::RateInstalledPackage));
        }

        if let Some(feedback) = &self.manage_feedback {
            body = body.push(text(feedback.clone()).size(12.0).style(common::muted));
        }

        // An imported dependency shares its parent's isolate and grants. A `requires` target runs
        // as its own root, so its own permissions and consent are the truthful view.
        if tab == InstalledPackageTab::Permissions {
            body = if viewing_as_import
                && let Selection::Dependency { parent, .. } = &self.selection
            {
                body.push(self.view_dependency_permissions_section(parent))
            } else {
                body.push(self.view_permissions_section(locked))
            };
        }

        // Required by.
        if tab == InstalledPackageTab::About
            && (!enabled_dependents.is_empty() || !self.graph.required_by(specifier).is_empty())
        {
            let mut req = Column::new()
                .spacing(4.0)
                .push(common::section_label(crate::i18n::ts!(
                    "package-required-by"
                )));
            for parent in self.graph.required_by(specifier) {
                let enabled = self.graph.effectively_enabled(&parent);
                req = req.push(self.dep_link_row(
                    &parent,
                    enabled,
                    crate::i18n::ts!("package-needs"),
                    None,
                    None,
                ));
            }
            body = body.push(req);
        }

        // Only explicit roots have independent Settings. Automatically installed `requires`
        // targets follow their active parent chain, and imported dependencies share a parent.
        if tab == InstalledPackageTab::Settings && !managed_only {
            let inherited_notices = self
                .profile_names
                .iter()
                .filter(|profile| !locked.activation().is_enabled_for(profile))
                .filter_map(|profile| {
                    locked
                        .required_by
                        .iter()
                        .find(|parent| {
                            self.governing_specifier(parent)
                                .eq_ignore_ascii_case(parent)
                                && package_lock.is_effectively_enabled_for(parent, profile)
                        })
                        .map(|parent| {
                            (
                                profile.clone(),
                                crate::i18n::t!(
                                    "package-required-in-profile",
                                    "package" => package_display_name(parent)
                                ),
                            )
                        })
                })
                .collect::<BTreeMap<_, _>>();
            if controllable {
                body = body.push(self.activation_controls(&locked.activation(), inherited_notices));
            }
            if self
                .param_config
                .as_ref()
                .is_some_and(|config| config.specifier == *specifier && !config.params.is_empty())
            {
                body = body.push(self.parameter_scope_control(locked));
                if let Some(settings) = self.view_param_config_section(specifier) {
                    body = body.push(settings);
                }
            }
        }

        // Update mode (controllable only).
        if tab == InstalledPackageTab::About && controllable {
            let mut update_row = row![
                common::section_label(crate::i18n::ts!("package-update-mode")),
                radio(
                    crate::i18n::t!("package-update-auto-track"),
                    false,
                    Some(matches!(locked.mode, UpdateMode::Pinned { .. })),
                    |_| Message::SetInstalledUpdateMode(UpdateMode::Auto)
                ),
            ]
            .spacing(16.0)
            .align_y(Vertical::Center);
            if !self.installed_versions.is_empty() {
                let current = match &locked.mode {
                    UpdateMode::Pinned { version } => Some(version.clone()),
                    UpdateMode::Auto => None,
                };
                update_row = update_row.push(
                    pick_list(self.installed_versions.clone(), current, |v| {
                        Message::SetInstalledUpdateMode(UpdateMode::Pinned { version: v })
                    })
                    .placeholder(crate::i18n::ts!("package-update-pinned-placeholder")),
                );
            }
            body = body.push(update_row);
        }

        // Dependencies.
        let deps = self
            .graph
            .requires
            .get(specifier)
            .cloned()
            .unwrap_or_default();
        if tab == InstalledPackageTab::About && !deps.is_empty() {
            let mut dep_col =
                Column::new()
                    .spacing(4.0)
                    .push(common::section_label(crate::i18n::ts!(
                        "package-dependencies"
                    )));
            for edge in &deps {
                // This row exists because the open package (`specifier`) depends on
                // `edge.specifier`, so its dot follows the parent's context: it greys when the
                // parent is disabled, instead of staying lit on the dep's global enabled state
                // (which a separately-installed dep keeps on its own row).
                let enabled = self.graph.dep_edge_active(specifier, &edge.specifier);
                let resolved = self.graph.resolved.get(&edge.specifier).cloned();
                let range = if edge.range.is_empty() {
                    resolved
                        .clone()
                        .map(|v| format!("→ v{v}"))
                        .unwrap_or_default()
                } else {
                    format!(
                        "{} → v{}",
                        edge.range,
                        resolved.clone().unwrap_or_else(|| "?".to_string())
                    )
                };
                dep_col = dep_col.push(self.dep_link_row(
                    &edge.specifier,
                    enabled,
                    &range,
                    Some(specifier),
                    Some(edge.kind),
                ));
            }
            if controllable
                && !effective
                && deps
                    .iter()
                    .any(|e| !self.graph.effectively_enabled(&e.specifier))
            {
                let names: Vec<String> = deps
                    .iter()
                    .map(|e| package_display_name(&e.specifier).to_string())
                    .collect();
                dep_col = dep_col.push(
                    text(crate::i18n::t!(
                        "package-enabling-dependencies",
                        "name" => &name,
                        "dependencies" => names.join(", ")
                    ))
                    .size(12.0)
                    .style(common::muted),
                );
            }
            body = body.push(dep_col);
        }

        if tab == InstalledPackageTab::About {
            body = body.push(self.installed_readme_view());
        } else if tab == InstalledPackageTab::Source {
            body = body.push(self.installed_source_browser());
        }

        // Actions. A dep-only package is removed automatically and has nothing to manage. A
        // dependency-reference view of a package that's *also* installed on its own defers
        // management to that package's own pane: uninstalling from here would drop only the
        // standalone install while the parent keeps the package resolved, so it reads as a no-op
        // (this mirrors how the dependency view already suppresses the toggle, params, and
        // permissions). Otherwise — the package's own pane — show the real actions. `controllable`
        // only gates the enable *toggle* (an enabled dependent forces it on); it must NOT gate
        // fork/uninstall here, or a package installed directly *and* pulled in as a dependency
        // would lose "Edit a copy" on its own pane just because something else also needs it.
        if tab == InstalledPackageTab::About && (dep_only || required_only) {
            body = body.push(
                container(
                    text(crate::i18n::t!("package-dependency-auto-remove"))
                        .size(12.0)
                        .style(common::muted),
                )
                .padding(10.0)
                .style(common::banner_style),
            );
        } else if tab == InstalledPackageTab::About && viewing_as_import {
            body = body.push(self.dependency_also_installed_note(specifier));
        } else if tab == InstalledPackageTab::About {
            body = body.push(self.installed_actions(&name, &requiring_dependents));
        }

        pane_scroll(body)
    }

    pub(super) fn view_owned_package(&self) -> Elem<'_> {
        let Some(package) = self.local_package.as_deref() else {
            return pane_scroll(column![
                text(crate::i18n::t!("package-no-selection")).size(13.0)
            ]);
        };
        let manifest = &package.manifest;
        let own_specifier = self.local_own_spec(&package.name);
        let shadows_remote = self.installed_packages.iter().any(|entry| {
            entry.specifier != own_specifier
                && package_display_name(&entry.specifier).eq_ignore_ascii_case(&package.name)
        });
        let locked = self
            .installed_packages
            .iter()
            .find(|entry| entry.specifier == own_specifier)
            .cloned()
            .unwrap_or_else(|| {
                let mut package = LockedPackage::new(own_specifier.clone(), UpdateMode::Auto);
                package.set_activation(ProfileActivation::None);
                package
            });
        let tab = self.local_package_tab;
        let signed_in = self.signed_in();
        let visibility = match &self.publication_status {
            PublicationStatus::Bound(_) if self.share_is_public => {
                crate::i18n::t!("package-public")
            }
            PublicationStatus::Bound(_) => crate::i18n::t!("package-private"),
            PublicationStatus::Checking => crate::i18n::t!("package-publication-status-checking"),
            _ => crate::i18n::t!("package-not-published"),
        };
        // Display the *draft* manifest the form is editing (falling back to the on-disk one), so the
        // header/meta/publish-verdict never contradict the editor below while there are unsaved edits.
        let draft = self.manifest_draft.as_ref();
        let disp_version = draft.map_or_else(
            || manifest.version.clone(),
            |d| d.version.trim().to_string(),
        );
        let disp_description = draft.map_or_else(
            || manifest.description.clone(),
            |d| d.description.trim().to_string(),
        );
        let disp_dep_count = draft.map_or(manifest.dependencies.len(), |d| {
            d.dependencies
                .iter()
                .filter(|s| !s.trim().is_empty())
                .count()
        });
        let verdict = publish_verdict(&disp_version, &self.share_versions);

        let source_tab_label =
            common::unsaved_tab_label(crate::i18n::t!("package-tab-source"), self.dirty);
        let tabs = row![
            local_package_tab_button(
                tab,
                LocalPackageTab::About,
                crate::i18n::ts!("package-tab-about"),
            ),
            local_package_tab_button(
                tab,
                LocalPackageTab::Settings,
                crate::i18n::ts!("package-tab-settings"),
            ),
            local_package_tab_button(tab, LocalPackageTab::Source, source_tab_label),
            local_package_tab_button(
                tab,
                LocalPackageTab::Permissions,
                crate::i18n::ts!("package-tab-permissions"),
            ),
            local_package_tab_button(
                tab,
                LocalPackageTab::Manifest,
                crate::i18n::ts!("package-tab-manifest"),
            ),
            local_package_tab_button(
                tab,
                LocalPackageTab::Sharing,
                crate::i18n::ts!("package-tab-sharing"),
            ),
        ]
        .spacing(16.0);
        let available_tabs = [
            LocalPackageTab::About,
            LocalPackageTab::Settings,
            LocalPackageTab::Source,
            LocalPackageTab::Permissions,
            LocalPackageTab::Manifest,
            LocalPackageTab::Sharing,
        ];
        let current_tab = available_tabs
            .iter()
            .position(|available| *available == tab)
            .unwrap_or(0);
        let id = Id::from(format!("local-package-tabs:{}", package.name));
        let focus_id = id.clone();
        let tabs: Elem<'_> = KeyboardControl::new(
            tabs,
            id,
            move || Message::FocusColorControl(focus_id.clone()),
            move |key, _repeat| {
                publish_selection(
                    linear_selection(key, current_tab, available_tabs.len()),
                    |index| Message::SelectLocalPackageTab(available_tabs[index]),
                )
            },
        )
        .focus_color(iced::Color::TRANSPARENT)
        .into();
        let package_lock = SharedPackageLock {
            packages: self.installed_packages.clone(),
        };
        let local_effective =
            package_lock.is_effectively_enabled_for(&own_specifier, &self.profile_name);
        let local_status = if !self.package_state_available() {
            NodeStatus::Error
        } else if local_effective {
            NodeStatus::Ok
        } else {
            NodeStatus::Disabled
        };
        let header_badges: Elem<'_> = row![
            common::badge(self.profile_activation_summary(&locked.activation())),
            common::badge(visibility),
        ]
        .spacing(6.0)
        .into();
        let header = self.scene_header(
            Some(local_status),
            &package.name,
            Some(crate::i18n::t!("package-owned-subtitle", "version" => &disp_version)),
            Some(header_badges),
        );

        // A pending manifest requirements review takes over the pane while it is open. The
        // manifest editor (and its Save) is unavailable behind it because the save is exactly
        // what the card decides; Cancel returns to the editor with the draft intact.
        if let Some(prompt) = self.consent_prompt_for_open_local() {
            return pane_scroll(column![header, self.view_consent_prompt(prompt)].spacing(16.0));
        }

        let mut body = column![header, tabs].spacing(16.0);

        // Local-package mutations may originate from Settings, Permissions, Source, or About.
        // Keep their results visible after a tab change so a failed action never looks like a no-op.
        if let Some(feedback) = self.manage_feedback.as_deref() {
            body = body.push(
                container(text(feedback).size(12.0).style(common::muted))
                    .padding(10.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
            );
        }
        if let Some(feedback) = self.authoring_feedback.as_deref()
            && self.manage_feedback.as_deref() != Some(feedback)
        {
            body = body.push(
                container(text(feedback).size(12.0).style(common::muted))
                    .padding(10.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
            );
        }

        // The package description (authored in the manifest) — what Discover shows publicly.
        if tab == LocalPackageTab::About && !disp_description.is_empty() {
            body = body.push(text(disp_description).size(13.0).style(common::muted));
        }
        if tab == LocalPackageTab::About && shadows_remote {
            body = body.push(
                container(text(crate::i18n::t!("package-local-override-note")).size(12.0))
                    .padding(12.0)
                    .width(Length::Fill)
                    .style(common::banner_style),
            );
        }
        if tab == LocalPackageTab::About
            && let Some(readme) = &self.local_readme
        {
            let settings = markdown::Settings::with_text_size(
                13.0,
                markdown::Style::from_palette(iced::theme::Palette::DARK),
            );
            body = body.push(
                container(markdown::view(readme.items(), settings).map(Message::OpenReadmeLink))
                    .width(Length::Fill),
            );
        }

        // Rename affordance — the folder name is the package's identity (the manifest has no name),
        // and renaming is how a fork is "claimed" so it can be published.
        if tab == LocalPackageTab::About
            && let Some(buffer) = self.open_rename_buffer()
        {
            body = body.push(
                row![
                    text_input(crate::i18n::ts!("package-new-name-placeholder"), buffer)
                        .on_input(Message::RenameOwnedChanged)
                        .on_submit(Message::CommitRenameOwned)
                        .width(Length::Fixed(220.0)),
                    button(text(crate::i18n::t!("package-save-name")).size(12.0))
                        .style(button_style::primary)
                        .on_press_maybe(
                            (!self.authoring_busy && !self.share_busy)
                                .then_some(Message::CommitRenameOwned,)
                        ),
                    button(text(crate::i18n::t!("action-cancel")).size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::CancelRenameOwned),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            );
        } else if tab == LocalPackageTab::About {
            body = match &self.publication_status {
                PublicationStatus::Unpublished => body.push(
                    button(text(crate::i18n::t!("package-rename")).size(12.0))
                        .style(button_style::subtle)
                        .on_press_maybe(
                            (!self.authoring_busy && !self.share_busy)
                                .then_some(Message::StartRenameOwned),
                        ),
                ),
                PublicationStatus::Bound(_) => body.push(
                    text(crate::i18n::t!("package-name-locked-published"))
                        .size(12.0)
                        .style(common::muted),
                ),
                PublicationStatus::Checking => body.push(
                    text(crate::i18n::t!("package-publication-status-checking"))
                        .size(12.0)
                        .style(common::muted),
                ),
                PublicationStatus::Unknown => body.push(
                    text(crate::i18n::t!("package-publication-status-unknown"))
                        .size(12.0)
                        .style(common::muted),
                ),
                PublicationStatus::Invalid(message) => {
                    body.push(text(message.clone()).size(12.0).style(common::danger))
                }
            };
        }

        if tab == LocalPackageTab::Settings {
            let inherited_notices = self
                .profile_names
                .iter()
                .filter(|profile| !locked.is_enabled_for(profile))
                .filter_map(|profile| {
                    locked
                        .required_by
                        .iter()
                        .find(|parent| {
                            self.governing_specifier(parent)
                                .eq_ignore_ascii_case(parent)
                                && package_lock.is_effectively_enabled_for(parent, profile)
                        })
                        .map(|parent| {
                            (
                                profile.clone(),
                                crate::i18n::t!(
                                    "package-required-in-profile",
                                    "package" => package_display_name(parent)
                                ),
                            )
                        })
                })
                .collect::<BTreeMap<_, _>>();
            body = body.push(self.activation_controls(&locked.activation(), inherited_notices));
            if self.param_config.as_ref().is_some_and(|config| {
                config.specifier == own_specifier && !config.params.is_empty()
            }) {
                body = body.push(self.parameter_scope_control(&locked));
                if let Some(settings) = self.view_param_config_section(&own_specifier) {
                    body = body.push(settings);
                }
            }
        }

        // Meta.
        let mut meta = row![].spacing(20.0).align_y(Vertical::Center);
        meta = meta.push(metric(
            crate::i18n::ts!("package-metric-latest"),
            &format!("v{disp_version}"),
        ));
        let live_count = self.share_versions.iter().filter(|v| !v.deleted).count();
        meta = meta.push(metric(
            crate::i18n::ts!("package-metric-versions"),
            &live_count.to_string(),
        ));
        if disp_dep_count > 0 {
            meta = meta.push(metric(
                crate::i18n::ts!("package-dependencies"),
                &disp_dep_count.to_string(),
            ));
        }
        if tab == LocalPackageTab::About {
            body = body.push(meta);
        }

        // Sandbox status: a local package runs sandboxed against its own manifest permissions (the
        // manifest is the grant table). States the runtime reality the QA pass found missing, and
        // links into the manifest editor as the capability-grant mechanism.
        if tab == LocalPackageTab::Permissions {
            body = body.push(self.view_owned_sandbox_section(package));
        }

        // Rich manifest editor (the smudgy.package.json file itself is hidden from the source
        // browser below).
        if tab == LocalPackageTab::Manifest {
            body = body.push(self.view_package_manifest_tab());
        }

        // Settings (configured param values) — the manifest above declares the params; this sets
        // the values the package reads when run locally. Keyed by the local package's own-handle
        // specifier, the same one the runtime resolves it under. Renders nothing without params.
        if tab == LocalPackageTab::Source {
            body = body.push(self.owned_file_browser(package));
        }

        // Publish. Publish reads the on-disk package, so it's disabled while a source or manifest
        // editor has unsaved edits (you'd otherwise ship the pre-edit bytes). Otherwise it's gated on a
        // semver-fluent verdict: disabled while busy, when the version isn't valid publishable semver,
        // or when the number is already used (live/yanked/deleted) — numbers are permanently reserved.
        // The package service owns the globally-unique leaf-name check. Local validation keeps the
        // button honest, while a conflicting remote package is reported by the publish result.
        let can_publish = signed_in
            && !self.authoring_busy
            && !self.share_busy
            && !self.dirty
            && !self.manifest_dirty
            && matches!(verdict, PublishVerdict::Ready);
        if tab == LocalPackageTab::Sharing && !signed_in {
            body = body.push(self.signed_out_banner());
        }
        if tab == LocalPackageTab::Sharing
            && let Some(feedback) = &self.share_feedback
        {
            body = body.push(text(feedback.clone()).size(12.0).style(common::danger));
        }
        if tab == LocalPackageTab::Sharing && signed_in {
            body = body.push(
                row![
                    iced::widget::space::horizontal(),
                    button(
                        row![
                            text(crate::assets::bootstrap_icons::CLOUD_UPLOAD)
                                .font(fonts::BOOTSTRAP_ICONS)
                                .size(13.0),
                            text(crate::i18n::t!("package-publish")).size(13.0),
                        ]
                        .spacing(6.0)
                        .align_y(Vertical::Center)
                    )
                    .style(button_style::primary)
                    .on_press_maybe(can_publish.then_some(Message::PublishOwned)),
                ]
                .align_y(Vertical::Center),
            );
        }
        // Explain why Publish is disabled (when it is). Unsaved manifest edits take precedence —
        // publishing them requires saving them first.
        if tab == LocalPackageTab::Sharing && signed_in && (self.dirty || self.manifest_dirty) {
            body = body.push(
                text(crate::i18n::t!("package-save-before-publish"))
                    .size(12.0)
                    .style(common::warning),
            );
        } else if tab == LocalPackageTab::Sharing && signed_in {
            match &verdict {
                PublishVerdict::Invalid(reason) => {
                    body = body.push(text(reason.clone()).size(12.0).style(common::danger));
                }
                PublishVerdict::AlreadyUsed => {
                    use super::sharing_status::{PublishedContent, next_patch};
                    let message = match self.published_content() {
                        PublishedContent::Compared(_, true) => text(crate::i18n::t!(
                            "package-version-up-to-date", "version" => &disp_version
                        ))
                        .style(common::success),
                        PublishedContent::Compared(_, false) => text(crate::i18n::t!(
                            "package-version-local-changes", "version" => &disp_version
                        ))
                        .style(common::warning),
                        PublishedContent::Checking(_) => text(crate::i18n::t!(
                            "package-version-checking", "version" => &disp_version
                        ))
                        .style(common::muted),
                        PublishedContent::Unknown => text(crate::i18n::t!(
                            "package-version-already-used", "version" => &disp_version
                        ))
                        .style(common::warning),
                    };
                    body = body.push(message.size(12.0));
                    if matches!(
                        self.published_content(),
                        PublishedContent::Compared(_, false)
                    ) {
                        body = body.push(
                            button(text(crate::i18n::t!("package-increase-version")).size(12.0))
                                .style(button_style::secondary)
                                .on_press_maybe(
                                    (!self.authoring_busy
                                        && !self.share_busy
                                        && !self.consent_busy
                                        && self.consent_prompt.is_none()
                                        && next_patch(&disp_version, &self.share_versions)
                                            .is_some())
                                    .then_some(Message::IncreasePackageVersion),
                                ),
                        );
                    }
                }
                PublishVerdict::Ready => {}
            }
        }

        if tab == LocalPackageTab::Sharing
            && let Some(output) = self
                .publish_output
                .as_ref()
                .filter(|output| output.package == package.name)
        {
            body = body.push(
                column![
                    common::section_label(crate::i18n::ts!("package-publish-output")),
                    publish_output_panel(&output.text),
                ]
                .spacing(6.0),
            );
        }

        // Published versions.
        let mut versions =
            Column::new()
                .spacing(4.0)
                .push(common::section_label(crate::i18n::ts!(
                    "package-published-versions"
                )));
        if self.share_versions.is_empty() {
            versions = versions.push(
                text(crate::i18n::t!("package-no-published-versions"))
                    .size(12.0)
                    .style(common::muted),
            );
        }
        // "latest" is the highest live (non-yanked, non-deleted) version. The list now
        // also carries hard-deleted numbers (reserved forever) which render greyed.
        let latest_idx = self
            .share_versions
            .iter()
            .position(|v| !v.yanked && !v.deleted);
        for (i, v) in self.share_versions.iter().enumerate() {
            // A hard-deleted number: content is gone, but the number stays reserved. Show
            // it greyed so the author sees it's spent; no actions.
            if v.deleted {
                versions = versions.push(
                    row![
                        text(format!("v{}", v.version))
                            .size(13.0)
                            .style(common::faint),
                        text(crate::i18n::t!("package-version-deleted"))
                            .size(11.0)
                            .style(common::faint),
                    ]
                    .spacing(8.0)
                    .align_y(Vertical::Center),
                );
                continue;
            }
            let mut left = row![text(format!("v{}", v.version)).size(13.0)]
                .spacing(8.0)
                .align_y(Vertical::Center);
            if Some(i) == latest_idx {
                left = left.push(common::badge(crate::i18n::t!("package-version-latest")));
            }
            if v.yanked {
                left = left.push(
                    text(crate::i18n::t!("package-version-yanked"))
                        .size(11.0)
                        .style(common::faint),
                );
            }
            let mut actions = row![
                left,
                iced::widget::space::horizontal(),
                button(
                    text(if v.yanked {
                        crate::i18n::t!("package-version-unyank")
                    } else {
                        crate::i18n::t!("package-version-yank")
                    })
                    .size(11.0)
                )
                .style(button_style::secondary)
                .on_press_maybe(
                    (!self.authoring_busy && !self.share_busy).then_some(Message::YankVersion {
                        version: v.version.clone(),
                        yanked: !v.yanked,
                    },)
                ),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center);
            // Delete is the heavy, deliberate step — only offered once a version is yanked.
            if v.yanked {
                actions = actions.push(
                    button(
                        text(crate::i18n::t!("action-delete"))
                            .size(11.0)
                            .style(common::danger),
                    )
                    .style(button_style::secondary)
                    .on_press_maybe(
                        (!self.authoring_busy && !self.share_busy)
                            .then_some(Message::DeleteVersion(v.version.clone())),
                    ),
                );
            }
            versions = versions.push(actions);
        }
        versions = versions.push(
            text(crate::i18n::t!("package-yank-help"))
                .size(11.0)
                .style(common::faint),
        );
        if tab == LocalPackageTab::Sharing && signed_in {
            body = body.push(versions);
        }

        // Sharing.
        if tab == LocalPackageTab::Sharing && signed_in {
            body = body.push(self.owned_sharing_section());
        }

        // Delete package.
        if tab == LocalPackageTab::About && self.confirm_delete_local {
            body = body.push(
                row![
                    text(crate::i18n::t!("package-delete-question")).size(12.0),
                    iced::widget::space::horizontal(),
                    button(text(crate::i18n::t!("action-cancel")).size(12.0))
                        .style(button_style::secondary)
                        .on_press(Message::CancelDeleteOwned),
                    button(text(crate::i18n::t!("action-delete")).size(12.0))
                        .style(button_style::secondary)
                        .on_press_maybe(
                            (!self.authoring_busy && !self.share_busy)
                                .then_some(Message::DeleteOwned),
                        ),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
            );
        } else if tab == LocalPackageTab::About {
            body = body.push(
                row![
                    iced::widget::space::horizontal(),
                    button(text(crate::i18n::t!("package-delete-ellipsis")).size(12.0))
                        .style(button_style::secondary)
                        .on_press_maybe(
                            (!self.authoring_busy && !self.share_busy)
                                .then_some(Message::RequestDeleteOwned),
                        ),
                ]
                .align_y(Vertical::Center),
            );
        }

        pane_scroll(body)
    }
}
