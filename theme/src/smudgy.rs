use iced::{Background, Border, Color, Gradient, Shadow, Vector, border::Radius, gradient::Linear};

use super::{Button, Buttons, General, Modal, Styles, Text, Theme};

#[must_use]
pub fn smudgy() -> Theme {
    Theme {
        name: "Smudgy".to_string(),
        styles: Styles {
            general: General {
                background: Color::from_rgb8(15, 15, 14),
                container_background: Color::from_rgb8(7, 7, 6),
                accent: Color::from_rgb8(55, 23, 130),
                border: Color::from_rgba8(255, 250, 239, 0.1),
                rule: Color::from_rgba8(255, 250, 239, 0.1),
                overlay_background: Color::from_rgba8(20, 20, 20, 0.9),
                // The input strip's darker-than-terminal contrast is
                // intentional; themes restate this pairing on purpose.
                input_background: Color::from_rgb8(7, 7, 6),
                input_text: Color::from_rgb8(255, 250, 239),
                top_highlight: Color::from_rgba8(255, 255, 255, 0.05),
            },
            text: Text {
                normal: Color::from_rgb8(255, 250, 239),
                success: Color::from_rgb8(0, 255, 0),
                error: Color::from_rgb8(255, 0, 0),
            },
            modal: Modal {
                title_bar_background: Background::Gradient(Gradient::Linear(
                    Linear::new(0)
                        .add_stop(0.0, Color::from_rgb8(55, 23, 130))
                        .add_stop(1.0, Color::from_rgb8(63, 40, 116)),
                )),
                title_bar_border: Border {
                    color: Color::from_rgb8(78, 55, 131),
                    width: 1.0,
                    radius: Radius::new(5),
                },
                body_background: Background::Color(Color::from_rgb8(30, 30, 30)),
                body_border: Border {
                    color: Color::from_rgb8(50, 50, 50),
                    width: 1.0,
                    radius: Radius::new(5),
                },
                shadow: Shadow {
                    color: Color::from_rgb8(0, 0, 0),
                    offset: Vector::new(0.0, 0.0),
                    blur_radius: 30.0,
                },
            },
            buttons: Buttons {
                primary: Button {
                    background: Background::Gradient(Gradient::Linear(
                        Linear::new(0)
                            .add_stop(0.0, Color::from_rgb8(55, 23, 130))
                            .add_stop(1.0, Color::from_rgb8(63, 40, 116)),
                    )),
                    background_hover: Background::Gradient(Gradient::Linear(
                        Linear::new(0)
                            .add_stop(0.0, Color::from_rgb8(60, 28, 135))
                            .add_stop(1.0, Color::from_rgb8(68, 45, 121)),
                    )),
                    background_pressed: Background::Gradient(Gradient::Linear(
                        Linear::new(0)
                            .add_stop(0.0, Color::from_rgb8(50, 18, 125))
                            .add_stop(1.0, Color::from_rgb8(63, 40, 116)),
                    )),
                    border: Border {
                        color: Color::from_rgb8(78, 55, 131),
                        width: 1.0,
                        radius: Radius::new(5),
                    },
                    text: Color::from_rgb8(255, 250, 239),
                },
                secondary: Button {
                    background: Background::Color(Color::from_rgb8(68, 68, 68)),
                    background_hover: Background::Color(Color::from_rgb8(0, 0, 0)),
                    background_pressed: Background::Color(Color::from_rgb8(0, 0, 0)),
                    border: Border {
                        color: Color::from_rgb8(131, 131, 131),
                        width: 1.0,
                        radius: Radius::new(5),
                    },
                    text: Color::from_rgb8(255, 250, 239),
                },
            },
        },
    }
}
