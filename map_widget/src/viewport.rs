//! Shared camera math for map widgets: screen ⇄ map-space conversion and
//! grid snapping. Map space measures in rooms (one grid unit per room
//! center); screen space measures in pixels with the origin at the canvas
//! center.

use iced::{Point, Size, Vector};

/// The portion of map space visible through a viewport, in map units.
#[derive(Debug, Clone, Copy)]
pub struct Region {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Region {
    #[must_use]
    pub fn contains(&self, point: Point) -> bool {
        point.x >= self.x
            && point.x <= self.x + self.width
            && point.y >= self.y
            && point.y <= self.y + self.height
    }
}

/// A camera over map space: `translation` is the map-space offset of the
/// view center (negated room coordinates center the view on that room) and
/// `scaling` is the zoom in pixels per map unit.
#[derive(Debug, Clone, Copy)]
pub struct Viewport {
    pub translation: Vector,
    pub scaling: f32,
}

impl Viewport {
    /// The grid pitch rooms snap to, in map units.
    pub const GRID_UNIT: f32 = 1.0;

    #[must_use]
    pub fn visible_region(&self, size: Size) -> Region {
        let width = size.width / self.scaling;
        let height = size.height / self.scaling;

        Region {
            x: -self.translation.x - width / 2.0,
            y: -self.translation.y - height / 2.0,
            width,
            height,
        }
    }

    /// Converts a screen-space position (relative to the canvas top-left)
    /// into map space.
    #[must_use]
    pub fn project(&self, position: Point, size: Size) -> Point {
        let region = self.visible_region(size);

        Point::new(
            position.x / self.scaling + region.x,
            position.y / self.scaling + region.y,
        )
    }

    /// Converts a map-space position into screen space (relative to the
    /// canvas top-left). Inverse of [`Self::project`].
    #[must_use]
    pub fn unproject(&self, position: Point, size: Size) -> Point {
        let region = self.visible_region(size);

        Point::new(
            (position.x - region.x) * self.scaling,
            (position.y - region.y) * self.scaling,
        )
    }
}

/// Snaps a map-space point to the room grid.
#[must_use]
pub fn snap(point: Point) -> Point {
    Point::new(point.x.round(), point.y.round())
}

/// Snaps a map-space offset to whole grid steps, preserving the relative
/// alignment of everything moved by it.
#[must_use]
pub fn snap_offset(offset: Vector) -> Vector {
    Vector::new(offset.x.round(), offset.y.round())
}
