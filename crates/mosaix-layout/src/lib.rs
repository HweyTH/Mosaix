//! Layout planners: manual zone planner and automatic tree planner with strategies and normalization.

mod balanced;
mod displays;
mod saved;
mod zones;

pub use balanced::plan_balanced_grid;
pub use displays::{cycle_display, throw_preserving_ratio, DisplayDirection};
pub use saved::resolve_saved_layout;
pub use zones::{
    apply_gaps, center_on, half_zone_at_edge, maximize_to_work_area, resolve_zone_cycle,
    snap_to_half, snap_to_quarter, snap_to_third, CycleStep, HalfZone, HorizontalDirection,
    QuarterZone, ThirdZone,
};
