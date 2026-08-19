//! Layout planners: manual zone planner and automatic tree planner with strategies and normalization.

mod zones;

pub use zones::{
    center_on, maximize_to_work_area, snap_to_half, snap_to_quarter, snap_to_third, HalfZone,
    QuarterZone, ThirdZone,
};
