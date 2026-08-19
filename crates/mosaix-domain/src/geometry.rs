//! Geometry primitives for window and display bounds.
//!
//! All coordinates are logical, top-left origin. Negative values are valid
//! for monitors left of or above the primary display. Only platform adapters
//! convert between core logical coordinates and native coordinate systems
//! (see ARCHITECTURE §10).

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
}
