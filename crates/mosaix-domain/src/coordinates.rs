//! Coordinate system: logical/physical DPI conversion, normalized-fraction
//! zones, and the deterministic edge-allocation algorithm (architecture doc
//! section 10, "Geometry and coordinate rules").
//!
//! Three coordinate spaces appear in the architecture doc:
//!
//! - **Physical pixels** ([`Rect`], already used by [`Display`](crate::Display)):
//!   raw device pixels as a platform adapter observes them (e.g. Win32
//!   monitor/window rects under Per-Monitor DPI Awareness V2).
//! - **Logical coordinates** ([`LogicalRect`]): the 96-DPI-baseline space
//!   the doc says core/layout logic should operate in. `Display.full_bounds`/
//!   `work_area` are still physical `Rect`s as populated by the Windows
//!   adapter today -- routing them through `LogicalRect` is follow-up work
//!   for whichever layer first needs DPI-independent math; only the
//!   conversion itself lives here.
//! - **Normalized fractions** ([`NormalizedRect`]): the persisted-zone
//!   format, fractions of a reference rect's width/height.
//!
//! Every conversion here rounds *edges*, never width/height independently,
//! and [`allocate_edges`] extends that to N-way splits. This is the "one
//! deterministic edge-allocation algorithm" the doc requires so adjacent
//! tiles neither overlap nor leave cumulative gaps.

use crate::Rect;

/// Rounds ties away from zero -- the one rounding rule every conversion in
/// this module uses, so independently-rounded values still compose without
/// dueling rounding rules producing a 1px gap or overlap.
fn round_half_away_from_zero(value: f64) -> i32 {
    value.round() as i32
}

/// A rectangle in logical (DPI-independent, 96-DPI-baseline) coordinates --
/// the space the architecture doc says core and layout logic operate in.
/// Only platform adapters convert to/from physical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LogicalRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl LogicalRect {
    /// Converts to physical pixels at `scale_factor` (1.5 == 150%, ...).
    ///
    /// Rounds the rectangle's two corners independently, then derives
    /// width/height from the rounded corners rather than rounding
    /// width/height on their own -- otherwise two logical rects that share
    /// an edge could round to physical rects that overlap or gap by a
    /// pixel.
    pub fn to_physical(&self, scale_factor: f64) -> Rect {
        debug_assert!(scale_factor > 0.0, "scale factor must be positive");

        let x1 = round_half_away_from_zero(self.x * scale_factor);
        let y1 = round_half_away_from_zero(self.y * scale_factor);
        let x2 = round_half_away_from_zero((self.x + self.width) * scale_factor);
        let y2 = round_half_away_from_zero((self.y + self.height) * scale_factor);

        Rect::new(x1, y1, (x2 - x1).max(0), (y2 - y1).max(0))
    }
}

impl Rect {
    /// Converts physical pixels to logical (96-DPI-baseline) coordinates at
    /// `scale_factor`. Exact -- the target is float, so no rounding is
    /// needed.
    pub fn to_logical(&self, scale_factor: f64) -> LogicalRect {
        debug_assert!(scale_factor > 0.0, "scale factor must be positive");

        LogicalRect {
            x: self.x as f64 / scale_factor,
            y: self.y as f64 / scale_factor,
            width: self.width as f64 / scale_factor,
            height: self.height as f64 / scale_factor,
        }
    }
}

/// A rectangle as fractions (normally 0.0-1.0) of a reference rect's
/// width/height -- the persisted-zone format from architecture doc section
/// 10, e.g. `{ "x": 0.0, "y": 0.0, "width": 0.5, "height": 1.0 }` for the
/// left half of a display's work area.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalizedRect {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

impl NormalizedRect {
    /// Resolves against `container`, rounding corners rather than
    /// width/height independently (see module docs).
    ///
    /// Safe to use independently for a set of zones meant to share an edge
    /// as long as the shared boundary is the same fraction value each time
    /// (e.g. two halves both using the literal `0.5` boundary) -- rounding
    /// the same value the same way always agrees. When the boundaries come
    /// from relative weights instead of a literal shared fraction (an even
    /// N-way split, or the container weights from architecture doc section
    /// 7.3), compute them once with [`allocate_edges`] instead.
    pub fn to_rect(&self, container: Rect) -> Rect {
        debug_assert!(
            container.width > 0 && container.height > 0,
            "container must have positive size"
        );

        let cw = container.width as f64;
        let ch = container.height as f64;

        let x1 = container.x + round_half_away_from_zero(self.x * cw);
        let y1 = container.y + round_half_away_from_zero(self.y * ch);
        let x2 = container.x + round_half_away_from_zero((self.x + self.width) * cw);
        let y2 = container.y + round_half_away_from_zero((self.y + self.height) * ch);

        Rect::new(x1, y1, (x2 - x1).max(0), (y2 - y1).max(0))
    }

    /// The inverse of [`to_rect`](Self::to_rect): expresses `rect` as
    /// fractions of `container`.
    pub fn from_rect(rect: Rect, container: Rect) -> NormalizedRect {
        debug_assert!(
            container.width > 0 && container.height > 0,
            "container must have positive size"
        );

        NormalizedRect {
            x: (rect.x - container.x) as f64 / container.width as f64,
            y: (rect.y - container.y) as f64 / container.height as f64,
            width: rect.width as f64 / container.width as f64,
            height: rect.height as f64 / container.height as f64,
        }
    }
}

/// Splits `total` (pixels, physical or logical) into `weights.len()`
/// contiguous, non-overlapping segments proportional to `weights` (which
/// need not sum to 1 -- they're normalized by their sum, like the container
/// weights in architecture doc section 7.3), returning each segment's
/// `[start, end)` pixel range in order.
///
/// This is the "one deterministic edge-allocation algorithm" architecture
/// doc section 10 requires: boundaries are rounded from *cumulative*
/// weight, so `segments[i].1 == segments[i + 1].0` always holds exactly,
/// and the final boundary is pinned to `total`. Rounding error lands as at
/// most one pixel of width difference between segments, never as a gap or
/// overlap between them -- rounding each segment's width independently
/// (the naive approach) does not have this guarantee: three equal thirds
/// of a 100px span each round to 33px, one pixel short of 100.
pub fn allocate_edges(total: i32, weights: &[f64]) -> Vec<(i32, i32)> {
    if weights.is_empty() {
        return Vec::new();
    }

    debug_assert!(total >= 0, "total must be non-negative");
    debug_assert!(weights.iter().all(|w| *w >= 0.0), "weights must be non-negative");
    let sum: f64 = weights.iter().sum();
    debug_assert!(sum > 0.0, "weights must not all be zero");

    let mut boundaries = Vec::with_capacity(weights.len() + 1);
    boundaries.push(0);
    let mut cumulative = 0.0;
    for &weight in weights {
        cumulative += weight;
        boundaries.push(round_half_away_from_zero(cumulative / sum * total as f64));
    }
    // Pin the last boundary to `total` exactly, guarding against float
    // error in the cumulative sum drifting it off by a pixel.
    *boundaries.last_mut().unwrap() = total;

    boundaries.windows(2).map(|pair| (pair[0], pair[1])).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_to_physical_rounds_ties_away_from_zero() {
        let rect = LogicalRect {
            x: 10.0,
            y: 0.0,
            width: 10.0,
            height: 5.0,
        };
        assert_eq!(
            rect.to_physical(1.5),
            Rect {
                x: 15,
                y: 0,
                width: 15,
                height: 8,
            }
        );
    }

    #[test]
    fn physical_to_logical_is_exact() {
        let rect = Rect {
            x: 15,
            y: 0,
            width: 30,
            height: 8,
        };
        let logical = rect.to_logical(1.5);
        assert_eq!(
            logical,
            LogicalRect {
                x: 10.0,
                y: 0.0,
                width: 20.0,
                height: 8.0 / 1.5,
            }
        );
    }

    #[test]
    fn logical_to_physical_adjacent_rects_share_exact_edge() {
        let scale = 1.25;
        let left = LogicalRect {
            x: 0.0,
            y: 0.0,
            width: 33.333,
            height: 100.0,
        };
        let right = LogicalRect {
            x: 33.333,
            y: 0.0,
            width: 33.334,
            height: 100.0,
        };

        let left_physical = left.to_physical(scale);
        let right_physical = right.to_physical(scale);

        assert_eq!(left_physical.x + left_physical.width, right_physical.x);
    }

    #[test]
    fn normalized_rect_left_half_of_work_area() {
        let container = Rect {
            x: 0,
            y: 0,
            width: 1920,
            height: 1080,
        };
        let left_half = NormalizedRect {
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        };

        assert_eq!(
            left_half.to_rect(container),
            Rect {
                x: 0,
                y: 0,
                width: 960,
                height: 1080,
            }
        );
    }

    #[test]
    fn normalized_rect_from_rect_round_trips() {
        let container = Rect {
            x: 100,
            y: 50,
            width: 1920,
            height: 1080,
        };
        let rect = Rect {
            x: 100,
            y: 50,
            width: 960,
            height: 1080,
        };

        let normalized = NormalizedRect::from_rect(rect, container);
        assert_eq!(
            normalized,
            NormalizedRect {
                x: 0.0,
                y: 0.0,
                width: 0.5,
                height: 1.0,
            }
        );
        assert_eq!(normalized.to_rect(container), rect);
    }

    #[test]
    fn normalized_rect_halves_are_contiguous_even_with_odd_container_width() {
        let container = Rect {
            x: 0,
            y: 0,
            width: 1921,
            height: 1080,
        };
        let left = NormalizedRect {
            x: 0.0,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        }
        .to_rect(container);
        let right = NormalizedRect {
            x: 0.5,
            y: 0.0,
            width: 0.5,
            height: 1.0,
        }
        .to_rect(container);

        assert_eq!(left.x + left.width, right.x, "halves must share an exact edge");
        assert_eq!(left.width + right.width, container.width, "halves must cover the container exactly");
    }

    #[test]
    fn allocate_edges_thirds_of_100_sum_exactly_with_no_gap() {
        let segments = allocate_edges(100, &[1.0, 1.0, 1.0]);
        assert_eq!(segments.len(), 3);

        for pair in segments.windows(2) {
            assert_eq!(pair[0].1, pair[1].0, "segments must be contiguous: {segments:?}");
        }
        assert_eq!(segments.first().unwrap().0, 0);
        assert_eq!(segments.last().unwrap().1, 100);

        let total_width: i32 = segments.iter().map(|(start, end)| end - start).sum();
        assert_eq!(total_width, 100, "widths must sum to the full span, not fall short by rounding");
    }

    #[test]
    fn allocate_edges_is_gapless_and_covers_total_across_various_splits() {
        let cases: &[(i32, &[f64])] = &[
            (100, &[1.0, 1.0, 1.0]),
            (1920, &[1.0, 2.0, 1.0]),
            (7, &[1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0]),
            (0, &[1.0, 1.0]),
            (1, &[1.0]),
            (100, &[0.5, 0.0, 0.5]),
        ];

        for &(total, weights) in cases {
            let segments = allocate_edges(total, weights);
            assert_eq!(segments.len(), weights.len());
            assert_eq!(segments.first().unwrap().0, 0, "case {total}/{weights:?}");
            assert_eq!(segments.last().unwrap().1, total, "case {total}/{weights:?}");

            for pair in segments.windows(2) {
                assert_eq!(pair[0].1, pair[1].0, "case {total}/{weights:?}: {segments:?}");
            }
            for &(start, end) in &segments {
                assert!(end >= start, "case {total}/{weights:?}: {segments:?}");
            }
        }
    }

    #[test]
    fn allocate_edges_empty_weights_returns_empty() {
        assert_eq!(allocate_edges(100, &[]), Vec::new());
    }
}
