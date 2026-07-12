//! An inline HSV color picker: a saturation/value square over a hue
//! strip, both plain canvases. Dragging emits [`Event::Preview`] per move
//! and [`Event::Committed`] on release, so hosts can show live feedback
//! while writing (and syncing) only once per gesture.

use iced::widget::canvas::{self, Canvas, gradient};
use iced::{Color, Length, Point, Rectangle, Size, mouse};

pub type Renderer = iced::Renderer;
pub type Theme = smudgy_theme::Theme;
pub type Element<'a, Message> = iced::Element<'a, Message, Theme, Renderer>;

const SQUARE_HEIGHT: f32 = 140.0;
const STRIP_HEIGHT: f32 = 14.0;
const CURSOR_RADIUS: f32 = 5.0;

/// A color in hue/saturation/value space, the picker's working state.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Hsv {
    /// Hue in degrees, `0.0..360.0`.
    pub hue: f32,
    /// Saturation, `0.0..=1.0`.
    pub saturation: f32,
    /// Value (brightness), `0.0..=1.0`.
    pub value: f32,
}

impl Hsv {
    #[must_use]
    pub fn to_color(self) -> Color {
        let h = (self.hue.rem_euclid(360.0)) / 60.0;
        let c = self.value * self.saturation;
        let x = c * (1.0 - (h.rem_euclid(2.0) - 1.0).abs());
        let m = self.value - c;

        let (r, g, b) = match h {
            h if h < 1.0 => (c, x, 0.0),
            h if h < 2.0 => (x, c, 0.0),
            h if h < 3.0 => (0.0, c, x),
            h if h < 4.0 => (0.0, x, c),
            h if h < 5.0 => (x, 0.0, c),
            _ => (c, 0.0, x),
        };

        Color::from_rgb(r + m, g + m, b + m)
    }

    #[must_use]
    pub fn from_color(color: Color) -> Self {
        let max = color.r.max(color.g).max(color.b);
        let min = color.r.min(color.g).min(color.b);
        let delta = max - min;

        let hue = if delta == 0.0 {
            0.0
        } else if max == color.r {
            60.0 * ((color.g - color.b) / delta).rem_euclid(6.0)
        } else if max == color.g {
            60.0 * ((color.b - color.r) / delta + 2.0)
        } else {
            60.0 * ((color.r - color.g) / delta + 4.0)
        };

        Self {
            hue,
            saturation: if max == 0.0 { 0.0 } else { delta / max },
            value: max,
        }
    }
}

/// The picked color as a `#rrggbb` CSS string.
#[must_use]
pub fn to_hex(color: Color) -> String {
    let [r, g, b, _] = color.into_rgba8();
    format!("#{r:02x}{g:02x}{b:02x}")
}

#[derive(Debug, Clone, Copy)]
pub enum Message {
    /// Saturation/value changed by pointing in the square.
    SvChanged {
        saturation: f32,
        value: f32,
        commit: bool,
    },
    /// Hue changed by pointing in the strip.
    HueChanged { hue: f32, commit: bool },
}

#[derive(Debug, Clone, Copy)]
pub enum Event {
    /// The pointer is mid-drag; the picker state already reflects the new
    /// color (read it via [`ColorPicker::color`]) but nothing should be
    /// written yet.
    Preview,
    /// The gesture finished; write the color.
    Committed(Color),
}

#[derive(Debug, Clone, Copy)]
pub struct ColorPicker {
    hsv: Hsv,
}

impl ColorPicker {
    #[must_use]
    pub fn from_color(color: Color) -> Self {
        Self {
            hsv: Hsv::from_color(color),
        }
    }

    #[must_use]
    pub fn color(&self) -> Color {
        self.hsv.to_color()
    }

    pub fn update(&mut self, message: Message) -> Event {
        let commit = match message {
            Message::SvChanged {
                saturation,
                value,
                commit,
            } => {
                self.hsv.saturation = saturation;
                self.hsv.value = value;
                commit
            }
            Message::HueChanged { hue, commit } => {
                self.hsv.hue = hue;
                commit
            }
        };

        if commit {
            Event::Committed(self.color())
        } else {
            Event::Preview
        }
    }

    pub fn view(&self) -> Element<'_, Message> {
        iced::widget::column![
            Canvas::new(SvSquare { hsv: self.hsv })
                .width(Length::Fill)
                .height(SQUARE_HEIGHT),
            Canvas::new(HueStrip { hsv: self.hsv })
                .width(Length::Fill)
                .height(STRIP_HEIGHT),
        ]
        .spacing(6)
        .into()
    }
}

/// Shared canvas drag state: whether the press started in this canvas.
#[derive(Default)]
pub struct DragState {
    dragging: bool,
}

/// Maps a pointer gesture over a canvas into picker messages: press and
/// drag preview, release commits. Returns `None` for unrelated events.
fn track_drag(
    state: &mut DragState,
    event: &iced::Event,
    bounds: Rectangle,
    cursor: mouse::Cursor,
    message: impl Fn(Point, Size, bool) -> Message,
) -> Option<canvas::Action<Message>> {
    match event {
        iced::Event::Mouse(mouse::Event::ButtonPressed(mouse::Button::Left)) => {
            let position = cursor.position_in(bounds)?;
            state.dragging = true;
            Some(
                canvas::Action::publish(message(position, bounds.size(), false)).and_capture(),
            )
        }
        iced::Event::Mouse(mouse::Event::CursorMoved { .. }) if state.dragging => {
            // Clamp to bounds so dragging past an edge pins to it.
            let position = cursor.position()?;
            let clamped = Point::new(
                (position.x - bounds.x).clamp(0.0, bounds.width),
                (position.y - bounds.y).clamp(0.0, bounds.height),
            );
            Some(canvas::Action::publish(message(clamped, bounds.size(), false)).and_capture())
        }
        iced::Event::Mouse(mouse::Event::ButtonReleased(mouse::Button::Left))
            if state.dragging =>
        {
            state.dragging = false;
            let position = cursor.position().map_or_else(
                || Point::new(bounds.width / 2.0, bounds.height / 2.0),
                |position| {
                    Point::new(
                        (position.x - bounds.x).clamp(0.0, bounds.width),
                        (position.y - bounds.y).clamp(0.0, bounds.height),
                    )
                },
            );
            Some(canvas::Action::publish(message(position, bounds.size(), true)).and_capture())
        }
        _ => None,
    }
}

fn cursor_ring(frame: &mut canvas::Frame, at: Point, fill: Color) {
    let ring = canvas::Path::circle(at, CURSOR_RADIUS);
    frame.fill(&ring, fill);
    frame.stroke(
        &ring,
        canvas::Stroke::default()
            .with_color(Color::WHITE)
            .with_width(1.5),
    );
}

/// The saturation (→) / value (↓) plane at the current hue.
struct SvSquare {
    hsv: Hsv,
}

impl canvas::Program<Message, Theme> for SvSquare {
    type State = DragState;

    fn update(
        &self,
        state: &mut DragState,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        track_drag(state, event, bounds, cursor, |position, size, commit| {
            Message::SvChanged {
                saturation: (position.x / size.width).clamp(0.0, 1.0),
                value: 1.0 - (position.y / size.height).clamp(0.0, 1.0),
                commit,
            }
        })
    }

    fn draw(
        &self,
        _state: &DragState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let full = canvas::Path::rectangle(Point::ORIGIN, bounds.size());

        // White → pure hue across, then transparent → black down: the
        // classic two-gradient construction of the SV plane.
        let hue_color = Hsv {
            hue: self.hsv.hue,
            saturation: 1.0,
            value: 1.0,
        }
        .to_color();

        frame.fill(
            &full,
            gradient::Linear::new(
                Point::new(0.0, 0.0),
                Point::new(bounds.width, 0.0),
            )
            .add_stop(0.0, Color::WHITE)
            .add_stop(1.0, hue_color),
        );
        frame.fill(
            &full,
            gradient::Linear::new(
                Point::new(0.0, 0.0),
                Point::new(0.0, bounds.height),
            )
            .add_stop(0.0, Color::TRANSPARENT)
            .add_stop(1.0, Color::BLACK),
        );

        cursor_ring(
            &mut frame,
            Point::new(
                self.hsv.saturation * bounds.width,
                (1.0 - self.hsv.value) * bounds.height,
            ),
            self.hsv.to_color(),
        );

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &DragState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}

/// The hue rainbow strip.
struct HueStrip {
    hsv: Hsv,
}

impl canvas::Program<Message, Theme> for HueStrip {
    type State = DragState;

    fn update(
        &self,
        state: &mut DragState,
        event: &iced::Event,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> Option<canvas::Action<Message>> {
        track_drag(state, event, bounds, cursor, |position, size, commit| {
            Message::HueChanged {
                hue: (position.x / size.width).clamp(0.0, 1.0) * 359.99,
                commit,
            }
        })
    }

    fn draw(
        &self,
        _state: &DragState,
        renderer: &Renderer,
        _theme: &Theme,
        bounds: Rectangle,
        _cursor: mouse::Cursor,
    ) -> Vec<canvas::Geometry> {
        let mut frame = canvas::Frame::new(renderer, bounds.size());
        let full = canvas::Path::rectangle(Point::ORIGIN, bounds.size());

        let mut rainbow = gradient::Linear::new(
            Point::new(0.0, 0.0),
            Point::new(bounds.width, 0.0),
        );
        for sextant in 0..=6 {
            #[allow(clippy::cast_precision_loss)]
            let offset = sextant as f32 / 6.0;
            rainbow = rainbow.add_stop(
                offset,
                Hsv {
                    hue: offset * 359.99,
                    saturation: 1.0,
                    value: 1.0,
                }
                .to_color(),
            );
        }
        frame.fill(&full, rainbow);

        cursor_ring(
            &mut frame,
            Point::new(
                (self.hsv.hue / 360.0) * bounds.width,
                bounds.height / 2.0,
            ),
            Hsv {
                hue: self.hsv.hue,
                saturation: 1.0,
                value: 1.0,
            }
            .to_color(),
        );

        vec![frame.into_geometry()]
    }

    fn mouse_interaction(
        &self,
        _state: &DragState,
        bounds: Rectangle,
        cursor: mouse::Cursor,
    ) -> mouse::Interaction {
        if cursor.is_over(bounds) {
            mouse::Interaction::Crosshair
        } else {
            mouse::Interaction::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Hsv, to_hex};

    #[test]
    fn hsv_round_trips_through_rgb() {
        for (h, s, v) in [
            (0.0, 1.0, 1.0),
            (120.0, 0.5, 0.75),
            (240.0, 1.0, 0.5),
            (300.0, 0.25, 1.0),
            (59.9, 0.9, 0.1),
        ] {
            let hsv = Hsv {
                hue: h,
                saturation: s,
                value: v,
            };
            let back = Hsv::from_color(hsv.to_color());
            assert!(
                (back.hue - h).abs() < 0.5
                    && (back.saturation - s).abs() < 0.01
                    && (back.value - v).abs() < 0.01,
                "{hsv:?} round-tripped to {back:?}"
            );
        }
    }

    #[test]
    fn grays_round_trip_without_hue_noise() {
        let gray = Hsv::from_color(iced::Color::from_rgb8(128, 128, 128));
        assert_eq!(gray.saturation, 0.0);
        let back = gray.to_color().into_rgba8();
        assert_eq!(back[0], back[1]);
        assert_eq!(back[1], back[2]);
    }

    #[test]
    fn hex_output_is_css_parseable() {
        let hex = to_hex(
            Hsv {
                hue: 200.0,
                saturation: 0.8,
                value: 0.9,
            }
            .to_color(),
        );
        assert!(smudgy_cloud::parse_css_color(&hex).is_some(), "{hex}");
    }
}
