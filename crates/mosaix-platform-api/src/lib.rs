//! Platform adapter traits, capability model, and shared platform abstractions.

pub mod adapter;
pub mod error;

pub use adapter::PlatformAdapter;
pub use error::PlatformError;
