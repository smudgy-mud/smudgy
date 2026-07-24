//! The navigator sidebar: New menu, search, filter chips, the status-dotted
//! tree (SCRIPTS / MODULES / PACKAGES with dependency nesting), and the footer.

use std::collections::{BTreeMap, HashMap};

use iced::alignment::Vertical;
use iced::widget::{Column, button, column, container, row, scrollable, text, text_input};
use iced::{Background, Border, Color, Length, Padding};

use smudgy_core::models::packages;
use smudgy_core::session::runtime::AutomationKind;

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::Theme;
use crate::theme::builtins::button as button_style;
use crate::widgets::wrap_row::wrap_row;

use super::common;
use super::model::{CreatorAutomations, NodeStatus, Script, ScriptKey, package_display_name};
use super::{AutomationsWindow, Chip, Elem, Message, Selection};

const SIDEBAR_WIDTH: f32 = 282.0;

impl AutomationsWindow {
    pub(super) fn view_sidebar(&self) -> Elem<'_> {
        let new_button = button(
            row![
                text(bootstrap_icons::PLUS_LG)
                    .font(fonts::BOOTSTRAP_ICONS)
                    .size(13.0),
                text("New").size(14.0),
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
            text_input("Search automations…", &self.search)
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
                "Aliases",
                Some(counts.aliases),
                Some(bootstrap_icons::AT),
            ),
            (
                Chip::Triggers,
                "Triggers",
                Some(counts.triggers),
                Some(bootstrap_icons::LIGHTNING),
            ),
            (
                Chip::Hotkeys,
                "Hotkeys",
                Some(counts.hotkeys),
                Some(bootstrap_icons::DPAD),
            ),
            (
                Chip::Folders,
                "Folders",
                Some(counts.folders),
                Some(bootstrap_icons::FOLDER_PLUS),
            ),
            (
                Chip::Modules,
                "Modules",
                Some(counts.modules),
                Some(bootstrap_icons::FONTS),
            ),
            (
                Chip::Packages,
                "Packages",
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
            Chip::Aliases => ("Alias", Message::NewAlias),
            Chip::Triggers => ("Trigger", Message::NewTrigger),
            Chip::Hotkeys => ("Hotkey", Message::NewHotkey),
            Chip::Folders => ("Folder", Message::NewFolder),
            Chip::Modules => ("Module", Message::NewModule),
            Chip::Packages => ("Package", Message::NewPackage),
        };
        Some(
            button(
                row![
                    text(bootstrap_icons::PLUS_LG)
                        .font(fonts::BOOTSTRAP_ICONS)
                        .size(10.0)
                        .style(common::muted),
                    text(format!("Create new {label}")).size(11.0),
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
                col = col.push(section_header("Scripts"));
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
                rows.push(tree_row(
                    0,
                    None,
                    NodeStatus::Ok,
                    bootstrap_icons::FONTS,
                    &m.subpath,
                    selected,
                    Message::SelectModule(m.subpath.clone()),
                    None,
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
                col = col.push(section_header("Modules"));
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
            let enabled = packages::is_package_effectively_enabled(&path, &self.packages);
            let status = if enabled {
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
                        None,
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
                    None,
                ));
            }
            if !collapsed && self.chip != Chip::Folders {
                self.build_script_rows(children, indent + 1, &path, searching, out);
            }
        }
        // Leaves.
        for (name, script) in scripts {
            let icon = match script {
                Script::Alias(_) => bootstrap_icons::AT,
                Script::Trigger(_) => bootstrap_icons::LIGHTNING,
                Script::Hotkey(_) => bootstrap_icons::DPAD,
                Script::Folder(_, _) => continue,
            };
            if !self.leaf_passes_chip(script) || !self.name_matches(name) {
                continue;
            }
            let leaf_only = matches!(self.chip, Chip::Aliases | Chip::Triggers | Chip::Hotkeys);
            let key = ScriptKey {
                folder_name: script.folder_name().map(str::to_string),
                script_name: name.clone(),
            };
            let selected = self.selection == Selection::Script(key.clone());
            out.push(tree_row(
                // A leaf sits at the SAME indent as its sibling folders, not one
                // deeper: the recursion already increments `indent` when it
                // descends into a folder's children, so adding +1 here pushed
                // root-level leaves in under a (possibly collapsed) root folder.
                if leaf_only { 0 } else { indent },
                None,
                self.script_status(script),
                icon,
                name,
                selected,
                Message::SelectScript(key),
                None,
            ));
        }
    }

    fn build_package_rows<'a>(&'a self, out: &mut Vec<Elem<'a>>) {
        // Installed specs that are a local package's OWN specifier are represented by the LOCAL row,
        // not a second INSTALLED row — this kills the active-fork double-count.
        let local_own_specs = self.local_own_specs();

        // Same-name groups: every installed + local package sharing a leaf name (case-folded like
        // the filesystem). Only one member of a group should run at a time, so a grouped row gets a
        // radio to switch which is live. Grouped purely by leaf name — fork provenance is no longer
        // tracked, so a local `boo` groups with an installed `bar/boo` simply because they're both
        // "boo". A self-fork keeping its name shares a single specifier slot (its installed row is
        // suppressed above), so it's a one-member group with no radio.
        let mut groups: HashMap<String, Vec<String>> = HashMap::new();
        for pkg in &self.installed_packages {
            if local_own_specs.contains(&pkg.specifier) {
                continue; // represented by its LOCAL row
            }
            groups
                .entry(package_display_name(&pkg.specifier).to_ascii_lowercase())
                .or_default()
                .push(pkg.specifier.clone());
        }
        for name in &self.local_packages {
            groups
                .entry(name.to_ascii_lowercase())
                .or_default()
                .push(self.local_own_spec(name));
        }
        // The OTHER members of the group `spec` belongs to (the radio's siblings to disable);
        // empty when `spec` is a lone package, in which case no radio is shown.
        let siblings_of = |spec: &str, leaf: &str| -> Vec<String> {
            groups
                .get(&leaf.to_ascii_lowercase())
                .into_iter()
                .flatten()
                .filter(|member| member.as_str() != spec)
                .cloned()
                .collect()
        };

        // ---- INSTALLED (cloud) ----
        let mut installed_rows: Vec<Elem<'a>> = Vec::new();
        for pkg in &self.installed_packages {
            let spec = &pkg.specifier;
            if local_own_specs.contains(spec) {
                continue; // shown as a LOCAL row instead
            }
            if !self.name_matches(package_display_name(spec))
                && !self.package_descendant_matches(spec)
            {
                continue;
            }
            let status = self.package_status(spec);
            let selected = self.selection == Selection::InstalledPackage(spec.clone());
            let select = Message::SelectInstalledPackage(spec.clone());
            let siblings = siblings_of(spec, package_display_name(spec));
            if siblings.is_empty() {
                installed_rows.push(package_row(
                    None,
                    status,
                    bootstrap_icons::CLOUD_CHECK,
                    package_display_name(spec).to_string(),
                    selected,
                    select,
                    None,
                ));
            } else {
                // A same-name group member: owner-qualify the label and give it a radio so the
                // user can pick which "<leaf>" is live.
                let active = self.graph.effectively_enabled(spec);
                let on_activate = (!active).then(|| Message::SetActiveMember {
                    target_spec: spec.clone(),
                    siblings,
                });
                let label = super::model::parse_specifier(spec).map_or_else(
                    || package_display_name(spec).to_string(),
                    |(o, n)| format!("{o}/{n}"),
                );
                installed_rows.push(package_row(
                    Some((active, on_activate)),
                    status,
                    bootstrap_icons::CLOUD_CHECK,
                    label,
                    selected,
                    select,
                    None,
                ));
            }
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
            let siblings = siblings_of(&own_spec, name);
            if siblings.is_empty() {
                // Lone local: no in-tree toggle — enable/disable lives in its pane like cloud
                // packages.
                local_rows.push(package_row(
                    None,
                    status,
                    bootstrap_icons::BOUNDING_BOX,
                    name.clone(),
                    selected,
                    select,
                    None,
                ));
            } else {
                // Shares a name with an installed package: give it a radio within the group.
                let active = self.local_active(name);
                let on_activate = (!active).then(|| Message::SetActiveMember {
                    target_spec: own_spec,
                    siblings,
                });
                local_rows.push(package_row(
                    Some((active, on_activate)),
                    status,
                    bootstrap_icons::BOUNDING_BOX,
                    name.clone(),
                    selected,
                    select,
                    None,
                ));
            }
            self.build_dep_rows(&format!("local:{name}"), 1, &mut local_rows);
        }

        if !installed_rows.is_empty() {
            out.push(section_header("Packages"));
            out.append(&mut installed_rows);
        }
        if !local_rows.is_empty() {
            out.push(section_header("Local"));
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
                self.package_status(spec)
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
                Some(common::dep_tag()),
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
        // Distinct logical packages: installed (minus any that are a local's own-spec install, to
        // avoid double-counting an active fork) + local.
        let own = self.local_own_specs();
        c.packages = self
            .installed_packages
            .iter()
            .filter(|p| !own.contains(&p.specifier))
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

/// A package navigator row: like `tree_row`, but the left gutter carries an optional same-name
/// radio (active U+25C9 / inactive U+25CB clickable) instead of a twisty. Used for the INSTALLED
/// and LOCAL package rows.
#[allow(clippy::too_many_arguments)]
fn package_row<'a>(
    radio: Option<(bool, Option<Message>)>,
    status: NodeStatus,
    icon: &'static str,
    label: String,
    selected: bool,
    on_press: Message,
    trailing: Option<Elem<'a>>,
) -> Elem<'a> {
    let mut inner = row![].spacing(7.0).align_y(Vertical::Center);
    // Left gutter: the active member's marker, an inactive member's "switch to this" button, or a
    // spacer matching tree_row's no-twisty gap for non-colliding rows.
    match radio {
        Some((true, _)) => {
            inner = inner.push(
                container(text("\u{25C9}").size(12.0).style(common::success)).padding(Padding {
                    top: 0.0,
                    bottom: 0.0,
                    left: 2.0,
                    right: 2.0,
                }),
            );
        }
        Some((false, on_activate)) => {
            inner = inner.push(
                button(text("\u{25CB}").size(12.0).style(common::muted))
                    .style(button_style::list_item)
                    .on_press_maybe(on_activate)
                    .padding(Padding {
                        top: 0.0,
                        bottom: 0.0,
                        left: 2.0,
                        right: 2.0,
                    }),
            );
        }
        None => {
            inner = inner.push(iced::widget::space::horizontal().width(Length::Fixed(14.0)));
        }
    }
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
    let label = format!("{total} automation{}", if total == 1 { "" } else { "s" });
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
        text(format!("Show {remaining} more…"))
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
