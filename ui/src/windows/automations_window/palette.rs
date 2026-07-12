//! The Ctrl/⌘+P command palette: a centered overlay with a search input over a
//! grouped, keyboard-navigable list of Create / Go / Jump-to actions.

use std::collections::BTreeMap;

use iced::Task;
use iced::alignment::Vertical;
use iced::widget::{Column, button, column, container, mouse_area, row, scrollable, text, text_input};
use iced::widget::{Id, operation};
use iced::{Background, Length, Padding};

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::Theme;
use crate::theme::builtins::button as button_style;
use crate::update::Update;

use smudgy_core::models::packages;

use super::common;
use super::model::{NodeStatus, Script, ScriptKey, package_display_name};
use super::{AutomationsWindow, Elem, Event, Message, Selection};

fn palette_input_id() -> Id {
    Id::new("automations-palette-input")
}

/// One runnable palette entry.
struct Item {
    group: &'static str,
    label: String,
    status: Option<NodeStatus>,
    kind: Option<&'static str>,
    message: Message,
}

impl AutomationsWindow {
    pub(super) fn focus_palette(&self) -> Task<Message> {
        operation::focus(palette_input_id())
    }

    pub(super) fn palette_move(&mut self, delta: i32) -> Update<Message, Event> {
        if !self.palette_open {
            return Update::none();
        }
        let len = self.palette_items().len() as i32;
        if len == 0 {
            self.palette_cursor = 0;
            return Update::none();
        }
        let next = (self.palette_cursor as i32 + delta).rem_euclid(len);
        self.palette_cursor = next as usize;
        Update::none()
    }

    pub(super) fn palette_run_active(&mut self) -> Update<Message, Event> {
        if !self.palette_open {
            return Update::none();
        }
        let items = self.palette_items();
        let Some(item) = items.into_iter().nth(self.palette_cursor) else {
            return Update::none();
        };
        self.palette_open = false;
        Update::with_task(Task::done(item.message))
    }

    /// The filtered, ordered palette items (Create → Go → Jump).
    fn palette_items(&self) -> Vec<Item> {
        let mut items = vec![
            Item { group: "Create", label: "Create alias".into(), status: None, kind: None, message: Message::NewAlias },
            Item { group: "Create", label: "Create trigger".into(), status: None, kind: None, message: Message::NewTrigger },
            Item { group: "Create", label: "Create hotkey".into(), status: None, kind: None, message: Message::NewHotkey },
            Item { group: "Create", label: "Create folder".into(), status: None, kind: None, message: Message::NewFolder },
            Item { group: "Create", label: "Create module".into(), status: None, kind: None, message: Message::NewModule },
            Item { group: "Create", label: "Create package".into(), status: None, kind: None, message: Message::NewPackage },
            Item { group: "Go", label: "Discover packages".into(), status: None, kind: None, message: Message::OpenDiscover },
            Item { group: "Go", label: "Private & shared packages".into(), status: None, kind: None, message: Message::OpenShared },
            Item { group: "Go", label: "Session store & events".into(), status: None, kind: None, message: Message::OpenStoreInspector },
            Item { group: "Go", label: "Reload all scripts".into(), status: None, kind: None, message: Message::Reload },
        ];

        // Move: when a script is selected (and thus open in the editor), offer a
        // destination per folder, plus top level. Running one re-homes the script
        // via the same `SetScriptFolder` handler the editor's Folder picker uses.
        if let Selection::Script(key) = &self.selection {
            let subject = &key.script_name;
            let current = key.folder_name.as_deref();
            if current.is_some() {
                items.push(Item {
                    group: "Move",
                    label: format!("Move {subject} to top level"),
                    status: None,
                    kind: Some("folder"),
                    message: Message::SetScriptFolder(None),
                });
            }
            for path in self.all_folder_paths() {
                if current == Some(path.as_str()) {
                    continue;
                }
                let status = if packages::is_package_effectively_enabled(&path, &self.packages) {
                    NodeStatus::Ok
                } else {
                    NodeStatus::Disabled
                };
                items.push(Item {
                    group: "Move",
                    label: format!("Move {subject} to {path}"),
                    status: Some(status),
                    kind: Some("folder"),
                    message: Message::SetScriptFolder(Some(path)),
                });
            }
        }

        // Jump to: scripts + folders + packages.
        collect_jump(&self.scripts, "", self, &mut items);
        for pkg in &self.installed_packages {
            items.push(Item {
                group: "Jump to",
                label: package_display_name(&pkg.specifier).to_string(),
                status: Some(self.package_status_for_palette(&pkg.specifier)),
                kind: Some("package"),
                message: Message::SelectInstalledPackage(pkg.specifier.clone()),
            });
        }
        for name in &self.local_packages {
            items.push(Item {
                group: "Jump to",
                label: name.clone(),
                status: Some(NodeStatus::Ok),
                kind: Some("package"),
                message: Message::SelectOwnedPackage(name.clone()),
            });
        }

        let q = self.palette_query.to_lowercase();
        if q.is_empty() {
            return items;
        }
        items
            .into_iter()
            .filter(|i| {
                i.label.to_lowercase().contains(&q) || i.group.to_lowercase().contains(&q)
            })
            .collect()
    }

    fn package_status_for_palette(&self, spec: &str) -> NodeStatus {
        if self.graph.effectively_enabled(spec) {
            NodeStatus::Ok
        } else {
            NodeStatus::Disabled
        }
    }

    pub(super) fn view_palette(&self) -> Elem<'_> {
        // Backdrop.
        let backdrop = mouse_area(
            container(iced::widget::space::vertical())
                .width(Length::Fill)
                .height(Length::Fill)
                .style(|theme: &Theme| iced::widget::container::Style {
                    background: Some(Background::Color(
                        theme.styles.general.overlay_background,
                    )),
                    ..Default::default()
                }),
        )
        .on_press(Message::ClosePalette);

        // Items list.
        let items = self.palette_items();
        let mut list = Column::new().spacing(1.0);
        let mut last_group = "";
        for (index, item) in items.iter().enumerate() {
            if item.group != last_group {
                last_group = item.group;
                list = list.push(
                    container(common::section_label(item.group)).padding(Padding {
                        top: 8.0,
                        bottom: 2.0,
                        left: 6.0,
                        right: 6.0,
                    }),
                );
            }
            let active = index == self.palette_cursor;
            let mut content = row![].spacing(8.0).align_y(Vertical::Center);
            if let Some(status) = item.status {
                content = content.push(common::status_dot(status));
            }
            content = content.push(text(item.label.clone()).size(13.0));
            content = content.push(iced::widget::space::horizontal());
            if let Some(kind) = item.kind {
                content = content.push(text(kind.to_string()).size(11.0).style(common::faint));
            }
            list = list.push(
                button(content)
                    .style(if active {
                        button_style::list_item_selected
                    } else {
                        button_style::list_item
                    })
                    .on_press(Message::PaletteRunItem(index))
                    .width(Length::Fill)
                    .padding(Padding {
                        top: 6.0,
                        bottom: 6.0,
                        left: 8.0,
                        right: 8.0,
                    }),
            );
        }

        let card = container(
            column![
                row![
                    text(bootstrap_icons::CURSOR).font(fonts::BOOTSTRAP_ICONS).size(13.0).style(common::faint),
                    text_input("Type a command or search…", &self.palette_query)
                        .id(palette_input_id())
                        .on_input(Message::PaletteInput)
                        .on_submit(Message::PaletteRun)
                        .size(15.0),
                    text("Esc").size(11.0).style(common::faint),
                ]
                .spacing(8.0)
                .align_y(Vertical::Center),
                iced::widget::rule::horizontal(1.0),
                scrollable(list).height(Length::Fixed(360.0)),
            ]
            .spacing(10.0),
        )
        .width(Length::Fixed(560.0))
        .padding(14.0)
        .style(|theme: &Theme| iced::widget::container::Style {
            background: Some(theme.styles.modal.body_background),
            border: theme.styles.modal.body_border,
            shadow: theme.styles.modal.shadow,
            ..Default::default()
        });

        let centered = container(card)
            .width(Length::Fill)
            .height(Length::Fill)
            .align_x(iced::alignment::Horizontal::Center)
            .padding(Padding {
                top: 80.0,
                bottom: 0.0,
                left: 0.0,
                right: 0.0,
            });

        iced::widget::stack![backdrop, centered]
            .width(Length::Fill)
            .height(Length::Fill)
            .into()
    }
}

fn collect_jump(
    scripts: &BTreeMap<String, Script>,
    parent: &str,
    window: &AutomationsWindow,
    items: &mut Vec<Item>,
) {
    for (name, script) in scripts {
        match script {
            Script::Folder(_, children) => {
                let path = if parent.is_empty() {
                    name.clone()
                } else {
                    format!("{parent}/{name}")
                };
                items.push(Item {
                    group: "Jump to",
                    label: name.clone(),
                    status: None,
                    kind: Some("folder"),
                    message: Message::SelectFolder(path.clone()),
                });
                collect_jump(children, &path, window, items);
            }
            other => {
                let kind = match other {
                    Script::Alias(_) => "alias",
                    Script::Trigger(_) => "trigger",
                    Script::Hotkey(_) => "hotkey",
                    Script::Folder(_, _) => "folder",
                };
                items.push(Item {
                    group: "Jump to",
                    label: name.clone(),
                    status: Some(window.script_status(other)),
                    kind: Some(kind),
                    message: Message::SelectScript(ScriptKey {
                        folder_name: other.folder_name().map(str::to_string),
                        script_name: name.clone(),
                    }),
                });
            }
        }
    }
}
