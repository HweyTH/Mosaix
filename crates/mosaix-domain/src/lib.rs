//! Core domain types: IDs, geometry primitives, state models, commands, and events.

mod coordinates;
pub mod display;
pub mod geometry;
pub mod id;
pub mod window;

// Re-export commonly used types at the crate root for convenience.
pub use coordinates::{allocate_edges, LogicalRect, NormalizedRect};
pub use display::{topology_fingerprint, Display, Rotation};
pub use geometry::{Gaps, Rect};
pub use id::{ApplicationId, DisplayId, WindowId};
pub use window::{Window, WindowCapabilities, WindowLifecycle, WindowRole};
