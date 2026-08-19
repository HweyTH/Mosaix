//! Core domain types: IDs, geometry primitives, state models, commands, and events.

pub mod geometry;
pub mod id;
pub mod window;

// Re-export commonly used types at the crate root for convenience.
pub use geometry::Rect;
pub use id::{ApplicationId, DisplayId, WindowId};
pub use window::{Window, WindowCapabilities, WindowLifecycle, WindowRole};
