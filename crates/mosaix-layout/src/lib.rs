//! Layout planners: manual zone planner and automatic tree planner with strategies and normalization.

mod displays;
mod zones;

pub use displays::{cycle_display, throw_preserving_ratio, DisplayDirection};
pub use zones::{
    apply_gaps, center_on, maximize_to_work_area, resolve_zone_cycle, snap_to_half,
    snap_to_quarter, snap_to_third, CycleStep, HalfZone, HorizontalDirection, QuarterZone,
    ThirdZone,
};
