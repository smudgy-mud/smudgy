use iced::alignment::Vertical;
use iced::widget::{Row, Space, button, mouse_area, row, svg, text};
use iced::{Color, Length};
use smudgy_theme::builtins;

use crate::assets;
use crate::theme::Element;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Message {
    ConnectPressed,
    SettingsPressed,
    AutomationsPressed,
    MapEditorPressed,
    ToggleExpand,
    // Window controls: the toolbar doubles as the titlebar of the borderless
    // main window.
    DragWindow,
    MinimizePressed,
    ToggleMaximizePressed,
    ClosePressed,
}

/// Context information about the active session for the toolbar
#[derive(Debug, Clone)]
#[derive(Default)]
pub struct SessionContext {
    pub has_active_session: bool,
    // Carries the active session's connection state; not currently read.
    #[allow(dead_code)]
    pub is_connected: bool,
    #[allow(dead_code)]
    pub server_name: String,
}


const TITLE_COLOR: Color = Color::from_rgb8(92, 92, 92);
// Companion hover shade for the title/window-control styling; currently unused.
#[allow(dead_code)]
const TITLE_COLOR_HOVER: Color = Color::from_rgb8(128, 128, 128);

/// Fixed toolbar height. The drag area fills the toolbar, so without an
/// explicit height the row would become fluid and grab half the window.
const TOOLBAR_HEIGHT: f32 = 42.0;

fn svg_style(_: &crate::Theme, _: iced::widget::svg::Status) -> iced::widget::svg::Style {
    iced::widget::svg::Style {
        color: Some(TITLE_COLOR),
    }
}

fn window_control_button(
    handle: iced::widget::svg::Handle,
    message: Message,
) -> Element<'static, Message> {
    button(svg(handle).width(14).height(14).style(svg_style))
        .style(builtins::button::link)
        .padding([4, 8])
        .on_press(message)
        .into()
}

/// The empty stretch of toolbar between the app buttons and the window
/// controls: dragging it moves the window, double-clicking toggles maximize.
fn drag_area() -> Element<'static, Message> {
    mouse_area(Space::new().width(Length::Fill).height(Length::Fill))
        .on_press(Message::DragWindow)
        .on_double_click(Message::ToggleMaximizePressed)
        .into()
}

/// Quiet text button used for the toolbar's menu items.
fn toolbar_button(label: &'static str, message: Message) -> Element<'static, Message> {
    button(text(label).size(14))
        .style(builtins::button::toolbar)
        .padding([4, 10])
        .on_press(message)
        .into()
}

/// The hamburger toggle, identical in both toolbar states so it doesn't jump
/// around when expanding/collapsing.
fn menu_button() -> Element<'static, Message> {
    button(
        svg(assets::hero_icons::BARS_3.clone())
            .width(16)
            .height(16)
            .style(svg_style),
    )
    .style(builtins::button::link)
    .padding([4, 8])
    .on_press(Message::ToggleExpand)
    .into()
}

fn window_controls(maximized: bool) -> Element<'static, Message> {
    let maximize_icon = if maximized {
        assets::hero_icons::SQUARE_2_STACK.clone()
    } else {
        assets::hero_icons::STOP.clone()
    };

    row![
        window_control_button(assets::hero_icons::MINUS.clone(), Message::MinimizePressed),
        window_control_button(maximize_icon, Message::ToggleMaximizePressed),
        window_control_button(assets::hero_icons::X_MARK.clone(), Message::ClosePressed),
    ]
    .spacing(2)
    .align_y(Vertical::Center)
    .into()
}

pub fn view(
    expanded: bool,
    maximized: bool,
    session_context: &SessionContext,
) -> Element<'static, Message> {
    if expanded {
        // Expanded view: quiet menu-bar items
        let mut buttons = vec![
            menu_button(),
            toolbar_button("Connect", Message::ConnectPressed),
        ];

        // Only show automations button if there's an active session
        if session_context.has_active_session {
            buttons.push(toolbar_button("Automations", Message::AutomationsPressed));
            buttons.push(toolbar_button("Map Editor", Message::MapEditorPressed));
        }

        buttons.push(toolbar_button("Settings", Message::SettingsPressed));

        buttons.push(drag_area());
        buttons.push(window_controls(maximized));

        Row::with_children(buttons)
            .spacing(4)
            .padding(5)
            // Make the expanded toolbar fill width
            .width(Length::Fill)
            .height(TOOLBAR_HEIGHT)
            .align_y(Vertical::Center)
            .into()
    } else {
        // Collapsed view: Hamburger + Text. This bar doubles as the window's
        // title bar, so it mirrors the OS title (incl. the dev-build marker).
        let title = text(crate::MAIN_WINDOW_TITLE).size(14).color(TITLE_COLOR);

        row![menu_button(), title, drag_area(), window_controls(maximized)]
            .padding(5)
            .spacing(10)
            // The collapsed toolbar still spans the window so the drag area
            // and window controls stay reachable
            .width(Length::Fill)
            .height(TOOLBAR_HEIGHT)
            .align_y(Vertical::Center)
            .into()
    }
}
