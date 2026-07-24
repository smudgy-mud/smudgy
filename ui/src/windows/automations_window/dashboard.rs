//! The welcome / dashboard pane: status cards, create tiles, and a
//! Discover teaser (the top featured packages). Shown when nothing is selected.

use std::collections::BTreeMap;

use iced::alignment::{Horizontal, Vertical};
use iced::widget::{Column, button, column, row, text};
use iced::{Font, Length, Padding};

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::builtins::button as button_style;
use crate::widgets::wrap_row::wrap_row;

use super::common;
use super::editors::pane_scroll;
use super::model::{NodeStatus, Script, ScriptKey};
use super::{AutomationsWindow, Chip, Elem, Message};

impl AutomationsWindow {
    pub(super) fn view_dashboard(&self) -> Elem<'_> {
        let stats = self.dashboard_stats();

        // Header.
        let host = self.mud_host.clone().unwrap_or_else(|| "—".to_string());
        let header = column![
            row![
                common::status_dot(NodeStatus::Ok),
                text(crate::i18n::t!("automations-title")).size(30.0).font(Font {
                    weight: iced::font::Weight::Light,
                    ..fonts::GEIST_VF
                }),
                iced::widget::space::horizontal(),
                row![
                    common::status_dot(NodeStatus::Ok),
                    text(crate::i18n::t!("automations-connected")).size(13.0).style(common::muted),
                ]
                .spacing(6.0)
                .align_y(Vertical::Center),
            ]
            .spacing(10.0)
            .align_y(Vertical::Center),
            text(crate::i18n::t!(
                "automations-profile",
                "server" => &self.server_name,
                "host" => host
            ))
                .size(13.0)
                .style(common::muted),
            iced::widget::rule::horizontal(1.0),
        ]
        .spacing(10.0);

        // Stat cards.
        let cards = row![
            stat_card(
                "Active",
                stats.active,
                NodeStatus::Ok,
                Message::ShowDashboard
            ),
            stat_card(
                crate::i18n::ts!("automations-errors"),
                stats.errors,
                if stats.errors > 0 {
                    NodeStatus::Error
                } else {
                    NodeStatus::Disabled
                },
                stats
                    .first_error
                    .clone()
                    .map_or(Message::ShowDashboard, Message::SelectScript),
            ),
            stat_card(
                "Disabled",
                stats.disabled,
                NodeStatus::Disabled,
                Message::ShowDashboard
            ),
            stat_card(
                "Packages",
                stats.packages,
                NodeStatus::Ok,
                Message::SelectChip(Chip::Packages)
            ),
        ]
        .spacing(12.0);

        // Create tiles. A wrapping flow row so the tiles reflow onto more rows
        // as the pane narrows, rather than overflowing (iced has no flex-wrap).
        let create = column![
            common::section_label(crate::i18n::ts!("automations-create")),
            wrap_row(vec![
                create_tile(bootstrap_icons::AT, "Alias", Message::NewAlias),
                create_tile(bootstrap_icons::LIGHTNING, "Trigger", Message::NewTrigger),
                create_tile(bootstrap_icons::DPAD, "Hotkey", Message::NewHotkey),
                create_tile(bootstrap_icons::FOLDER_PLUS, "Folder", Message::NewFolder),
                create_tile(bootstrap_icons::FONTS, "Module", Message::NewModule),
                create_tile(
                    bootstrap_icons::BOUNDING_BOX,
                    "Package",
                    Message::NewPackage
                ),
                palette_tile(),
            ])
            .spacing(10.0, 10.0),
        ]
        .spacing(8.0);

        // Discover teaser: the top featured public packages (a default-scope, empty-query search
        // loaded on window init) plus a jump into the full Discover pane. Public discovery works
        // without an account, so the teaser loads for everyone.
        let mut discover = Column::new()
            .spacing(8.0)
            .push(common::section_label(crate::i18n::ts!("automations-discover")));
        if self.featured_packages.is_empty() {
            discover = discover.push(
                text(crate::i18n::t!("automations-discover-help"))
                    .size(13.0)
                    .style(common::muted),
            );
        }
        for result in self.featured_packages.iter().take(3) {
            discover = discover.push(self.discover_result_card(result));
        }
        discover = discover.push(
            row![
                iced::widget::space::horizontal(),
                button(text(crate::i18n::t!("automations-see-more")).size(13.0))
                    .style(button_style::secondary)
                    .on_press(Message::OpenDiscover),
            ]
            .align_y(Vertical::Center),
        );

        pane_scroll(column![header, cards, create, discover].spacing(24.0))
    }

    fn dashboard_stats(&self) -> Stats {
        let mut s = Stats {
            packages: self.installed_packages.len() + self.local_packages.len(),
            ..Stats::default()
        };
        fn walk(
            window: &AutomationsWindow,
            scripts: &BTreeMap<String, Script>,
            parent: Option<&str>,
            s: &mut Stats,
        ) {
            for (name, script) in scripts {
                match script {
                    Script::Folder(_, children) => {
                        let path = match parent {
                            Some(p) => format!("{p}/{name}"),
                            None => name.clone(),
                        };
                        walk(window, children, Some(&path), s);
                    }
                    other => match window.script_status(other) {
                        // Scripts never warn (only installed packages do), but the match is
                        // exhaustive — count a warning as active (it's still enabled).
                        NodeStatus::Ok | NodeStatus::Warning => s.active += 1,
                        NodeStatus::Error => {
                            s.errors += 1;
                            if s.first_error.is_none() {
                                s.first_error = Some(ScriptKey {
                                    folder_name: other.folder_name().map(str::to_string),
                                    script_name: name.clone(),
                                });
                            }
                        }
                        NodeStatus::Disabled => s.disabled += 1,
                    },
                }
            }
        }
        walk(self, &self.scripts, None, &mut s);
        s
    }
}

#[derive(Default)]
struct Stats {
    active: usize,
    errors: usize,
    disabled: usize,
    packages: usize,
    first_error: Option<ScriptKey>,
}

fn stat_card(label: &str, value: usize, status: NodeStatus, msg: Message) -> Elem<'static> {
    button(
        column![
            row![
                common::status_dot(status),
                text(label.to_uppercase()).size(10.0).style(common::faint),
            ]
            .spacing(6.0)
            .align_y(Vertical::Center),
            text(value.to_string()).size(34.0).font(Font {
                weight: iced::font::Weight::Light,
                ..fonts::GEIST_VF
            }),
        ]
        .spacing(8.0),
    )
    .style(card_button_style)
    .on_press(msg)
    .width(Length::Fill)
    .padding(16.0)
    .into()
}

/// One create tile: a muted icon over a bright label on a quiet flat surface.
fn create_tile(icon: &str, label: &str, msg: Message) -> Elem<'static> {
    tile(icon, label, msg, create_tile_style)
}

/// The Ctrl/⌘+P tile: an affordance for the command palette rather than a create
/// action, so it sits in the same row but as an unfilled "ghost" tile.
fn palette_tile() -> Elem<'static> {
    tile(
        bootstrap_icons::SEARCH,
        common::palette_shortcut_label(),
        Message::OpenPalette,
        palette_tile_style,
    )
}

/// Shared tile body, parameterised by the surface style so the create tiles and
/// the palette tile stay identically sized and aligned in the wrapping row.
fn tile(
    icon: &str,
    label: &str,
    msg: Message,
    style: fn(&crate::theme::Theme, iced::widget::button::Status) -> iced::widget::button::Style,
) -> Elem<'static> {
    button(
        column![
            text(icon.to_string())
                .font(fonts::BOOTSTRAP_ICONS)
                .size(18.0)
                .style(common::muted),
            text(label.to_string()).size(12.0),
        ]
        .spacing(8.0)
        .align_x(Horizontal::Center)
        .width(Length::Fill),
    )
    .style(style)
    .on_press(msg)
    .width(Length::Fixed(92.0))
    .padding(Padding {
        top: 14.0,
        bottom: 14.0,
        left: 8.0,
        right: 8.0,
    })
    .into()
}

/// Flat create-tile surface: a faint translucent fill with a hairline border
/// (the same quiet treatment as the window's banners/outline boxes), brightening
/// on hover. Quieter than the raised stat cards so the tiles don't shout.
fn create_tile_style(
    theme: &crate::theme::Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    use iced::widget::button::Status;
    let fill = match status {
        Status::Hovered => 0.09,
        Status::Pressed => 0.06,
        _ => 0.04,
    };
    tile_surface(theme, theme.styles.text.normal.scale_alpha(fill))
}

/// The palette tile surface: unfilled at rest so it reads as a shortcut next to the
/// filled create tiles, picking up a faint fill only on hover/press.
fn palette_tile_style(
    theme: &crate::theme::Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    use iced::widget::button::Status;
    let fill = match status {
        Status::Hovered => 0.06,
        Status::Pressed => 0.04,
        _ => 0.0,
    };
    tile_surface(theme, theme.styles.text.normal.scale_alpha(fill))
}

fn tile_surface(theme: &crate::theme::Theme, fill: iced::Color) -> iced::widget::button::Style {
    iced::widget::button::Style {
        background: Some(iced::Background::Color(fill)),
        text_color: theme.styles.text.normal,
        border: iced::Border {
            color: theme.styles.general.border,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}

/// A card-styled button (used for stat cards + create tiles).
fn card_button_style(
    theme: &crate::theme::Theme,
    status: iced::widget::button::Status,
) -> iced::widget::button::Style {
    use iced::widget::button::Status;
    let base = theme.styles.general.container_background;
    let bg = match status {
        Status::Hovered | Status::Pressed => common::lighten(base, 0.06),
        _ => base,
    };
    iced::widget::button::Style {
        background: Some(iced::Background::Color(bg)),
        text_color: theme.styles.text.normal,
        border: iced::Border {
            color: theme.styles.general.border,
            width: 1.0,
            radius: 8.0.into(),
        },
        ..Default::default()
    }
}
