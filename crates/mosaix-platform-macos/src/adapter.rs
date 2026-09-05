use mosaix_domain::Window;
use mosaix_platform_api::PlatformAdapter;

use crate::{accessibility, enumeration, MacosError, Result};

/// macOS implementation of the shared platform adapter contract.
#[derive(Debug, Default)]
pub struct MacosPlatformAdapter;

impl MacosPlatformAdapter {
    /// Creates an adapter only after the OS has authorized Accessibility use.
    pub fn new() -> Result<Self> {
        if accessibility::is_process_trusted() {
            Ok(Self)
        } else {
            Err(MacosError::AccessibilityPermissionDenied)
        }
    }
}

impl PlatformAdapter for MacosPlatformAdapter {
    fn enumerate_windows(&self) -> anyhow::Result<Vec<Window>> {
        enumeration::enumerate_windows()
    }
}
