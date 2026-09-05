//! Platform-specific error types.

use thiserror::Error;

/// Errors originating from platform adapter operations.
#[derive(Debug, Error)]
pub enum PlatformError {
    /// A Win32 API call failed with the given error code.
    #[error("Win32 error: code {0:#010x}")]
    Win32Error(u32),

    /// A macOS Accessibility API call failed with the given AXError code.
    #[error("Accessibility error: code {0}")]
    AccessibilityError(i32),

    /// A Core Graphics operation failed.
    #[error("Core Graphics error: {0}")]
    CoreGraphicsError(String),

    /// An unexpected or unclassified error.
    #[error("platform error: {0}")]
    Unexpected(String),
}
