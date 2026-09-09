//! Geometry primitives for window and display bounds.
//!
//! All coordinates are logical, top-left origin. Negative values are valid
//! for monitors left of or above the primary display. Only platform
//! adapters convert between core logical coordinates and native coordinate
//! systems.

use serde::{Deserialize, Serialize};

/// An axis-aligned rectangle in logical coordinates.
///
/// `(x, y)` is the top-left corner. `width` and `height` are extents.
/// Negative positions are valid (multi-monitor). Dimensions should be
/// non-negative for well-formed rects, but the type does not enforce this
/// so that raw OS data can be represented before validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    /// Create a new rectangle.
    pub const fn new(x: i32, y: i32, width: i32, height: i32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// Returns `true` if the rectangle has positive area.
    pub const fn has_positive_area(&self) -> bool {
        self.width > 0 && self.height > 0
    }

    /// Right edge (exclusive).
    pub const fn right(&self) -> i32 {
        self.x + self.width
    }

    /// Bottom edge (exclusive).
    pub const fn bottom(&self) -> i32 {
        self.y + self.height
    }

    /// Whether `other` lies entirely within this rectangle.
    pub const fn contains(&self, other: &Rect) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }
}

/// A width and height without a position, in logical coordinates.
///
/// What a window's minimum size is expressed as: the platform reports the
/// smallest extent a window will accept, and the tree planner refuses to
/// issue a placement below it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Size {
    pub width: i32,
    pub height: i32,
}

impl Size {
    pub const fn new(width: i32, height: i32) -> Self {
        Self { width, height }
    }

    /// Whether a rectangle of this size fits inside `rect`.
    pub const fn fits_within(&self, rect: Rect) -> bool {
        rect.width >= self.width && rect.height >= self.height
    }
}

/// Configurable gap insets applied to a computed zone [`Rect`] as a
/// post-processing step, kept separate from the pure zone functions that
/// compute the rect itself.
///
/// Both values are uniform scalars, not per-edge. *Outer gap* insets edges
/// that touch the container's boundary; *inner gap* insets edges that
/// would border a neighboring zone.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Gaps {
    pub outer: i32,
    pub inner: i32,
}

impl Gaps {
    /// Create a new gap configuration.
    pub const fn new(outer: i32, inner: i32) -> Self {
        Self { outer, inner }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_positive_area() {
        assert!(Rect::new(0, 0, 100, 200).has_positive_area());
        assert!(!Rect::new(0, 0, 0, 200).has_positive_area());
        assert!(!Rect::new(0, 0, 100, 0).has_positive_area());
        assert!(!Rect::new(0, 0, -1, 10).has_positive_area());
    }

    #[test]
    fn rect_edges() {
        let r = Rect::new(-100, -50, 300, 200);
        assert_eq!(r.right(), 200);
        assert_eq!(r.bottom(), 150);
    }

    #[test]
    fn rect_negative_origin_is_valid() {
        let r = Rect::new(-1920, -1080, 1920, 1080);
        assert!(r.has_positive_area());
        assert_eq!(r.right(), 0);
        assert_eq!(r.bottom(), 0);
    }

    #[test]
    fn rect_contains_is_inclusive_of_edges() {
        let outer = Rect::new(0, 0, 100, 100);
        assert!(outer.contains(&Rect::new(0, 0, 100, 100)));
        assert!(outer.contains(&Rect::new(10, 10, 50, 50)));
        assert!(!outer.contains(&Rect::new(-1, 0, 100, 100)));
        assert!(!outer.contains(&Rect::new(0, 0, 101, 100)));
    }
}
