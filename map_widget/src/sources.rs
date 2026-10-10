//! The map's other sources drawn beside it: each Secret, and the caller's
//! Private additions, in its own color. Their rooms, labels and shapes wear a
//! ring of that color, and their connections take it.

use std::sync::Arc;

use iced::widget::canvas;
use iced::{Color, Point, Size};
use smudgy_cloud::mapper::AtlasCache;
use smudgy_cloud::mapper::area_cache::{AreaCache, SourceLayer};
use smudgy_cloud::mapper::room_cache::RoomCache;
use smudgy_cloud::mapper::room_connection::RoomConnection;
use smudgy_cloud::{ConnectionId, LabelId, RoomNumber, ShapeId, SourceId};
use smudgy_palette::ColorSequence;

use crate::render::{self, MAP_ROOM_BORDER_RADIUS, MAP_ROOM_SIZE, apply_opacity, solid_stroke};

/// How far a ring stands off what it marks, in map units.
pub const RING_GAP: f32 = MAP_ROOM_SIZE * 0.16;
/// A ring's stroke width. Canvas strokes are in screen pixels whatever the
/// zoom, like every other outline on the map.
pub const RING_WIDTH: f32 = 2.5;

/// How far outside a ringed item its selection outline sits, in map units at
/// `scaling` pixels per unit: past the ring, with a hairline of space.
#[must_use]
pub fn ring_clearance(scaling: f32) -> f32 {
    RING_GAP + (RING_WIDTH + 2.0) / scaling
}

/// A label or shape, for skipping one while it is dragged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Drawing {
    Label(LabelId),
    Shape(ShapeId),
}

/// A layer with the color it draws in.
pub type ColoredLayer<'a> = (&'a SourceLayer, Color);

/// The area's layers with their colors, in layer order. A Secret with a
/// chosen color draws in it; the others take the palette sequence seeded by
/// the map's name and its first Secret's, by position; the caller's Private
/// additions take the position after the last Secret.
#[must_use]
pub fn colored_layers(area: &AreaCache) -> Vec<ColoredLayer<'_>> {
    let layers = area.source_layers();
    let first = layers
        .iter()
        .find(|layer| layer.source().is_secret())
        .and_then(SourceLayer::name)
        .unwrap_or_default();
    let sequence = ColorSequence::seeded(&[area.get_name(), first]);
    layers
        .iter()
        .enumerate()
        .map(|(index, layer)| {
            if let Some([r, g, b]) = layer.color() {
                return (layer, Color::from_rgb8(r, g, b));
            }
            let rgb = sequence.color(index);
            (layer, Color::from_rgb8(rgb.r, rgb.g, rgb.b))
        })
        .collect()
}

/// The color `source`'s layer draws in.
#[must_use]
pub fn layer_color(area: &AreaCache, source: SourceId) -> Option<Color> {
    colored_layers(area)
        .into_iter()
        .find(|(layer, _)| layer.source() == source)
        .map(|(_, color)| color)
}

/// The color content in `source` will draw in, including the caller's
/// Private additions before their first write creates the layer: the
/// position after the last Secret. `None` for the map, which has no ring.
#[must_use]
pub fn source_color(area: &AreaCache, source: SourceId) -> Option<Color> {
    if let Some(color) = layer_color(area, source) {
        return Some(color);
    }
    if source != SourceId::Private {
        return None;
    }
    let layers = area.source_layers();
    let first = layers
        .iter()
        .find(|layer| layer.source().is_secret())
        .and_then(SourceLayer::name)
        .unwrap_or_default();
    let rgb = ColorSequence::seeded(&[area.get_name(), first]).color(layers.len());
    Some(Color::from_rgb8(rgb.r, rgb.g, rgb.b))
}

/// One of a source's owned rooms.
#[must_use]
pub fn source_room(
    area: &AreaCache,
    source: SourceId,
    number: RoomNumber,
) -> Option<&Arc<RoomCache>> {
    let layer = area
        .source_layers()
        .iter()
        .find(|layer| layer.source() == source)?;
    layer.content().get_room(&number)
}

/// A ring around a room centered at `(x, y)`.
pub fn room_ring(frame: &mut canvas::Frame, x: f32, y: f32, color: Color, opacity: f32) {
    ring(
        frame,
        Point::new(x - MAP_ROOM_SIZE / 2.0, y - MAP_ROOM_SIZE / 2.0),
        Size::new(MAP_ROOM_SIZE, MAP_ROOM_SIZE),
        MAP_ROOM_BORDER_RADIUS,
        color,
        opacity,
    );
}

/// A ring around the rectangle at `top_left` of `size`.
pub fn ring(
    frame: &mut canvas::Frame,
    top_left: Point,
    size: Size,
    radius: f32,
    color: Color,
    opacity: f32,
) {
    let path = canvas::Path::rounded_rectangle(
        Point::new(top_left.x - RING_GAP, top_left.y - RING_GAP),
        Size::new(size.width + RING_GAP * 2.0, size.height + RING_GAP * 2.0),
        (radius + RING_GAP).into(),
    );
    frame.stroke(
        &path,
        solid_stroke(apply_opacity(color, opacity), RING_WIDTH),
    );
}

/// Each layer's shapes and labels on `level`, ringed, except those `skip`
/// names (a dragged selection draws itself). Positions scale by `spacing` as
/// the map's own do.
pub fn draw_drawings(
    frame: &mut canvas::Frame,
    layers: &[ColoredLayer<'_>],
    level: i32,
    spacing: f32,
    opacity: f32,
    skip: &dyn Fn(Drawing) -> bool,
) {
    for (layer, color) in layers {
        for shape in layer.content().get_shapes() {
            if shape.level == level && !skip(Drawing::Shape(shape.id)) {
                let mut shape = shape.clone();
                shape.x *= spacing;
                shape.y *= spacing;
                render::draw_shape(frame, &shape, opacity);
                ring(
                    frame,
                    Point::new(shape.x, shape.y),
                    Size::new(shape.width, shape.height),
                    shape.border_radius,
                    *color,
                    opacity,
                );
            }
        }
        for label in layer.content().get_labels() {
            if label.level == level && !skip(Drawing::Label(label.id)) {
                let mut label = label.clone();
                label.x *= spacing;
                label.y *= spacing;
                render::draw_label(frame, &label, opacity);
                ring(
                    frame,
                    Point::new(label.x, label.y),
                    Size::new(label.width, label.height),
                    0.0,
                    *color,
                    opacity,
                );
            }
        }
    }
}

/// One of `source`'s own rooms or, for the map, one of the map's: a room as
/// a link can end at it.
#[must_use]
pub fn placed_room(
    area: &AreaCache,
    source: SourceId,
    number: RoomNumber,
) -> Option<&Arc<RoomCache>> {
    if source.is_map() {
        area.get_room(&number)
    } else {
        source_room(area, source, number)
    }
}

/// Each layer's connections leaving rooms on `level` within the query box,
/// in the layer's color, except those `skip` names (a dragged link draws
/// itself). Every link into another map names it.
#[allow(clippy::too_many_arguments)]
pub fn draw_connections(
    frame: &mut canvas::Frame,
    atlas: &AtlasCache,
    layers: &[ColoredLayer<'_>],
    level: i32,
    bounds: (f32, f32, f32, f32),
    spacing: f32,
    opacity: f32,
    skip: &dyn Fn(ConnectionId) -> bool,
) {
    draw_connections_labelled(
        frame,
        atlas,
        layers,
        level,
        bounds,
        spacing,
        opacity,
        skip,
        &|_, _| true,
    );
}

/// [`draw_connections`], naming another map at the end of a link only where
/// `label` says, given the link's layer and its half leaving a room there.
#[allow(clippy::too_many_arguments)]
pub fn draw_connections_labelled(
    frame: &mut canvas::Frame,
    atlas: &AtlasCache,
    layers: &[ColoredLayer<'_>],
    level: i32,
    (min_x, min_y, max_x, max_y): (f32, f32, f32, f32),
    spacing: f32,
    opacity: f32,
    skip: &dyn Fn(ConnectionId) -> bool,
    label: &dyn Fn(&SourceLayer, &RoomConnection) -> bool,
) {
    for (layer, color) in layers {
        layer
            .content()
            .with_room_connections_in(min_x, min_y, max_x, max_y, |connection| {
                if connection.from_level == level && !skip(connection.connection_id) {
                    render::draw_connection_styled(
                        frame,
                        atlas,
                        &connection.with_room_spacing(spacing),
                        opacity,
                        false,
                        Some(*color),
                        None,
                        None,
                        label(layer, connection),
                        None,
                    );
                }
            });
    }
}

/// Each layer's own rooms on `level` within the query box, ringed, except
/// those `skip` names (a dragged selection draws itself).
#[allow(clippy::too_many_arguments)]
pub fn draw_rooms(
    frame: &mut canvas::Frame,
    layers: &[ColoredLayer<'_>],
    level: i32,
    (min_x, min_y, max_x, max_y): (f32, f32, f32, f32),
    spacing: f32,
    opacity: f32,
    skip: &dyn Fn(SourceId, RoomNumber) -> bool,
) {
    for (layer, color) in layers {
        layer
            .content()
            .with_rooms_in(min_x, min_y, max_x, max_y, |room| {
                let number = room.get_room_number();
                if room.get_level() != level || skip(layer.source(), number) {
                    return;
                }
                let (x, y) = (room.get_x() * spacing, room.get_y() * spacing);
                render::draw_room_styled(
                    frame,
                    room,
                    x,
                    y,
                    opacity,
                    &crate::ResolvedRoomStyle::default(),
                );
                room_ring(frame, x, y, *color, opacity);
            });
    }
}
