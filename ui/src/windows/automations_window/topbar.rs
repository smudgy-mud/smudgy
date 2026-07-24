//! The main column's top action bar: a breadcrumb on the left, and quiet
//! browse/run/inspect actions + a Ctrl/⌘+P button on the right. Creation never appears
//! here (it is deliberately kept out of this toolbar).

use iced::alignment::Vertical;
use iced::widget::{button, column, container, row, rule, text, tooltip};
use iced::{Border, Color, Length, Padding};

use crate::assets::{bootstrap_icons, fonts};
use crate::theme::Theme;
use crate::theme::builtins::button as button_style;

use super::common;
use super::model::package_display_name;
use super::{AutomationsWindow, Elem, Message, Pane, Selection};

impl AutomationsWindow {
    pub(super) fn view_topbar(&self) -> Elem<'_> {
        // ---- breadcrumb ----
        let mut crumbs = row![
            button(text(crate::i18n::t!("automations-title")).size(13.0).style(common::muted))
                .style(button_style::list_item)
                .on_press(Message::ShowDashboard)
                .padding(Padding {
                    top: 2.0,
                    bottom: 2.0,
                    left: 2.0,
                    right: 2.0,
                }),
        ]
        .spacing(6.0)
        .align_y(Vertical::Center);
        let trail = self.breadcrumb_trail();
        let last = trail.len();
        for (i, crumb) in trail.into_iter().enumerate() {
            crumbs = crumbs.push(text("\u{203A}").size(13.0).style(common::faint));
            let emphasized = i + 1 == last;
            crumbs = crumbs.push(text(crumb).size(13.0).style(if emphasized {
                common::regular
            } else {
                common::muted
            }));
        }

        // ---- actions ----
        let discover_active = matches!(self.pane, Pane::Discover);
        let shared_active = matches!(self.pane, Pane::Shared);
        let store_active = matches!(self.pane, Pane::StoreInspector);
        let mut actions = row![
            action_button(
                bootstrap_icons::CLOUD_CHECK,
                "Discover",
                Message::OpenDiscover,
                discover_active
            ),
            action_button(
                bootstrap_icons::PEOPLE,
                "Private & Shared",
                Message::OpenShared,
                shared_active
            ),
            action_button(
                bootstrap_icons::DATABASE,
                "Store",
                Message::OpenStoreInspector,
                store_active
            ),
            action_button(
                bootstrap_icons::ARROW_REPEAT,
                "Reload",
                Message::Reload,
                false
            ),
        ]
        .spacing(6.0)
        .align_y(Vertical::Center);
        // The inspector is an advanced feature (debugging full session state) — gated.
        if self.advanced_features {
            actions = actions.push(inspect_button());
        }
        actions = actions
            .push(container(iced::widget::space::horizontal()).width(Length::Fixed(8.0)))
            .push(palette_button());

        let bar = container(
            row![crumbs, iced::widget::space::horizontal(), actions]
                .align_y(Vertical::Center)
                .spacing(12.0),
        )
        .width(Length::Fill)
        .height(Length::Fixed(62.0))
        .padding(Padding {
            top: 16.0,
            bottom: 0.0,
            left: 18.0,
            right: 14.0,
        });

        // A hairline beneath the bar separates the action toolbar from the
        // content pane (the toolbar reads as its own region).
        column![bar, rule::horizontal(1.0)]
            .width(Length::Fill)
            .into()
    }

    fn breadcrumb_trail(&self) -> Vec<String> {
        match &self.selection {
            Selection::None | Selection::Dashboard => Vec::new(),
            Selection::Script(key) => {
                let mut v: Vec<String> = key
                    .folder_name
                    .as_deref()
                    .map(|f| f.split('/').map(str::to_string).collect())
                    .unwrap_or_default();
                v.push(key.script_name.clone());
                v
            }
            Selection::Folder(path) => path.split('/').map(str::to_string).collect(),
            Selection::Module(subpath) => vec![crate::i18n::t!("automations-modules"), subpath.clone()],
            Selection::OwnedPackage(name) => vec![name.clone()],
            Selection::InstalledPackage(spec) => vec![package_display_name(spec).to_string()],
            Selection::Dependency { parent, spec } => vec![
                package_display_name(parent).to_string(),
                package_display_name(spec).to_string(),
            ],
            Selection::CreatorAutomation {
                creator_id, name, ..
            } => {
                let creator = creator_id
                    .strip_prefix("module:")
                    .map(str::to_string)
                    .or_else(|| {
                        creator_id
                            .strip_prefix("package:")
                            .map(|spec| package_display_name(spec).to_string())
                    })
                    .unwrap_or_else(|| creator_id.clone());
                vec![creator, name.clone()]
            }
            Selection::Discover => vec![crate::i18n::t!("automations-discover")],
            Selection::Shared => vec![crate::i18n::t!("automations-private-shared")],
            Selection::StoreInspector => vec![crate::i18n::t!("automations-session-store")],
        }
    }
}

/// Active/selected variant of the flat toolbar button: a quiet filled pill so
/// the currently open browse pane (Discover/Shared) reads as current without
/// the heavy boxed chrome the rest of the toolbar avoids.
fn toolbar_active(theme: &Theme, status: button::Status) -> button::Style {
    let fill = match status {
        button::Status::Hovered | button::Status::Pressed => 0.16,
        _ => 0.12,
    };
    button::Style {
        background: Some(Color::from_rgba8(255, 255, 255, fill).into()),
        border: Border {
            radius: 4.0.into(),
            ..Border::default()
        },
        text_color: theme.styles.text.normal,
        ..Default::default()
    }
}

fn action_button(
    icon: &'static str,
    label: &'static str,
    msg: Message,
    active: bool,
) -> Elem<'static> {
    let style: fn(&Theme, button::Status) -> button::Style = if active {
        toolbar_active
    } else {
        button_style::toolbar
    };
    button(
        row![
            text(icon).font(fonts::BOOTSTRAP_ICONS).size(13.0),
            text(label).size(13.0),
        ]
        .spacing(7.0)
        .align_y(Vertical::Center),
    )
    .style(style)
    .on_press(msg)
    .padding(Padding {
        top: 5.0,
        bottom: 5.0,
        left: 9.0,
        right: 9.0,
    })
    .into()
}

fn inspect_button() -> Elem<'static> {
    tooltip(
        button(
            row![
                text(bootstrap_icons::CROSSHAIR)
                    .font(fonts::BOOTSTRAP_ICONS)
                    .size(13.0),
                text(crate::i18n::t!("automations-inspect")).size(13.0),
            ]
            .spacing(7.0)
            .align_y(Vertical::Center),
        )
        .style(button_style::toolbar)
        .on_press(Message::Inspect)
        .padding(Padding {
            top: 5.0,
            bottom: 5.0,
            left: 9.0,
            right: 9.0,
        }),
        crate::i18n::ts!("automations-inspect-help"),
        tooltip::Position::Bottom,
    )
    .into()
}

/// The Ctrl/⌘+P command-palette button: a key-cap badge with a search affordance,
/// deliberately keeping its outline so it reads as a key, not a toolbar action.
fn palette_button() -> Elem<'static> {
    button(
        row![
            text(bootstrap_icons::SEARCH)
                .font(fonts::BOOTSTRAP_ICONS)
                .size(11.0)
                .style(common::muted),
            text(common::palette_shortcut_label())
                .size(12.0)
                .style(common::muted),
        ]
        .spacing(6.0)
        .align_y(Vertical::Center),
    )
    .style(button_style::subtle)
    .on_press(Message::OpenPalette)
    .padding(Padding {
        top: 4.0,
        bottom: 4.0,
        left: 9.0,
        right: 9.0,
    })
    .into()
}
