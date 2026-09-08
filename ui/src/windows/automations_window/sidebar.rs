//! The navigator sidebar: New menu, search, filter chips, the status-dotted
//! tree (SCRIPTS / MODULES / PACKAGES with dependency nesting), and the footer.

use std::collections::BTreeMap;

use iced::alignment::Vertical;
use iced::widget::{Column, button, column, container, row, scrollable, text, text_input};
use iced::{Background, Border, Color, Length, Padding};

use smudgy_cloud::DependencyKind;
use smudgy_core::models::packages;
use smudgy_core::models::shared_packages::SharedPackageLock;
use smudgy_core::session::runtime::AutomationKind;

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::Theme;
use crate::theme::builtins::button as button_style;
use crate::widgets::wrap_row::wrap_row;

use super::common;
use super::model::{CreatorAutomations, NodeStatus, Script, ScriptKey, package_display_name};
use super::{AutomationsWindow, Chip, Elem, Message, Selection};

const SIDEBAR_WIDTH: f32 = 282.0;

fn partial_scope_badge<'a>(enabled: usize, total: usize) -> Option<Elem<'a>> {
    is_partial_scope(enabled, total).then(|| common::badge(format!("{enabled}/{total}")))
}

fn is_partial_scope(enabled: usize, total: usize) -> bool {
    enabled != 0 && enabled != total
}

fn effective_folder_profile_count(
    path: &str,
    tree: &packages::PackageTree,
    profile_names: &[String],
) -> usize {
    profile_names
        .iter()
        .filter(|profile| {
            packages::is_package_effectively_enabled_for(path, tree, profile.as_str())
        })
        .count()
}

impl AutomationsWindow {
    pub(super) fn view_sidebar(&self) -> Elem<'_> {
        let new_button = button(
            row![
                text(bootstrap_icons::PLUS_LG)
                    .font(fonts::BOOTSTRAP_ICONS)
                    .size(13.0),
                text(crate::i18n::t!("automations-new")).size(14.0),
                iced::widget::space::horizontal(),
                text(bootstrap_icons::CHEVRON_DOWN)
                    .font(fonts::BOOTSTRAP_ICONS)
                    .size(11.0),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        )
        .style(button_style::primary)
        .on_press(Message::ToggleNewMenu)
        .width(Length::Fill)
        .padding(Padding {
            top: 8.0,
            bottom: 8.0,
            left: 12.0,
            right: 12.0,
        });

        let mut top = column![new_button].spacing(8.0);
        if self.new_menu_open {
            top = top.push(self.new_menu());
        }

        // Search field.
        let mut search_row = row![
            text(bootstrap_icons::SEARCH)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(12.0)
                .style(common::faint),
            text_input(
                crate::i18n::ts!("automations-search-placeholder"),
                &self.search
            )
            .on_input(Message::SearchChanged)
            .size(13.0),
        ]
        .spacing(6.0)
        .align_y(Vertical::Center);
        if !self.search.is_empty() {
            search_row = search_row.push(
                button(text("\u{2715}").size(12.0).style(common::muted))
                    .style(button_style::list_item)
                    .on_press(Message::ClearSearch)
                    .padding(2),
            );
        }
        top = top.push(
            container(search_row)
                .padding(Padding {
                    top: 6.0,
                    bottom: 6.0,
                    left: 10.0,
                    right: 8.0,
                })
                .style(common::code_surface_style),
        );

        // Filter chips.
        top = top.push(self.filter_chips());

        // A contextual create shortcut for the selected single-type chip, sitting
        // directly beneath the chips (the `All` chip has no single type, so it
        // shows nothing — the New menu covers that case).
        if let Some(create) = self.create_new_button() {
            top = top.push(create);
        }

        // The tree.
        let tree = scrollable(self.tree())
            .height(Length::Fill)
            .width(Length::Fill);

        let content = column![
            top,
            container(tree).height(Length::Fill).padding(Padding {
                top: 6.0,
                bottom: 6.0,
                left: 0.0,
                right: 0.0,
            }),
            self.footer(),
        ]
        .spacing(10.0)
        .height(Length::Fill);

        container(content)
            .width(Length::Fixed(SIDEBAR_WIDTH))
            .height(Length::Fill)
            .padding(12.0)
            .style(|theme: &Theme| iced::widget::container::Style {
                background: Some(common::top_gradient(
                    theme.styles.general.top_highlight,
                    theme.styles.general.container_background,
                )),
                border: Border {
                    color: theme.styles.general.border,
                    width: 0.0,
                    radius: 0.0.into(),
                },
                ..Default::default()
            })
            .into()
    }

    fn new_menu(&self) -> Elem<'_> {
        let item = |icon: &'static str, label: &'static str, msg: Message| -> Elem<'_> {
            button(
                row![
                    text(icon).font(fonts::BOOTSTRAP_ICONS).size(14.0),
                    text(label).size(13.0),
                ]
                .spacing(10.0)
                .align_y(Vertical::Center),
            )
            .style(button_style::list_item)
            .on_press(msg)
            .width(Length::Fill)
            .into()
        };
        container(
            column![
                item(bootstrap_icons::AT, "Alias", Message::NewAlias),
                item(bootstrap_icons::LIGHTNING, "Trigger", Message::NewTrigger),
                item(bootstrap_icons::DPAD, "Hotkey", Message::NewHotkey),
                item(bootstrap_icons::FOLDER_PLUS, "Folder", Message::NewFolder),
                item(bootstrap_icons::FONTS, "Module", Message::NewModule),
                item(
                    bootstrap_icons::BOUNDING_BOX,
                    "Package",
                    Message::NewPackage
                ),
            ]
            .spacing(2.0),
        )
        .padding(6.0)
        .width(Length::Fill)
        .style(common::card_style)
        .into()
    }

    fn filter_chips(&self) -> Elem<'_> {
        let counts = self.counts();
        let chips = [
            (Chip::All, "All", None, None),
            (
                Chip::Aliases,
                crate::i18n::ts!("automations-aliases"),
                Some(counts.aliases),
                Some(bootstrap_icons::AT),
            ),
            (
                Chip::Triggers,
                crate::i18n::ts!("automations-triggers"),
                Some(counts.triggers),
                Some(bootstrap_icons::LIGHTNING),
            ),
            (
                Chip::Hotkeys,
                crate::i18n::ts!("automations-hotkeys"),
                Some(counts.hotkeys),
                Some(bootstrap_icons::DPAD),
            ),
            (
                Chip::Folders,
                crate::i18n::ts!("automations-folders"),
                Some(counts.folders),
                Some(bootstrap_icons::FOLDER_PLUS),
            ),
            (
                Chip::Modules,
                crate::i18n::ts!("automations-modules-plural"),
                Some(counts.modules),
                Some(bootstrap_icons::FONTS),
            ),
            (
                Chip::Packages,
                crate::i18n::ts!("automations-packages-plural"),
                Some(counts.packages),
                Some(bootstrap_icons::BOUNDING_BOX),
            ),
        ];
        // A wrapping flow of chips: they reflow to as many rows as the sidebar
        // width needs instead of being clipped (the narrow sidebar couldn't fit
        // four chips on a fixed first row, which hid the Hotkeys chip).
        let mut items: Vec<Elem<'_>> = Vec::new();
        for (chip, label, count, icon) in chips {
            let selected = self.chip == chip;
            let mut inner = row![].spacing(6.0).align_y(Vertical::Center);
            if let Some(icon) = icon {
                inner = inner.push(
                    text(icon)
                        .font(fonts::BOOTSTRAP_ICONS)
                        .size(10.0)
                        .style(common::muted),
                );
            }
            // The label inherits the chip's text colour (bright when selected);
            // the count rides alongside as a quieter secondary, fading further
            // when it's zero so empty categories visibly recede.
            inner = inner.push(text(label).size(11.0));
            if let Some(count) = count {
                let count_style: fn(&Theme) -> text::Style = if count == 0 {
                    common::faint
                } else {
                    common::muted
                };
                inner = inner.push(text(count.to_string()).size(11.0).style(count_style));
            }
            items.push(
                button(inner)
                    .style(chip_style(selected))
                    .on_press(Message::SelectChip(chip))
                    .padding(Padding {
                        top: 4.0,
                        bottom: 4.0,
                        left: 9.0,
                        right: 9.0,
                    })
                    .into(),
            );
        }
        wrap_row(items).spacing(6.0, 6.0).into()
    }

    /// The contextual "Create new …" shortcut shown directly beneath the filter
    /// chips. When a single-type chip is active it offers a one-click create for
    /// that kind (the same action as the matching New-menu entry); the `All` chip
    /// has no single type, so it returns `None` and nothing is shown.
    ///
    /// A content-sized `subtle` pill, sized to echo the chips it sits under (the
    /// plus + label match the chip icon/label sizes and padding) so it reads as a
    /// quiet member of the chip region rather than a heavy full-width bar.
    fn create_new_button(&self) -> Option<Elem<'_>> {
        let (label, msg) = match self.chip {
            Chip::All => return None,
            Chip::Aliases => (crate::i18n::ts!("automation-alias"), Message::NewAlias),
            Chip::Triggers => (crate::i18n::ts!("automation-trigger"), Message::NewTrigger),
            Chip::Hotkeys => (crate::i18n::ts!("automation-hotkey"), Message::NewHotkey),
            Chip::Folders => (crate::i18n::ts!("automation-folder"), Message::NewFolder),
            Chip::Modules => (crate::i18n::ts!("automation-module"), Message::NewModule),
            Chip::Packages => (crate::i18n::ts!("automation-package"), Message::NewPackage),
        };
        Some(
            button(
                row![
                    text(bootstrap_icons::PLUS_LG)
                        .font(fonts::BOOTSTRAP_ICONS)
                        .size(10.0)
                        .style(common::muted),
                    text(crate::i18n::t!("automations-create-new", "kind" => label)).size(11.0),
                ]
                .spacing(6.0)
                .align_y(Vertical::Center),
            )
            .style(button_style::subtle)
            .on_press(msg)
            .padding(Padding {
                top: 4.0,
                bottom: 4.0,
                left: 9.0,
                right: 9.0,
            })
            .into(),
        )
    }

    // ---- the tree ----------------------------------------------------------

    fn tree(&self) -> Column<'_, Message, Theme> {
        let mut col = Column::new().spacing(3.0);
        let searching = !self.search.is_empty();

        // SCRIPTS.
        if matches!(
            self.chip,
            Chip::All | Chip::Aliases | Chip::Triggers | Chip::Hotkeys | Chip::Folders
        ) {
            let mut rows = Vec::new();
            self.build_script_rows(&self.scripts, 0, "", searching, &mut rows);
            if !rows.is_empty() {
                col = col.push(section_header(crate::i18n::ts!("automations-scripts")));
                for r in rows {
                    col = col.push(r);
                }
            }
        }

        // MODULES.
        if matches!(self.chip, Chip::All | Chip::Modules) && !self.modules.is_empty() {
            let mut rows: Vec<Elem> = Vec::new();
            for m in self
                .modules
                .iter()
                .filter(|m| self.name_matches(&m.subpath))
            {
                let selected = self.selection == Selection::Module(m.subpath.clone());
                let activation =
                    smudgy_core::models::modules::activation(&self.module_settings, &m.subpath);
                let executable = smudgy_core::models::modules::is_script_module(&m.subpath);
                let enabled = executable && activation.is_enabled_for(&self.profile_name);
                let enabled_count = self
                    .profile_names
                    .iter()
                    .filter(|profile| executable && activation.is_enabled_for(profile))
                    .count();
                rows.push(tree_row(
                    0,
                    None,
                    if self.module_state_error.is_some() {
                        NodeStatus::Error
                    } else if !executable {
                        NodeStatus::Neutral
                    } else if enabled {
                        NodeStatus::Ok
                    } else {
                        NodeStatus::Disabled
                    },
                    bootstrap_icons::FONTS,
                    &m.subpath,
                    selected,
                    Message::SelectModule(m.subpath.clone()),
                    (executable && self.profile_inventory_complete)
                        .then(|| partial_scope_badge(enabled_count, self.profile_names.len()))
                        .flatten(),
                ));
                if let Some(automations) = self.live.module(&m.subpath) {
                    self.build_creator_rows(
                        format!("module:{}", m.subpath),
                        automations,
                        1,
                        &mut rows,
                    );
                }
            }
            if !rows.is_empty() {
                col = col.push(section_header(crate::i18n::ts!(
                    "automations-modules-plural"
                )));
                for r in rows {
                    col = col.push(r);
                }
            }
        }

        // PACKAGES.
        if matches!(self.chip, Chip::All | Chip::Packages) {
            let mut rows = Vec::new();
            self.build_package_rows(&mut rows);
            if !rows.is_empty() {
                for r in rows {
                    col = col.push(r);
                }
            }
        }

        col
    }

    fn build_script_rows<'a>(
        &'a self,
        scripts: &'a BTreeMap<String, Script>,
        indent: usize,
        parent: &str,
        searching: bool,
        out: &mut Vec<Elem<'a>>,
    ) {
        // Folders first.
        for (name, script) in scripts {
            let Script::Folder(_, children) = script else {
                continue;
            };
            let path = if parent.is_empty() {
                name.clone()
            } else {
                format!("{parent}/{name}")
            };
            // When filtering to a single leaf type, skip folder chrome entirely.
            let leaf_only = matches!(self.chip, Chip::Aliases | Chip::Triggers | Chip::Hotkeys);
            let collapsed = !searching && self.collapsed_folders.contains(&path);
            let enabled = packages::is_package_effectively_enabled_for(
                &path,
                &self.packages,
                &self.profile_name,
            );
            let enabled_count = effective_folder_profile_count(
                &path,
                &self.packages,
                self.profile_names.as_slice(),
            );
            let status = if self.folder_state_error.is_some() {
                NodeStatus::Error
            } else if enabled {
                NodeStatus::Ok
            } else {
                NodeStatus::Disabled
            };
            if !leaf_only && self.chip != Chip::Folders {
                if self.folder_or_descendant_matches(&path, children) {
                    let selected = self.selection == Selection::Folder(path.clone());
                    out.push(tree_row(
                        indent,
                        Some((collapsed, path.clone())),
                        status,
                        bootstrap_icons::FOLDER_PLUS,
                        name,
                        selected,
                        Message::SelectFolder(path.clone()),
                        self.profile_inventory_complete
                            .then(|| partial_scope_badge(enabled_count, self.profile_names.len()))
                            .flatten(),
                    ));
                }
            } else if self.chip == Chip::Folders && self.name_matches(name) {
                let selected = self.selection == Selection::Folder(path.clone());
                out.push(tree_row(
                    indent,
                    None,
                    status,
                    bootstrap_icons::FOLDER_PLUS,
                    name,
                    selected,
                    Message::SelectFolder(path.clone()),
                    self.profile_inventory_complete
                        .then(|| partial_scope_badge(enabled_count, self.profile_names.len()))
                        .flatten(),
                ));
            }
            if !collapsed && self.chip != Chip::Folders {
                self.build_script_rows(children, indent + 1, &path, searching, out);
            }
        }
        // Leaves. An inner trigger renders under the trigger it is inside, not here.
        for (name, script) in scripts {
            if let Script::Trigger(t) = script
                && t.outer.is_some()
            {
                continue;
            }
            let icon = match script {
                Script::Alias(_) => bootstrap_icons::AT,
                Script::Trigger(_) => bootstrap_icons::LIGHTNING,
                Script::Hotkey(_) => bootstrap_icons::DPAD,
                Script::Folder(_, _) => continue,
            };
            if !self.leaf_passes_chip(script)
                || !(self.name_matches(name) || self.inner_trigger_matches(scripts, name))
            {
                continue;
            }
            let leaf_only = matches!(self.chip, Chip::Aliases | Chip::Triggers | Chip::Hotkeys);
            // A leaf sits at the SAME indent as its sibling folders, not one
            // deeper: the recursion already increments `indent` when it
            // descends into a folder's children, so adding +1 here pushed
            // root-level leaves in under a (possibly collapsed) root folder.
            let leaf_indent = if leaf_only { 0 } else { indent };
            self.build_trigger_leaf_rows(scripts, name, script, icon, leaf_indent, searching, out);
        }
    }

    /// One leaf row, then, for a trigger, the rows of the triggers inside it, nested one
    /// level deeper behind a twisty keyed `trigger:<name>` in the collapsed set.
    #[allow(clippy::too_many_arguments)]
    fn build_trigger_leaf_rows<'a>(
        &'a self,
        scripts: &'a BTreeMap<String, Script>,
        name: &'a str,
        script: &'a Script,
        icon: &'static str,
        indent: usize,
        searching: bool,
        out: &mut Vec<Elem<'a>>,
    ) {
        let key = ScriptKey {
            folder_name: script.folder_name().map(str::to_string),
            script_name: name.to_string(),
        };
        let selected = self.selection == Selection::Script(key.clone());
        let inner: Vec<(&'a String, &'a Script)> = scripts
            .iter()
            .filter(|(_, candidate)| {
                matches!(candidate, Script::Trigger(t) if t.outer.as_deref() == Some(name))
            })
            .collect();
        let twisty_key = format!("trigger:{name}");
        let collapsed = !searching && self.collapsed_folders.contains(&twisty_key);
        out.push(tree_row(
            indent,
            (!inner.is_empty()).then_some((collapsed, twisty_key)),
            self.script_status(script),
            icon,
            name,
            selected,
            Message::SelectScript(key),
            None,
        ));
        if collapsed {
            return;
        }
        for (inner_name, inner_script) in inner {
            if !(self.name_matches(inner_name) || self.inner_trigger_matches(scripts, inner_name)) {
                continue;
            }
            self.build_trigger_leaf_rows(
                scripts,
                inner_name,
                inner_script,
                bootstrap_icons::LIGHTNING,
                indent + 1,
                searching,
                out,
            );
        }
    }

    /// Whether any trigger inside `name` (at any depth, within the same folder) matches
    /// the search, so its outer stays visible.
    fn inner_trigger_matches(&self, scripts: &BTreeMap<String, Script>, name: &str) -> bool {
        scripts.iter().any(|(inner_name, candidate)| {
            matches!(candidate, Script::Trigger(t) if t.outer.as_deref() == Some(name))
                && (self.name_matches(inner_name)
                    || self.inner_trigger_matches(scripts, inner_name))
        })
    }

    fn build_package_rows<'a>(&'a self, out: &mut Vec<Elem<'a>>) {
        // A local package is the canonical package for its leaf name. Its remote lock record stays
        // on disk as a fallback, but it is not a second selectable package while the local folder
        // exists.
        let local_leaf_names = self
            .local_packages
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>();
        let package_lock = SharedPackageLock {
            packages: self.installed_packages.clone(),
        };

        // ---- INSTALLED (cloud) ----
        let mut installed_rows: Vec<Elem<'a>> = Vec::new();
        for pkg in &self.installed_packages {
            let spec = &pkg.specifier;
            if local_leaf_names.contains(&package_display_name(spec).to_ascii_lowercase()) {
                continue; // the LOCAL row is the canonical view; this record is fallback only
            }
            if !self.name_matches(package_display_name(spec))
                && !self.package_descendant_matches(spec)
            {
                continue;
            }
            let status = self.package_status(spec);
            let enabled_count = self
                .profile_names
                .iter()
                .filter(|profile| package_lock.is_effectively_enabled_for(spec, profile))
                .count();
            let scope_badge = || {
                self.profile_inventory_complete
                    .then(|| partial_scope_badge(enabled_count, self.profile_names.len()))
                    .flatten()
            };
            let selected = self.selection == Selection::InstalledPackage(spec.clone());
            let select = Message::SelectInstalledPackage(spec.clone());
            installed_rows.push(package_row(
                status,
                bootstrap_icons::CLOUD_CHECK,
                package_display_name(spec).to_string(),
                selected,
                select,
                scope_badge(),
            ));
            if let Some((owner, name)) = super::model::parse_specifier(spec)
                && let Some(automations) = self.live.package(&owner, &name)
            {
                self.build_creator_rows(
                    format!("package:{spec}"),
                    automations,
                    1,
                    &mut installed_rows,
                );
            }
            self.build_dep_rows(spec, 1, &mut installed_rows);
        }

        // ---- LOCAL (authored) ----
        let mut local_rows: Vec<Elem<'a>> = Vec::new();
        for name in &self.local_packages {
            if !self.name_matches(name) {
                continue;
            }
            let status = self.local_status(name);
            let selected = self.selection == Selection::OwnedPackage(name.clone());
            let select = Message::SelectOwnedPackage(name.clone());
            let own_spec = self.local_own_spec(name);
            let enabled_count = self
                .profile_names
                .iter()
                .filter(|profile| package_lock.is_effectively_enabled_for(&own_spec, profile))
                .count();
            let scope_badge = || {
                self.profile_inventory_complete
                    .then(|| partial_scope_badge(enabled_count, self.profile_names.len()))
                    .flatten()
            };
            local_rows.push(package_row(
                status,
                bootstrap_icons::BOUNDING_BOX,
                name.clone(),
                selected,
                select,
                scope_badge(),
            ));
            self.build_dep_rows(&self.local_own_spec(name), 1, &mut local_rows);
        }

        if !installed_rows.is_empty() {
            out.push(section_header(crate::i18n::ts!(
                "automations-packages-plural"
            )));
            out.append(&mut installed_rows);
        }
        if !local_rows.is_empty() {
            out.push(section_header(crate::i18n::ts!("automations-local")));
            out.append(&mut local_rows);
        }
    }

    fn build_dep_rows<'a>(&'a self, parent: &str, indent: usize, out: &mut Vec<Elem<'a>>) {
        let Some(edges) = self.graph.requires.get(parent) else {
            return;
        };
        for edge in edges {
            let spec = &edge.specifier;
            // Key the selection to this dependency *reference* (parent + spec), not the package
            // itself, so clicking it highlights only this row — not the package's own top-level
            // row when it's also directly installed.
            let selection = Selection::Dependency {
                parent: parent.to_string(),
                spec: spec.clone(),
            };
            let selected = self.selection == selection;
            // A nested dep row follows its parent's context: grey it once `parent` is off, rather
            // than lighting up on the dep's global state (a separately-installed dep keeps its own
            // top-level row lit). When the edge IS live, defer to `package_status` so an
            // update-blocked dep still surfaces its Warning dot rather than a flat Ok.
            let status = if self.graph.dep_edge_active(parent, spec) {
                if self.blocked_updates.contains(spec) {
                    NodeStatus::Warning
                } else {
                    NodeStatus::Ok
                }
            } else {
                NodeStatus::Disabled
            };
            out.push(tree_row(
                indent,
                None,
                status,
                bootstrap_icons::CLOUD_CHECK,
                package_display_name(spec),
                selected,
                Message::SelectDependency {
                    parent: parent.to_string(),
                    spec: spec.clone(),
                },
                Some(if edge.kind == DependencyKind::Requires {
                    common::required_tag()
                } else {
                    common::dep_tag()
                }),
            ));
        }
    }

    /// Render a creator's script-created automations nested under its module/package node: a
    /// collapsible "N automations" toggle, then (when expanded) the first
    /// `CREATOR_SHOW_LIMIT` alias/trigger leaves, with a "show more" for the rest. Selecting a
    /// leaf opens its read-only detail pane (pattern + body).
    fn build_creator_rows<'a>(
        &'a self,
        creator_id: String,
        automations: &'a CreatorAutomations,
        indent: usize,
        out: &mut Vec<Elem<'a>>,
    ) {
        let total = automations.aliases.len() + automations.triggers.len();
        if total == 0 {
            return;
        }
        let expanded = self.expanded_creators.contains(&creator_id);
        out.push(creator_toggle_row(
            indent,
            expanded,
            total,
            creator_id.clone(),
        ));
        if !expanded {
            return;
        }
        let limit = if self.show_all_creators.contains(&creator_id) {
            usize::MAX
        } else {
            CREATOR_SHOW_LIMIT
        };
        let status = |enabled: bool| {
            if enabled {
                NodeStatus::Ok
            } else {
                NodeStatus::Disabled
            }
        };
        let mut shown = 0usize;
        let push_leaf = |kind: AutomationKind,
                         icon: &'static str,
                         name: &str,
                         enabled: bool,
                         out: &mut Vec<Elem<'a>>| {
            let selection = Selection::CreatorAutomation {
                creator_id: creator_id.clone(),
                kind,
                name: name.to_string(),
            };
            let selected = self.selection == selection;
            out.push(tree_row(
                indent + 1,
                None,
                status(enabled),
                icon,
                name,
                selected,
                Message::SelectCreatorAutomation {
                    creator_id: creator_id.clone(),
                    kind,
                    name: name.to_string(),
                },
                None,
            ));
        };
        for (name, entry) in &automations.aliases {
            if shown >= limit {
                break;
            }
            push_leaf(
                AutomationKind::Alias,
                bootstrap_icons::AT,
                name,
                entry.enabled,
                out,
            );
            shown += 1;
        }
        for (name, entry) in &automations.triggers {
            if shown >= limit {
                break;
            }
            push_leaf(
                AutomationKind::Trigger,
                bootstrap_icons::LIGHTNING,
                name,
                entry.enabled,
                out,
            );
            shown += 1;
        }
        if shown < total {
            out.push(show_more_row(indent + 1, total - shown, creator_id));
        }
    }

    // ---- footer ------------------------------------------------------------

    fn footer(&self) -> Elem<'_> {
        let errors = self.error_count();
        let status: Elem = if errors > 0 {
            row![
                text(format!(
                    "{errors} error{}",
                    if errors == 1 { "" } else { "s" }
                ))
                .size(11.0)
                .style(common::danger),
            ]
            .into()
        } else {
            row![].into()
        };
        container(
            row![
                common::status_dot(NodeStatus::Ok),
                text(self.server_name.clone()).size(12.0),
                iced::widget::space::horizontal(),
                status,
                button(
                    text(common::palette_shortcut_label())
                        .size(11.0)
                        .style(common::muted)
                )
                .style(button_style::subtle)
                .on_press(Message::OpenPalette)
                .padding(Padding {
                    top: 2.0,
                    bottom: 2.0,
                    left: 6.0,
                    right: 6.0,
                }),
            ]
            .spacing(8.0)
            .align_y(Vertical::Center),
        )
        .width(Length::Fill)
        .padding(Padding {
            top: 8.0,
            bottom: 2.0,
            left: 4.0,
            right: 0.0,
        })
        .into()
    }

    // ---- filtering helpers -------------------------------------------------

    fn name_matches(&self, name: &str) -> bool {
        self.search.is_empty() || name.to_lowercase().contains(&self.search.to_lowercase())
    }

    fn leaf_passes_chip(&self, script: &Script) -> bool {
        match self.chip {
            Chip::All => true,
            Chip::Aliases => matches!(script, Script::Alias(_)),
            Chip::Triggers => matches!(script, Script::Trigger(_)),
            Chip::Hotkeys => matches!(script, Script::Hotkey(_)),
            _ => false,
        }
    }

    fn folder_or_descendant_matches(
        &self,
        _path: &str,
        children: &BTreeMap<String, Script>,
    ) -> bool {
        if self.search.is_empty() {
            return true;
        }
        fn rec(window: &AutomationsWindow, scripts: &BTreeMap<String, Script>) -> bool {
            scripts.iter().any(|(name, script)| {
                window.name_matches(name)
                    || matches!(script, Script::Folder(_, children) if rec(window, children))
            })
        }
        rec(self, children)
    }

    fn package_descendant_matches(&self, spec: &str) -> bool {
        if self.search.is_empty() {
            return false;
        }
        self.graph
            .requires
            .get(spec)
            .into_iter()
            .flatten()
            .any(|e| self.name_matches(package_display_name(&e.specifier)))
    }

    fn counts(&self) -> Counts {
        let mut c = Counts::default();
        fn walk(scripts: &BTreeMap<String, Script>, c: &mut Counts) {
            for script in scripts.values() {
                match script {
                    Script::Alias(_) => c.aliases += 1,
                    Script::Trigger(_) => c.triggers += 1,
                    Script::Hotkey(_) => c.hotkeys += 1,
                    Script::Folder(_, children) => {
                        c.folders += 1;
                        walk(children, c);
                    }
                }
            }
        }
        walk(&self.scripts, &mut c);
        c.modules = self.modules.len();
        // Distinct logical packages: a local leaf replaces every same-leaf remote fallback row.
        let local_leaf_names = self
            .local_packages
            .iter()
            .map(|name| name.to_ascii_lowercase())
            .collect::<std::collections::HashSet<_>>();
        c.packages = self
            .installed_packages
            .iter()
            .filter(|package| {
                !local_leaf_names
                    .contains(&package_display_name(&package.specifier).to_ascii_lowercase())
            })
            .count()
            + self.local_packages.len();
        c
    }

    fn error_count(&self) -> usize {
        let mut n = 0;
        fn walk(window: &AutomationsWindow, scripts: &BTreeMap<String, Script>, n: &mut usize) {
            for script in scripts.values() {
                if let Script::Folder(_, children) = script {
                    walk(window, children, n);
                } else if window.script_status(script) == NodeStatus::Error {
                    *n += 1;
                }
            }
        }
        walk(self, &self.scripts, &mut n);
        n
    }
}

#[derive(Default)]
struct Counts {
    aliases: usize,
    triggers: usize,
    hotkeys: usize,
    folders: usize,
    modules: usize,
    packages: usize,
}

// ---- row builders ----------------------------------------------------------

fn section_header<'a>(label: &str) -> Elem<'a> {
    container(common::section_label(label))
        .padding(Padding {
            top: 10.0,
            bottom: 2.0,
            left: 4.0,
            right: 0.0,
        })
        .into()
}

/// A filter-chip button: one consistent hairline-pill silhouette for every
/// chip, with a brighter fill + border when selected.
fn chip_style(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme: &Theme, status: button::Status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        // Keep the selected resting fill clearly above the unselected hover
        // fill so the two states read apart from the fill alone, before the
        // border/text cues register.
        let fill = if selected {
            if hovered { 0.20 } else { 0.16 }
        } else if hovered {
            0.08
        } else {
            0.02
        };
        button::Style {
            background: Some(Background::Color(Color::from_rgba8(255, 255, 255, fill))),
            border: Border {
                color: if selected {
                    Color::from_rgba8(255, 255, 255, 0.22)
                } else {
                    theme.styles.general.border
                },
                width: 1.0,
                radius: 7.0.into(),
            },
            text_color: if selected {
                theme.styles.text.normal
            } else {
                theme.styles.text.normal.scale_alpha(0.7)
            },
            ..Default::default()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn tree_row<'a>(
    indent: usize,
    twisty: Option<(bool, String)>,
    status: NodeStatus,
    icon: &'static str,
    label: &str,
    selected: bool,
    on_press: Message,
    trailing: Option<Elem<'a>>,
) -> Elem<'a> {
    let indent_px = (indent as f32) * 16.0;
    let mut inner = row![].spacing(7.0).align_y(Vertical::Center);
    if let Some((collapsed, path)) = twisty {
        let chevron = if collapsed {
            bootstrap_icons::CHEVRON_RIGHT
        } else {
            bootstrap_icons::CHEVRON_DOWN
        };
        inner = inner.push(
            button(
                text(chevron)
                    .font(fonts::BOOTSTRAP_ICONS)
                    .size(10.0)
                    .style(common::muted),
            )
            .style(button_style::list_item)
            .on_press(Message::ToggleFolderExpanded(path))
            .padding(Padding {
                top: 0.0,
                bottom: 0.0,
                left: 2.0,
                right: 2.0,
            }),
        );
    } else {
        inner = inner.push(iced::widget::space::horizontal().width(Length::Fixed(14.0)));
    }
    inner = inner.push(common::status_dot(status));
    // Disabled nodes dim their icon *and* label, not just the status dot, so
    // the on/off state reads at a glance. The label only drops to `muted` (not
    // `faint`) so a disabled row still reads — `faint` on a 13px label is too
    // low-contrast to be the primary text of a row.
    let disabled = status == NodeStatus::Disabled;
    let icon_style: fn(&Theme) -> text::Style = if disabled {
        common::faint
    } else {
        common::muted
    };
    let label_style: fn(&Theme) -> text::Style = if disabled {
        common::muted
    } else {
        common::regular
    };
    inner = inner.push(
        text(icon)
            .font(fonts::BOOTSTRAP_ICONS)
            .size(13.0)
            .style(icon_style),
    );
    inner = inner.push(text(label.to_string()).size(13.0).style(label_style));
    if let Some(trailing) = trailing {
        inner = inner.push(iced::widget::space::horizontal());
        inner = inner.push(trailing);
    }

    let btn = button(inner)
        .style(if selected {
            button_style::list_item_selected
        } else {
            button_style::list_item
        })
        .on_press(on_press)
        .width(Length::Fill)
        .padding(Padding {
            top: 3.0,
            bottom: 3.0,
            left: 6.0 + indent_px,
            right: 6.0,
        });
    btn.into()
}

/// A package navigator row without a folder twisty. Local leaf-name overrides are automatic, so
/// package rows deliberately have no same-name provider selector.
#[allow(clippy::too_many_arguments)]
fn package_row<'a>(
    status: NodeStatus,
    icon: &'static str,
    label: String,
    selected: bool,
    on_press: Message,
    trailing: Option<Elem<'a>>,
) -> Elem<'a> {
    let mut inner = row![iced::widget::space::horizontal().width(Length::Fixed(14.0))]
        .spacing(7.0)
        .align_y(Vertical::Center);
    inner = inner.push(common::status_dot(status));
    let disabled = status == NodeStatus::Disabled;
    let icon_style: fn(&Theme) -> text::Style = if disabled {
        common::faint
    } else {
        common::muted
    };
    let label_style: fn(&Theme) -> text::Style = if disabled {
        common::muted
    } else {
        common::regular
    };
    inner = inner.push(
        text(icon)
            .font(fonts::BOOTSTRAP_ICONS)
            .size(13.0)
            .style(icon_style),
    );
    inner = inner.push(text(label).size(13.0).style(label_style));
    if let Some(trailing) = trailing {
        inner = inner.push(iced::widget::space::horizontal());
        inner = inner.push(trailing);
    }
    button(inner)
        .style(if selected {
            button_style::list_item_selected
        } else {
            button_style::list_item
        })
        .on_press(on_press)
        .width(Length::Fill)
        .padding(Padding {
            top: 3.0,
            bottom: 3.0,
            left: 6.0,
            right: 6.0,
        })
        .into()
}

/// Threshold for how many of a creator's automations render before a "show more" row.
const CREATOR_SHOW_LIMIT: usize = 100;

/// The collapsible header for a creator's nested automations: a chevron + the count.
fn creator_toggle_row<'a>(
    indent: usize,
    expanded: bool,
    total: usize,
    creator_id: String,
) -> Elem<'a> {
    let chevron = if expanded {
        bootstrap_icons::CHEVRON_DOWN
    } else {
        bootstrap_icons::CHEVRON_RIGHT
    };
    let label = crate::i18n::t!("automations-created-count", "count" => total as i64);
    button(
        row![
            text(chevron)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(10.0)
                .style(common::muted),
            text(label).size(12.0).style(common::muted),
        ]
        .spacing(7.0)
        .align_y(Vertical::Center),
    )
    .style(button_style::list_item)
    .on_press(Message::ToggleCreator(creator_id))
    .width(Length::Fill)
    .padding(Padding {
        top: 3.0,
        bottom: 3.0,
        left: 6.0 + (indent as f32) * 16.0,
        right: 6.0,
    })
    .into()
}

/// The "show N more…" row revealing a creator's remaining automations beyond the cap.
fn show_more_row<'a>(indent: usize, remaining: usize, creator_id: String) -> Elem<'a> {
    button(
        text(crate::i18n::t!("automations-show-more", "count" => remaining as i64))
            .size(12.0)
            .style(common::faint),
    )
    .style(button_style::list_item)
    .on_press(Message::ToggleCreatorShowAll(creator_id))
    .width(Length::Fill)
    .padding(Padding {
        top: 3.0,
        bottom: 3.0,
        left: 6.0 + (indent as f32) * 16.0,
        right: 6.0,
    })
    .into()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use smudgy_core::models::packages::{PackageNode, PackageTree};
    use smudgy_core::models::profile_activation::ProfileActivation;

    use super::{effective_folder_profile_count, is_partial_scope};

    fn folder(
        activation: ProfileActivation,
        children: HashMap<String, PackageNode>,
    ) -> PackageNode {
        PackageNode {
            enabled: activation.legacy_enabled(),
            activation: Some(activation),
            children,
        }
    }

    #[test]
    fn scope_hint_appears_only_for_a_nonempty_proper_subset() {
        assert!(!is_partial_scope(0, 0));
        assert!(!is_partial_scope(0, 3));
        assert!(is_partial_scope(1, 3));
        assert!(is_partial_scope(2, 3));
        assert!(!is_partial_scope(3, 3));
    }

    #[test]
    fn nested_folder_scope_count_includes_ancestor_activation() {
        let profiles = vec!["Main".to_string(), "Alt".to_string(), "Test".to_string()];
        let main_only = ProfileActivation::Selected {
            profiles: ["Main".to_string()].into_iter().collect(),
        };
        let tree = PackageTree::from([
            (
                "partial-parent".to_string(),
                folder(
                    main_only.clone(),
                    HashMap::from([(
                        "all-child".to_string(),
                        folder(ProfileActivation::All, HashMap::new()),
                    )]),
                ),
            ),
            (
                "off-parent".to_string(),
                folder(
                    ProfileActivation::None,
                    HashMap::from([(
                        "selected-child".to_string(),
                        folder(main_only, HashMap::new()),
                    )]),
                ),
            ),
        ]);

        assert_eq!(
            effective_folder_profile_count("partial-parent/all-child", &tree, &profiles),
            1
        );
        assert_eq!(
            effective_folder_profile_count("off-parent/selected-child", &tree, &profiles),
            0
        );
    }
}
