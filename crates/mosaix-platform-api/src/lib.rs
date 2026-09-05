//! Platform adapter traits, capability model, and shared platform abstractions.

pub mod adapter;
pub mod error;
pub mod parking;

pub use adapter::PlatformAdapter;
pub use error::PlatformError;
pub use parking::{
    plan_parking_sites, virtual_screen_of, ParkedAs, ParkingEdge, ParkingSite, ParkingSiteRefusal,
    COORDINATE_LIMIT, PARKING_MARGIN, PROBE_EXTENT,
};
