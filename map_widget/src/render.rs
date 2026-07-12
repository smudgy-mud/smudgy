//! Pure drawing helpers shared by [`crate::MapView`] and the map editor.
//!
//! All functions draw into an already-transformed [`canvas::Frame`] whose
//! coordinate system is map space (see [`crate::viewport`]). Path/fill
//! geometry is therefore in map units, while stroke widths and text remain
//! in their iced semantics (stroke widths in pixels, text sizes scaled by
//! the frame transform).

use iced::{
    Color, Point, Size, Vector,
    advanced::text::Alignment,
    alignment::Vertical,
    widget::canvas::{self, LineDash, Stroke, gradient, stroke},
};
use smudgy_cloud::{
    ExitDirection, ExitStyle, HorizontalAlignment, Label, Shape, ShapeType, VerticalAlignment,
    mapper::{
        AtlasCache,
        room_cache::RoomCache,
        room_connection::{RoomConnection, RoomConnectionEnd},
    },
};

use crate::viewport::Region;

pub const EXIT_COLOR: Color = Color::from_rgb8(164, 164, 164);
pub const AREA_NAME_FONT_COLOR: Color = EXIT_COLOR;

pub const MAP_ROOM_SIZE: f32 = 0.5;
pub const MAP_ROOM_SIZE_AS_SIZE: Size = Size::new(MAP_ROOM_SIZE, MAP_ROOM_SIZE);
pub const MAP_ROOM_BORDER_RADIUS: f32 = MAP_ROOM_SIZE * 0.2;
pub const MAP_EXIT_STUB_LENGTH: f32 = 0.4;
pub const MAP_PLAYER_INDICATOR_RADIUS: f32 = MAP_ROOM_SIZE / 4.0;

pub const MIN_SCALING_FOR_MAP_GRID: f32 = 20.0;
pub const MIN_SCALING_FOR_MAP_GRID_OPAQUE: f32 = 50.0;

const LEVEL_STUB_HALF_SIZE: f32 = 0.08;
const LEVEL_STUB_OFFSET: f32 = MAP_ROOM_SIZE / 2.0 + 0.09;

/// Radius (map units) of the self-loop marker: a small circle tangent to the
/// room's wall. Sized to read as a loop rather than the smaller external/player
/// dots, and to reach out about as far as a directional stub.
const SELF_LOOP_RADIUS: f32 = 0.15;

/// Dash pattern (in map units) for secret connections.
const SECRET_DASH_SEGMENTS: &[f32] = &[0.12, 0.08];
/// Dash pattern for the `Dashed` exit style, kept distinct from the secret
/// dash so the two reads can diverge.
const STYLE_DASH_SEGMENTS: &[f32] = &[0.16, 0.1];
/// On/off pattern for the `Dotted` exit style; the short on-segment plus a
/// round line cap renders as dots.
const DOTTED_SEGMENTS: &[f32] = &[0.02, 0.1];
/// Alpha the cross-level directional stub fades to at its outward tip. Echoes
/// the opacity a one-level-away ghost draws at (`GHOST_BASE_OPACITY` in the map
/// view) so the stub reads as "leaving for the neighbouring floor".
const CROSS_LEVEL_FADE_FLOOR: f32 = 0.4;
/// Visible length (map units) of the cross-level directional stub, measured
/// outward from the room's edge.
const CROSS_LEVEL_STUB_REACH: f32 = 0.5;
/// Opacity multiplier for "Unknown map" stubs — dimmer than real exits so
/// redacted links read as placeholders, never as named destinations.
const UNKNOWN_MAP_OPACITY: f32 = 0.6;
/// Opacity multiplier applied to secret labels/shapes (and the fill-only
/// parts of secret connections, which cannot be dashed).
const SECRET_OPACITY: f32 = 0.6;
/// Muted accent for the corner mark on secret rooms.
const SECRET_MARK_COLOR: Color = Color::from_rgb8(201, 164, 92);

/// Multiplies a color's alpha, leaving the rest untouched.
#[must_use]
pub fn apply_opacity(color: Color, opacity: f32) -> Color {
    Color {
        r: color.r,
        g: color.g,
        b: color.b,
        a: (color.a * opacity).clamp(0.0, 1.0),
    }
}

/// Parses a CSS-style color string; `None` when empty or unparseable.
/// Panic-safe: editors run this on every keystroke of arbitrary input.
#[must_use]
pub fn parse_color(color: &str) -> Option<Color> {
    smudgy_cloud::parse_css_color(color)
}

/// The connection/stub stroke. The exit's [`ExitStyle`] selects the dash
/// pattern (solid / dashed / dotted); a secret connection whose style is
/// otherwise pattern-less falls back to the secret dash so it still reads as
/// secret. `Meandering` and `Stub` stroke solid — their distinction is in
/// geometry, not the line pattern.
#[must_use]
pub fn exit_stroke(opacity: f32, is_secret: bool, style: ExitStyle) -> Stroke<'static> {
    Stroke {
        line_cap: style_line_cap(style),
        line_dash: LineDash {
            segments: style_segments(style, is_secret),
            offset: 0,
        },
        ..solid_stroke(apply_opacity(EXIT_COLOR, opacity), 1.0)
    }
}

/// Dash segments for an exit's [`ExitStyle`], falling back to the secret dash
/// for pattern-less styles on a secret connection.
fn style_segments(style: ExitStyle, is_secret: bool) -> &'static [f32] {
    match style {
        ExitStyle::Dashed => STYLE_DASH_SEGMENTS,
        ExitStyle::Dotted => DOTTED_SEGMENTS,
        ExitStyle::Normal | ExitStyle::Meandering | ExitStyle::Stub => {
            if is_secret {
                SECRET_DASH_SEGMENTS
            } else {
                &[]
            }
        }
    }
}

/// Round caps turn the short `Dotted` on-segments into dots; every other style
/// keeps butt caps.
fn style_line_cap(style: ExitStyle) -> stroke::LineCap {
    if matches!(style, ExitStyle::Dotted) {
        stroke::LineCap::Round
    } else {
        stroke::LineCap::Butt
    }
}

#[must_use]
pub fn solid_stroke(color: Color, width: f32) -> Stroke<'static> {
    Stroke {
        style: stroke::Style::Solid(color),
        width,
        line_cap: stroke::LineCap::Butt,
        line_join: stroke::LineJoin::Round,
        line_dash: LineDash {
            segments: &[],
            offset: 0,
        },
    }
}

#[inline]
pub fn draw_arrow_head(
    frame: &mut canvas::Frame,
    from: Vector,
    to: Vector,
    color: Color,
    arrow_head_size: f32,
) {
    frame.with_save(|frame| {
        frame.translate(to);
        frame.rotate((to.y - from.y).atan2(to.x - from.x));
        let mut path = canvas::path::Builder::new();
        path.move_to(Point::new(0.0, 0.0));
        path.line_to(Point::new(-arrow_head_size, arrow_head_size));
        path.line_to(Point::new(-arrow_head_size, -arrow_head_size));
        path.close();
        let path = path.build();

        frame.fill(&path, color);
    });
}

/// Shortens a line so it ends at the boundary of a square centered on
/// `line_end`, keeping arrowheads clear of the room they point at.
#[must_use]
pub fn clip_line_end_to_square(line_start: Point, line_end: Point, square_size: f32) -> Point {
    let half_size = square_size / 2.0;

    // Direction vector from start to end
    let dx = line_end.x - line_start.x;
    let dy = line_end.y - line_start.y;

    // If the line has zero length, return the original end point
    if dx.abs() < f32::EPSILON && dy.abs() < f32::EPSILON {
        return line_end;
    }

    // Calculate the intersection with the square boundary
    // The square is centered at line_end
    let left = line_end.x - half_size;
    let right = line_end.x + half_size;
    let top = line_end.y - half_size;
    let bottom = line_end.y + half_size;

    // Find the intersection point on the boundary closest to line_start
    let mut best_t = 1.0; // Start with the original end point

    // Check intersection with left edge (x = left)
    if dx != 0.0 {
        let t = (left - line_start.x) / dx;
        if t > 0.0 && t < best_t {
            let y = line_start.y + t * dy;
            if y >= top && y <= bottom {
                best_t = t;
            }
        }
    }

    // Check intersection with right edge (x = right)
    if dx != 0.0 {
        let t = (right - line_start.x) / dx;
        if t > 0.0 && t < best_t {
            let y = line_start.y + t * dy;
            if y >= top && y <= bottom {
                best_t = t;
            }
        }
    }

    // Check intersection with top edge (y = top)
    if dy != 0.0 {
        let t = (top - line_start.y) / dy;
        if t > 0.0 && t < best_t {
            let x = line_start.x + t * dx;
            if x >= left && x <= right {
                best_t = t;
            }
        }
    }

    // Check intersection with bottom edge (y = bottom)
    if dy != 0.0 {
        let t = (bottom - line_start.y) / dy;
        if t > 0.0 && t < best_t {
            let x = line_start.x + t * dx;
            if x >= left && x <= right {
                best_t = t;
            }
        }
    }

    // Return the intersection point
    Point::new(line_start.x + best_t * dx, line_start.y + best_t * dy)
}

/// Draws the dot grid covering `region`, fading in with zoom.
pub fn draw_grid(frame: &mut canvas::Frame, region: &Region, scaling: f32) {
    if scaling <= MIN_SCALING_FOR_MAP_GRID {
        return;
    }

    let grid_alpha = if scaling > MIN_SCALING_FOR_MAP_GRID_OPAQUE {
        0.05
    } else {
        (scaling - MIN_SCALING_FOR_MAP_GRID)
            / (MIN_SCALING_FOR_MAP_GRID_OPAQUE - MIN_SCALING_FOR_MAP_GRID)
            * 0.05
    };

    #[allow(clippy::cast_possible_truncation)]
    let (x_start, x_end) = (
        region.x.floor() as i32,
        (region.x + region.width).ceil() as i32,
    );
    #[allow(clippy::cast_possible_truncation)]
    let (y_start, y_end) = (
        region.y.floor() as i32,
        (region.y + region.height).ceil() as i32,
    );

    for x in x_start..x_end {
        for y in y_start..y_end {
            #[allow(clippy::cast_precision_loss)]
            let circle = canvas::Path::circle(
                Point {
                    x: x as f32,
                    y: y as f32,
                },
                0.1,
            );
            frame.fill(&circle, Color::from_rgba8(255, 255, 255, grid_alpha));
        }
    }
}

/// The stub line poking out of a room for a cardinal-direction exit, if the
/// direction has a stub representation.
#[must_use]
fn cardinal_stub(x: f32, y: f32, direction: ExitDirection) -> Option<(Point, Point)> {
    let from = Point { x, y };
    match direction {
        ExitDirection::North => Some((
            from,
            Point {
                x,
                y: y - MAP_EXIT_STUB_LENGTH,
            },
        )),
        ExitDirection::East => Some((
            from,
            Point {
                x: x + MAP_EXIT_STUB_LENGTH,
                y,
            },
        )),
        ExitDirection::South => Some((
            from,
            Point {
                x,
                y: y + MAP_EXIT_STUB_LENGTH,
            },
        )),
        ExitDirection::West => Some((
            from,
            Point {
                x: x - MAP_EXIT_STUB_LENGTH,
                y,
            },
        )),
        _ => None,
    }
}

/// Draws a small up (▲) or down (▼) triangle centered at `(cx, cy)`.
fn draw_level_triangle(frame: &mut canvas::Frame, cx: f32, cy: f32, up: bool, opacity: f32) {
    let dir = if up { -1.0 } else { 1.0 };

    let mut path = canvas::path::Builder::new();
    path.move_to(Point::new(cx, cy + dir * LEVEL_STUB_HALF_SIZE));
    path.line_to(Point::new(
        cx - LEVEL_STUB_HALF_SIZE,
        cy - dir * LEVEL_STUB_HALF_SIZE,
    ));
    path.line_to(Point::new(
        cx + LEVEL_STUB_HALF_SIZE,
        cy - dir * LEVEL_STUB_HALF_SIZE,
    ));
    path.close();

    frame.fill(&path.build(), apply_opacity(EXIT_COLOR, opacity));
}

/// Draws the small triangle marking an Up (▲, top-right corner) or Down
/// (▼, bottom-left corner) connection on a room.
pub fn draw_level_stub(frame: &mut canvas::Frame, x: f32, y: f32, up: bool, opacity: f32) {
    let (cx, cy) = if up {
        (x + LEVEL_STUB_OFFSET, y - LEVEL_STUB_OFFSET)
    } else {
        (x - LEVEL_STUB_OFFSET, y + LEVEL_STUB_OFFSET)
    };
    draw_level_triangle(frame, cx, cy, up, opacity);
}

/// The center of a cross-level exit's level triangle when the exit carries a
/// compass direction: placed on that side of the room rather than the fixed
/// up/down corner. Non-planar directions (Up/Down/unset) keep the corner.
fn level_stub_anchor(x: f32, y: f32, direction: ExitDirection, up: bool) -> (f32, f32) {
    match direction {
        ExitDirection::North => (x, y - LEVEL_STUB_OFFSET),
        ExitDirection::South => (x, y + LEVEL_STUB_OFFSET),
        ExitDirection::East => (x + LEVEL_STUB_OFFSET, y),
        ExitDirection::West => (x - LEVEL_STUB_OFFSET, y),
        ExitDirection::Northeast => (x + LEVEL_STUB_OFFSET, y - LEVEL_STUB_OFFSET),
        ExitDirection::Southeast => (x + LEVEL_STUB_OFFSET, y + LEVEL_STUB_OFFSET),
        ExitDirection::Southwest => (x - LEVEL_STUB_OFFSET, y + LEVEL_STUB_OFFSET),
        ExitDirection::Northwest => (x - LEVEL_STUB_OFFSET, y - LEVEL_STUB_OFFSET),
        _ if up => (x + LEVEL_STUB_OFFSET, y - LEVEL_STUB_OFFSET),
        _ => (x - LEVEL_STUB_OFFSET, y + LEVEL_STUB_OFFSET),
    }
}

/// The outward unit vector for a planar cardinal direction (N/E/S/W); `None`
/// for diagonals and non-planar directions, which have no single stub axis.
fn cardinal_unit(direction: ExitDirection) -> Option<Vector> {
    match direction {
        ExitDirection::North => Some(Vector { x: 0.0, y: -1.0 }),
        ExitDirection::East => Some(Vector { x: 1.0, y: 0.0 }),
        ExitDirection::South => Some(Vector { x: 0.0, y: 1.0 }),
        ExitDirection::West => Some(Vector { x: -1.0, y: 0.0 }),
        _ => None,
    }
}

/// The outward unit vector a self-loop arc bulges along. Covers all eight
/// compass points; non-planar directions (Up/Down/In/Out/Special/Other) have no
/// wall of their own, so the loop defaults to the north wall rather than
/// vanishing.
fn self_loop_unit(direction: ExitDirection) -> Vector {
    const DIAG: f32 = std::f32::consts::FRAC_1_SQRT_2;
    match direction {
        ExitDirection::South => Vector { x: 0.0, y: 1.0 },
        ExitDirection::East => Vector { x: 1.0, y: 0.0 },
        ExitDirection::West => Vector { x: -1.0, y: 0.0 },
        ExitDirection::Northeast => Vector { x: DIAG, y: -DIAG },
        ExitDirection::Southeast => Vector { x: DIAG, y: DIAG },
        ExitDirection::Southwest => Vector { x: -DIAG, y: DIAG },
        ExitDirection::Northwest => Vector { x: -DIAG, y: -DIAG },
        // North plus every non-planar direction defaults to the north wall.
        _ => Vector { x: 0.0, y: -1.0 },
    }
}

/// Draws a cross-level cardinal exit as a directional stub in the exit's
/// compass direction, fading from full opacity at the room's edge toward
/// [`CROSS_LEVEL_FADE_FLOOR`] at its tip so it reads as leaving for the
/// neighbouring floor. `style` supplies the dash pattern. Deliberately carries
/// no up/down glyph — the vertical sense is left to the fade; the `Stub` style
/// is the one that keeps the ▲/▼ marker.
fn draw_cross_level_stub(
    frame: &mut canvas::Frame,
    x: f32,
    y: f32,
    unit: Vector,
    opacity: f32,
    is_secret: bool,
    style: ExitStyle,
) {
    let half = MAP_ROOM_SIZE / 2.0;
    let edge = Point {
        x: x + unit.x * half,
        y: y + unit.y * half,
    };
    let reach = half + CROSS_LEVEL_STUB_REACH;
    let tip = Point {
        x: x + unit.x * reach,
        y: y + unit.y * reach,
    };

    let near = apply_opacity(EXIT_COLOR, opacity);
    let far = apply_opacity(EXIT_COLOR, opacity * CROSS_LEVEL_FADE_FLOOR);
    let fade = gradient::Linear::new(edge, tip)
        .add_stop(0.0, near)
        .add_stop(1.0, far);
    let stroke = Stroke {
        style: stroke::Style::Gradient(fade.into()),
        width: 1.0,
        line_cap: style_line_cap(style),
        line_join: stroke::LineJoin::Round,
        line_dash: LineDash {
            segments: style_segments(style, is_secret),
            offset: 0,
        },
    };
    frame.stroke(&canvas::Path::line(edge, tip), stroke);
}

/// Draws a self-loop exit (destination == origin) as a small circle tangent to
/// the room's wall on the exit's side, bulging outward. Reuses the exit
/// `stroke` so style/secret/color read the same as any other exit, while the
/// loop shape keeps a self-referential exit visually distinct from a dangling
/// stub (which is otherwise geometrically identical).
fn draw_self_loop(
    frame: &mut canvas::Frame,
    x: f32,
    y: f32,
    direction: ExitDirection,
    stroke: Stroke<'_>,
) {
    let unit = self_loop_unit(direction);
    // Place the center a radius beyond the wall so the circle is tangent to the
    // room edge and grows outward along the exit direction.
    let offset = MAP_ROOM_SIZE / 2.0 + SELF_LOOP_RADIUS;
    let center = Point {
        x: x + unit.x * offset,
        y: y + unit.y * offset,
    };
    frame.stroke(&canvas::Path::circle(center, SELF_LOOP_RADIUS), stroke);
}

/// Draws one room connection: stubs, connecting line, arrowheads for
/// one-way exits, external-area markers, and level-change stubs.
///
/// When `show_secrets` is false the connection's secrecy is ignored and it
/// renders like any other exit — the map widget hides what the editor marks.
///
/// `suppress_level_stubs` collapses cross-level exits back to the compact
/// corner triangle (today's look), so ghosted adjacent floors don't bristle
/// with directional stubs; the current floor passes it `false`.
#[allow(clippy::too_many_lines)]
pub fn draw_connection(
    frame: &mut canvas::Frame,
    atlas: &AtlasCache,
    connection: &RoomConnection,
    opacity: f32,
    show_secrets: bool,
    suppress_level_stubs: bool,
) {
    let is_secret = show_secrets && connection.is_secret;
    let style = connection.style;
    // The exit's style selects the dash pattern; secret connections still mute
    // their fill-only pieces (level stubs, arrowheads, external markers).
    let stroke = exit_stroke(opacity, is_secret, style);
    let fill_opacity = if is_secret {
        opacity * SECRET_OPACITY
    } else {
        opacity
    };

    match connection.to {
        RoomConnectionEnd::Normal {
            ref direction,
            x,
            y,
            ..
        } => {
            let from_stub = cardinal_stub(
                connection.from_x,
                connection.from_y,
                connection.from_direction,
            );
            let to_stub = cardinal_stub(x, y, *direction);

            // A same-level Stub exit shows only bare directional stubs, no
            // connecting line — both ends of a bidirectional pair, the source
            // alone otherwise. Non-cardinal exits have no stub geometry, so they
            // fall through to the normal line and never vanish.
            if style == ExitStyle::Stub && from_stub.is_some() {
                if let Some((from, to)) = from_stub {
                    frame.stroke(&canvas::Path::line(from, to), stroke);
                }
                if connection.is_bidirectional
                    && let Some((from, to)) = to_stub
                {
                    frame.stroke(&canvas::Path::line(from, to), stroke);
                }
                return;
            }

            let conn_line = match (from_stub, to_stub) {
                (Some(from_stub), _) if !connection.is_bidirectional => {
                    let start = from_stub.1;
                    let end = Point { x, y };
                    let clipped_end = clip_line_end_to_square(start, end, MAP_ROOM_SIZE * 1.25);
                    Some((start, clipped_end))
                }
                (None, _) if !connection.is_bidirectional => {
                    let start = Point {
                        x: connection.from_x,
                        y: connection.from_y,
                    };
                    let end = Point { x, y };
                    let clipped_end = clip_line_end_to_square(start, end, MAP_ROOM_SIZE * 1.25);
                    Some((start, clipped_end))
                }
                (Some(from_stub), Some(to_stub)) => Some((from_stub.1, to_stub.1)),
                (Some(from_stub), None) => Some((from_stub.1, Point { x, y })),
                (None, Some(to_stub)) => Some((
                    Point {
                        x: connection.from_x,
                        y: connection.from_y,
                    },
                    to_stub.1,
                )),
                (None, None) => Some((
                    Point {
                        x: connection.from_x,
                        y: connection.from_y,
                    },
                    Point { x, y },
                )),
            };

            if let Some((from, to)) = from_stub {
                let path = canvas::Path::line(from, to);
                frame.stroke(&path, stroke);
            }

            if let Some((from, to)) = to_stub {
                let path = canvas::Path::line(from, to);
                frame.stroke(&path, stroke);
            }

            if let Some((from, to)) = conn_line {
                let path = canvas::Path::line(from, to);
                frame.stroke(&path, stroke);

                if !connection.is_bidirectional {
                    draw_arrow_head(
                        frame,
                        Vector {
                            x: from.x,
                            y: from.y,
                        },
                        Vector { x: to.x, y: to.y },
                        apply_opacity(EXIT_COLOR, fill_opacity),
                        0.1,
                    );
                }
            }
        }
        RoomConnectionEnd::ToLevel { level, .. } => {
            let up = level > connection.from_level
                || (level == connection.from_level
                    && connection.from_direction == ExitDirection::Up);
            let (x, y) = (connection.from_x, connection.from_y);
            if suppress_level_stubs {
                // Ghost passes keep the compact corner triangle.
                draw_level_stub(frame, x, y, up, fill_opacity);
            } else if style == ExitStyle::Stub {
                // Option 1: re-anchor the level triangle to the exit's side.
                let (cx, cy) = level_stub_anchor(x, y, connection.from_direction, up);
                draw_level_triangle(frame, cx, cy, up, fill_opacity);
            } else if let Some(unit) = cardinal_unit(connection.from_direction) {
                // Option 3: a fade-only directional gradient stub (no glyph)
                // for planar cardinals.
                draw_cross_level_stub(frame, x, y, unit, opacity, is_secret, style);
            } else {
                // Diagonal or non-planar (Up/Down): fall back to the corner.
                draw_level_stub(frame, x, y, up, fill_opacity);
            }
        }
        RoomConnectionEnd::External { area_id } => {
            let area_name = atlas.get_area(&area_id).map_or_else(
                || "(unknown area)".to_string(),
                |area| area.get_name().to_string(),
            );

            let (end, text_anchor, align_x, align_y) = out_of_area_stub(connection);

            let path = canvas::Path::line(
                Point {
                    x: connection.from_x,
                    y: connection.from_y,
                },
                end,
            );

            frame.stroke(&path, stroke);

            let circle = canvas::Path::circle(end, 0.075);

            frame.fill(&circle, apply_opacity(EXIT_COLOR, fill_opacity));

            let text = canvas::Text {
                content: area_name,
                position: text_anchor,
                align_x,
                align_y,
                color: apply_opacity(AREA_NAME_FONT_COLOR, fill_opacity),
                size: 0.375.into(),
                ..Default::default()
            };

            frame.fill_text(text);
        }
        RoomConnectionEnd::Unknown { .. } => {
            // Redacted destination: the link exists but its target was not
            // shared with the viewer. Render dimmer than a real external
            // stub, mark the stub end with a small "?", and label it with
            // the literal "Unknown map" — never a name or id. Exits whose
            // hidden destinations coincide share a server token and thus
            // converge on the identical label.
            let dim = fill_opacity * UNKNOWN_MAP_OPACITY;
            let (end, text_anchor, align_x, align_y) = out_of_area_stub(connection);

            let path = canvas::Path::line(
                Point {
                    x: connection.from_x,
                    y: connection.from_y,
                },
                end,
            );

            frame.stroke(
                &path,
                exit_stroke(opacity * UNKNOWN_MAP_OPACITY, is_secret, style),
            );

            // A small "?" stands in for the usual stub dot.
            frame.fill_text(canvas::Text {
                content: "?".to_string(),
                position: end,
                align_x: Alignment::Center,
                align_y: Vertical::Center,
                color: apply_opacity(EXIT_COLOR, dim),
                size: 0.3.into(),
                ..Default::default()
            });

            // Push the label one extra step out so it clears the "?" glyph.
            let label_anchor = Point {
                x: text_anchor.x + (text_anchor.x - end.x),
                y: text_anchor.y + (text_anchor.y - end.y),
            };
            frame.fill_text(canvas::Text {
                content: "Unknown map".to_string(),
                position: label_anchor,
                align_x,
                align_y,
                color: apply_opacity(AREA_NAME_FONT_COLOR, dim),
                size: 0.375.into(),
                ..Default::default()
            });
        }
        RoomConnectionEnd::SelfLoop => {
            draw_self_loop(
                frame,
                connection.from_x,
                connection.from_y,
                connection.from_direction,
                stroke,
            );
        }
        RoomConnectionEnd::None => match connection.from_direction {
            ExitDirection::Up | ExitDirection::Down => {
                draw_level_stub(
                    frame,
                    connection.from_x,
                    connection.from_y,
                    connection.from_direction == ExitDirection::Up,
                    fill_opacity,
                );
            }
            direction => {
                if let Some((from, to)) =
                    cardinal_stub(connection.from_x, connection.from_y, direction)
                {
                    let path = canvas::Path::line(from, to);
                    frame.stroke(&path, stroke);
                }
            }
        },
    }
}

/// Geometry shared by the out-of-area connection markers (external area and
/// "Unknown map"): the stub endpoint, the label anchor just beyond it, and
/// the label alignment, all derived from the exit's direction.
fn out_of_area_stub(connection: &RoomConnection) -> (Point, Point, Alignment, Vertical) {
    let (x, y, text_x, text_y, text_align_x, text_align_y) = match connection.from_direction {
        ExitDirection::North | ExitDirection::Up => (
            connection.from_x,
            connection.from_y - MAP_EXIT_STUB_LENGTH,
            connection.from_x,
            connection.from_y - MAP_EXIT_STUB_LENGTH - 0.1,
            Alignment::Center,
            Vertical::Bottom,
        ),
        ExitDirection::East => (
            connection.from_x + MAP_EXIT_STUB_LENGTH,
            connection.from_y,
            connection.from_x + MAP_EXIT_STUB_LENGTH + 0.1,
            connection.from_y,
            Alignment::Left,
            Vertical::Center,
        ),
        ExitDirection::West => (
            connection.from_x - MAP_EXIT_STUB_LENGTH,
            connection.from_y,
            connection.from_x - MAP_EXIT_STUB_LENGTH - 0.1,
            connection.from_y,
            Alignment::Right,
            Vertical::Center,
        ),
        ExitDirection::Northeast => (
            connection.from_x + MAP_EXIT_STUB_LENGTH,
            connection.from_y - MAP_EXIT_STUB_LENGTH,
            connection.from_x + MAP_EXIT_STUB_LENGTH + 0.1,
            connection.from_y - MAP_EXIT_STUB_LENGTH - 0.1,
            Alignment::Left,
            Vertical::Bottom,
        ),
        ExitDirection::Southeast => (
            connection.from_x + MAP_EXIT_STUB_LENGTH,
            connection.from_y + MAP_EXIT_STUB_LENGTH,
            connection.from_x + MAP_EXIT_STUB_LENGTH + 0.1,
            connection.from_y + MAP_EXIT_STUB_LENGTH + 0.1,
            Alignment::Left,
            Vertical::Top,
        ),
        ExitDirection::Southwest => (
            connection.from_x - MAP_EXIT_STUB_LENGTH,
            connection.from_y + MAP_EXIT_STUB_LENGTH,
            connection.from_x - MAP_EXIT_STUB_LENGTH - 0.1,
            connection.from_y + MAP_EXIT_STUB_LENGTH + 0.1,
            Alignment::Right,
            Vertical::Top,
        ),
        ExitDirection::Northwest => (
            connection.from_x - MAP_EXIT_STUB_LENGTH,
            connection.from_y - MAP_EXIT_STUB_LENGTH,
            connection.from_x - MAP_EXIT_STUB_LENGTH - 0.1,
            connection.from_y - MAP_EXIT_STUB_LENGTH - 0.1,
            Alignment::Right,
            Vertical::Bottom,
        ),
        _ => (
            connection.from_x,
            connection.from_y + MAP_EXIT_STUB_LENGTH,
            connection.from_x,
            connection.from_y + MAP_EXIT_STUB_LENGTH + 0.1,
            Alignment::Center,
            Vertical::Top,
        ),
    };

    (
        Point { x, y },
        Point {
            x: text_x,
            y: text_y,
        },
        text_align_x,
        text_align_y,
    )
}

/// Draws a room as a filled, outlined rounded square centered on its
/// coordinates, plus a small corner diamond when the room is secret. The
/// secret mark is suppressed entirely when `show_secrets` is false.
pub fn draw_room(frame: &mut canvas::Frame, room: &RoomCache, opacity: f32, show_secrets: bool) {
    let room_shape = canvas::Path::rounded_rectangle(
        Point {
            x: room.get_x() - MAP_ROOM_SIZE / 2.0,
            y: room.get_y() - MAP_ROOM_SIZE / 2.0,
        },
        MAP_ROOM_SIZE_AS_SIZE,
        MAP_ROOM_BORDER_RADIUS.into(),
    );

    frame.fill(&room_shape, apply_opacity(room.get_iced_color(), opacity));
    frame.stroke(
        &room_shape,
        solid_stroke(apply_opacity(Color::from_rgba8(0, 0, 0, 0.1), opacity), 2.0),
    );

    if show_secrets && room.is_secret() {
        draw_secret_mark(frame, room.get_x(), room.get_y(), opacity);
    }
}

/// Draws the small diamond marking a secret room, just outside its top-left
/// corner (mirroring the level stubs at the other corners).
fn draw_secret_mark(frame: &mut canvas::Frame, x: f32, y: f32, opacity: f32) {
    let cx = x - LEVEL_STUB_OFFSET;
    let cy = y - LEVEL_STUB_OFFSET;

    let mut path = canvas::path::Builder::new();
    path.move_to(Point::new(cx, cy - LEVEL_STUB_HALF_SIZE));
    path.line_to(Point::new(cx + LEVEL_STUB_HALF_SIZE, cy));
    path.line_to(Point::new(cx, cy + LEVEL_STUB_HALF_SIZE));
    path.line_to(Point::new(cx - LEVEL_STUB_HALF_SIZE, cy));
    path.close();

    frame.fill(&path.build(), apply_opacity(SECRET_MARK_COLOR, opacity));
}

/// Draws the player's position marker on a room center.
pub fn draw_player_indicator(frame: &mut canvas::Frame, x: f32, y: f32, opacity: f32) {
    let circle = canvas::Path::circle(Point { x, y }, MAP_PLAYER_INDICATOR_RADIUS);
    frame.fill(&circle, apply_opacity(Color::from_rgb8(0, 0, 255), opacity));
}

/// Buckets a CSS-style numeric font weight into iced's weight scale.
#[must_use]
fn font_weight(weight: i32) -> iced::font::Weight {
    match weight {
        i32::MIN..150 => iced::font::Weight::Thin,
        150..250 => iced::font::Weight::ExtraLight,
        250..350 => iced::font::Weight::Light,
        350..450 => iced::font::Weight::Normal,
        450..550 => iced::font::Weight::Medium,
        550..650 => iced::font::Weight::Semibold,
        650..750 => iced::font::Weight::Bold,
        750..850 => iced::font::Weight::ExtraBold,
        _ => iced::font::Weight::Black,
    }
}

/// The default zoom (pixels per map unit) at which a label's `font_size`
/// reads as that many pixels.
const LABEL_FONT_SIZE_REFERENCE_SCALING: f32 = 40.0;

/// Draws a text label: optional background fill, then text aligned within
/// the label's bounds. Secret labels draw at reduced opacity, unless
/// `show_secrets` is false, in which case secrecy is ignored.
pub fn draw_label(frame: &mut canvas::Frame, label: &Label, opacity: f32, show_secrets: bool) {
    let opacity = if show_secrets && label.is_secret {
        opacity * SECRET_OPACITY
    } else {
        opacity
    };
    let top_left = Point::new(label.x, label.y);
    let size = Size::new(label.width, label.height);

    if let Some(background) = parse_color(&label.background_color) {
        frame.fill(
            &canvas::Path::rectangle(top_left, size),
            apply_opacity(background, opacity),
        );
    }

    let (x, align_x) = match label.horizontal_alignment {
        HorizontalAlignment::Left => (label.x, Alignment::Left),
        HorizontalAlignment::Center => (label.x + label.width / 2.0, Alignment::Center),
        HorizontalAlignment::Right => (label.x + label.width, Alignment::Right),
    };
    let (y, align_y) = match label.vertical_alignment {
        VerticalAlignment::Top => (label.y, Vertical::Top),
        VerticalAlignment::Center => (label.y + label.height / 2.0, Vertical::Center),
        VerticalAlignment::Bottom => (label.y + label.height, Vertical::Bottom),
    };

    #[allow(clippy::cast_precision_loss)]
    let font_size = label.font_size as f32 / LABEL_FONT_SIZE_REFERENCE_SCALING;

    let text = canvas::Text {
        content: label.text.clone(),
        position: Point::new(x, y),
        align_x,
        align_y,
        color: apply_opacity(
            parse_color(&label.color).unwrap_or(Color::from_rgb8(200, 200, 200)),
            opacity,
        ),
        size: font_size.into(),
        font: iced::Font {
            weight: font_weight(label.font_weight),
            ..iced::Font::default()
        },
        ..Default::default()
    };

    frame.fill_text(text);
}

/// Draws a shape: optional fill and optional stroke. Secret shapes draw at
/// reduced opacity, unless `show_secrets` is false, in which case secrecy is
/// ignored.
pub fn draw_shape(frame: &mut canvas::Frame, shape: &Shape, opacity: f32, show_secrets: bool) {
    let opacity = if show_secrets && shape.is_secret {
        opacity * SECRET_OPACITY
    } else {
        opacity
    };
    let top_left = Point::new(shape.x, shape.y);
    let size = Size::new(shape.width, shape.height);

    let path = match shape.shape_type {
        ShapeType::Rectangle => canvas::Path::rectangle(top_left, size),
        ShapeType::RoundedRectangle => {
            canvas::Path::rounded_rectangle(top_left, size, shape.border_radius.into())
        }
    };

    if let Some(background) = shape.background_color.as_deref().and_then(parse_color) {
        frame.fill(&path, apply_opacity(background, opacity));
    }

    if shape.stroke_width > 0.0
        && let Some(stroke_color) = shape.stroke_color.as_deref().and_then(parse_color)
    {
        frame.stroke(
            &path,
            solid_stroke(apply_opacity(stroke_color, opacity), shape.stroke_width),
        );
    }
}
