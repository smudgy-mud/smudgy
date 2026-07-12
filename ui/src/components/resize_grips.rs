//! Invisible edge/corner hit zones for resizing the borderless main window.
//!
//! Stacked over the window content (`stack![content, resize_grips::view(..)]`);
//! everything except the thin edge strips is a plain `Space`, which never
//! captures events, so clicks fall through to the content beneath. Pressing a
//! grip hands the resize off to the OS via `window::drag_resize`, so snapping
//! and minimum-size constraints behave natively.

use iced::widget::{Space, column, mouse_area, row};
use iced::window::Direction;
use iced::{Length, mouse::Interaction};

use crate::theme::Element;

/// Thickness of the edge strips, in logical pixels.
const GRIP: f32 = 6.0;
/// How far corner zones extend along each edge.
const CORNER: f32 = 16.0;

fn interaction_for(direction: Direction) -> Interaction {
    match direction {
        Direction::North | Direction::South => Interaction::ResizingVertically,
        Direction::East | Direction::West => Interaction::ResizingHorizontally,
        Direction::NorthWest | Direction::SouthEast => Interaction::ResizingDiagonallyDown,
        Direction::NorthEast | Direction::SouthWest => Interaction::ResizingDiagonallyUp,
    }
}

fn grip<'a, Message: Clone + 'a>(
    width: impl Into<Length>,
    height: impl Into<Length>,
    direction: Direction,
    on_resize: &impl Fn(Direction) -> Message,
) -> Element<'a, Message> {
    mouse_area(Space::new().width(width).height(height))
        .on_press(on_resize(direction))
        .interaction(interaction_for(direction))
        .into()
}

pub fn view<'a, Message: Clone + 'a>(
    on_resize: impl Fn(Direction) -> Message,
) -> Element<'a, Message> {
    let on_resize = &on_resize;

    let top = row![
        grip(CORNER, GRIP, Direction::NorthWest, on_resize),
        grip(Length::Fill, GRIP, Direction::North, on_resize),
        grip(CORNER, GRIP, Direction::NorthEast, on_resize),
    ];

    // Vertical extensions of the corner zones, so diagonals are grabbable
    // from the sides too, not just the top/bottom strips.
    let left = column![
        grip(GRIP, CORNER - GRIP, Direction::NorthWest, on_resize),
        grip(GRIP, Length::Fill, Direction::West, on_resize),
        grip(GRIP, CORNER - GRIP, Direction::SouthWest, on_resize),
    ];

    let right = column![
        grip(GRIP, CORNER - GRIP, Direction::NorthEast, on_resize),
        grip(GRIP, Length::Fill, Direction::East, on_resize),
        grip(GRIP, CORNER - GRIP, Direction::SouthEast, on_resize),
    ];

    let bottom = row![
        grip(CORNER, GRIP, Direction::SouthWest, on_resize),
        grip(Length::Fill, GRIP, Direction::South, on_resize),
        grip(CORNER, GRIP, Direction::SouthEast, on_resize),
    ];

    column![
        top,
        row![
            left,
            Space::new().width(Length::Fill).height(Length::Fill),
            right
        ]
        .height(Length::Fill),
        bottom,
    ]
    .width(Length::Fill)
    .height(Length::Fill)
    .into()
}
