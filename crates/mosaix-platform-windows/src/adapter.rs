//! Windows platform adapter implementing `PlatformAdapter`.

use mosaix_domain::Window;
use mosaix_platform_api::PlatformAdapter;
use tracing::info;

use crate::enumeration;

/// Win32 implementation of the platform adapter.
///
/// Currently supports window enumeration. Display enumeration, event
/// subscription, and placement execution will be added in future features.
#[derive(Debug, Default)]
pub struct WindowsPlatformAdapter;

impl WindowsPlatformAdapter {
    /// Create a new Windows platform adapter.
    pub fn new() -> Self {
        Self
    }
}

impl PlatformAdapter for WindowsPlatformAdapter {
    fn enumerate_windows(&self) -> anyhow::Result<Vec<Window>> {
        let windows = enumeration::enumerate_windows()?;
        info!(count = windows.len(), "enumerated manageable windows");
        Ok(windows)
    }
}
