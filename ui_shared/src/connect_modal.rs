//! The common Connect dialog presentation.
//!
//! Hosts own persistence and connection effects. The modal geometry and
//! content layout stay identical across desktop and browser.

use iced::widget::{button, column, container, row, scrollable, text};
use iced::{Alignment, Element, Fill, Font, Length, Padding, Pixels, font::Weight};
use smudgy_theme::{Theme, builtins};

pub const WIDTH: f32 = 800.0;
pub const HEIGHT: f32 = 600.0;

pub fn frame<'a, Message: 'a>(
    title: impl Into<String>,
    body: Element<'a, Message, Theme>,
) -> Element<'a, Message, Theme> {
    frame_with_size(title, body, WIDTH, HEIGHT)
}

pub fn frame_with_size<'a, Message: 'a>(
    title: impl Into<String>,
    body: Element<'a, Message, Theme>,
    width: f32,
    height: f32,
) -> Element<'a, Message, Theme> {
    let title = text(title.into())
        .center()
        .width(Fill)
        .height(Length::Fixed(34.0));
    container(column![
        container(row![title])
            .style(builtins::container::modal_title_bar)
            .width(Fill)
            .height(Length::Fixed(34.0)),
        container(body)
            .style(builtins::container::modal_body)
            .width(Fill)
            .height(Fill),
    ])
    .width(Length::Fixed(width))
    .height(Length::Fixed(height))
    .style(builtins::container::modal_container)
    .into()
}

pub fn panes<'a, Message: 'a>(
    servers: Element<'a, Message, Theme>,
    details: Element<'a, Message, Theme>,
) -> Element<'a, Message, Theme> {
    row![servers, details].width(Fill).height(Fill).into()
}

/// The rail and details pane use the same scroll and padding behavior on both
/// hosts. A server details view owns its profile-list scroll and pinned footer;
/// forms and placeholders scroll as one pane.
pub fn body<'a, Message: 'a>(
    servers: Element<'a, Message, Theme>,
    details: Element<'a, Message, Theme>,
    details_own_scroll: bool,
    banner: Option<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let details = if details_own_scroll {
        details
    } else {
        scrollable(container(details).padding(Padding::ZERO.right(14)))
            .height(Fill)
            .into()
    };
    let details = container(details)
        .width(Fill)
        .height(Fill)
        .padding(15)
        .into();
    let panes = panes(servers, details);
    match banner {
        Some(banner) => column![banner, panes].into(),
        None => panes,
    }
}

/// Host data and actions for the one server-details layout. Optional elements
/// carry platform capabilities (observed metadata, restore, or direct connect)
/// without making the browser depend on desktop persistence or runtime types.
pub struct ServerDetails<'a, Message> {
    pub name: &'a str,
    pub icon: Option<Element<'a, Message, Theme>>,
    pub edit: Element<'a, Message, Theme>,
    pub summary: Vec<Element<'a, Message, Theme>>,
    pub profiles_title: String,
    pub profiles_help: String,
    pub profiles: Element<'a, Message, Theme>,
    pub new_profile: Option<Element<'a, Message, Theme>>,
}

pub fn server_details<'a, Message: 'a>(
    details: ServerDetails<'a, Message>,
) -> Element<'a, Message, Theme> {
    let mut title = row![].spacing(10).align_y(Alignment::Center);
    if let Some(icon) = details.icon {
        title = title.push(icon);
    }
    let title = title
        .push(text(details.name).size(Pixels(24.0)))
        .push(iced::widget::space::horizontal())
        .push(details.edit);

    let mut content = column![title].spacing(12);
    for item in details.summary {
        content = content.push(item);
    }
    content = content
        .push(iced::widget::space::vertical().height(Pixels(4.0)))
        .push(text(details.profiles_title).size(Pixels(18.0)))
        .push(
            text(details.profiles_help)
                .size(12)
                .style(builtins::text::muted),
        )
        .push(
            scrollable(container(details.profiles).padding(Padding::ZERO.right(14)))
                .height(Length::FillPortion(1)),
        );
    if let Some(new_profile) = details.new_profile {
        content = content.push(new_profile);
    }
    content.into()
}

pub fn empty_profiles<'a, Message: Clone + 'a>(
    label: impl Into<String>,
    new_label: impl Into<String>,
    new_action: Message,
) -> Element<'a, Message, Theme> {
    container(
        column![
            text(label.into()).size(Pixels(16.0)),
            button(text(new_label.into()))
                .padding([8, 18])
                .style(builtins::button::primary)
                .on_press(new_action),
        ]
        .spacing(12)
        .align_x(Alignment::Center),
    )
    .width(Fill)
    .padding(20)
    .center_x(Fill)
    .into()
}

pub fn new_profile<'a, Message: Clone + 'a>(
    label: impl Into<String>,
    action: Message,
) -> Element<'a, Message, Theme> {
    button(text(label.into()))
        .width(Fill)
        .padding([6, 10])
        .style(builtins::button::secondary)
        .on_press(action)
        .into()
}

pub fn form<'a, Message: 'a>(
    title: Element<'a, Message, Theme>,
    fields: Vec<Element<'a, Message, Theme>>,
    actions: Element<'a, Message, Theme>,
    trailing: Option<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let mut content = column![title].spacing(15);
    for field in fields {
        content = content.push(field);
    }
    content = content.push(actions);
    if let Some(trailing) = trailing {
        content = content
            .push(iced::widget::space::vertical().height(Pixels(10.0)))
            .push(trailing);
    }
    content.into()
}

pub fn field<'a, Message: 'a>(
    label: impl Into<String>,
    control: Element<'a, Message, Theme>,
) -> Element<'a, Message, Theme> {
    column![
        text(label.into()).size(13).style(builtins::text::muted),
        control,
    ]
    .spacing(4)
    .into()
}

pub fn empty_servers<'a, Message: Clone + 'a>(
    title: impl Into<String>,
    help: impl Into<String>,
    add_label: impl Into<String>,
    add_action: Message,
) -> Element<'a, Message, Theme> {
    column![
        text(title.into()).size(Pixels(22.0)),
        text(help.into()).style(builtins::text::muted),
        button(text(add_label.into()))
            .style(builtins::button::primary)
            .padding([8, 18])
            .on_press(add_action),
    ]
    .spacing(15)
    .into()
}

pub struct RailItem<'a, Message> {
    pub label: Element<'a, Message, Theme>,
    pub selected: bool,
    pub select: Option<Message>,
}

pub fn server_rail<'a, Message: Clone + 'a>(
    heading: &str,
    empty: &str,
    new_label: &str,
    items: Vec<RailItem<'a, Message>>,
    new_action: Message,
) -> Element<'a, Message, Theme> {
    let contents: Element<'a, Message, Theme> = if items.is_empty() {
        column![text(empty.to_owned()).style(builtins::text::muted)].into()
    } else {
        items
            .into_iter()
            .fold(column![].spacing(2), |column, item| {
                let button =
                    button(item.label)
                        .width(Fill)
                        .padding([6, 10])
                        .style(if item.selected {
                            builtins::button::list_item_selected
                        } else {
                            builtins::button::list_item
                        });
                column.push(if let Some(message) = item.select {
                    button.on_press(message)
                } else {
                    button
                })
            })
            .into()
    };
    column![
        text(heading.to_owned())
            .size(12)
            .style(builtins::text::muted),
        scrollable(contents).height(Fill),
        button(text(new_label.to_owned()))
            .width(Fill)
            .padding([6, 10])
            .style(builtins::button::secondary)
            .on_press(new_action),
    ]
    .width(Length::Fixed(200.0))
    .spacing(10)
    .padding(15)
    .into()
}

pub fn profile_row<'a, Message: 'a>(
    name: &'a str,
    caption: &'a str,
    actions: Vec<Element<'a, Message, Theme>>,
) -> Element<'a, Message, Theme> {
    let mut label = column![text(name).font(Font {
        weight: Weight::Bold,
        ..crate::assets::GEIST
    })]
    .spacing(2);
    if !caption.is_empty() {
        label = label.push(text(caption).size(12).style(builtins::text::muted));
    }
    actions
        .into_iter()
        .fold(
            row![label.width(Fill)]
                .spacing(10)
                .align_y(Alignment::Center),
            |row, action| row.push(action),
        )
        .into()
}
