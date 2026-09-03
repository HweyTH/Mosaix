//! Minimal, ownership-safe boundary around the public Accessibility API.

use std::ffi::c_void;

use crate::{MacosError, Result};

pub type AXUIElementRef = *const c_void;
pub type AXError = i32;
pub const K_AX_ERROR_SUCCESS: AXError = 0;
pub const K_AX_WINDOWS_ATTRIBUTE: &str = "AXWindows";
pub const K_AX_TITLE_ATTRIBUTE: &str = "AXTitle";
pub const K_AX_POSITION_ATTRIBUTE: &str = "AXPosition";
pub const K_AX_SIZE_ATTRIBUTE: &str = "AXSize";
pub const K_AX_ROLE_ATTRIBUTE: &str = "AXRole";
pub const K_AX_SUBROLE_ATTRIBUTE: &str = "AXSubrole";
pub const K_AX_MINIMIZED_ATTRIBUTE: &str = "AXMinimized";
pub const K_AX_FULL_SCREEN_ATTRIBUTE: &str = "AXFullScreen";

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
}

/// An app-scoped Accessibility element. It is deliberately opaque: callers
/// never retain or persist native accessibility handles as window identity.
#[derive(Debug)]
pub struct ApplicationElement(AXUIElementRef);

impl ApplicationElement {
    pub fn for_process(pid: u32) -> Result<Self> {
        let element = unsafe { AXUIElementCreateApplication(pid as i32) };
        if element.is_null() {
            Err(MacosError::AccessibilityPermissionDenied)
        } else {
            Ok(Self(element))
        }
    }

    pub fn as_ptr(&self) -> AXUIElementRef {
        self.0
    }
}

/// Returns whether the user has granted this process Accessibility access.
pub fn is_process_trusted() -> bool {
    unsafe { AXIsProcessTrusted() }
}

pub fn ax_result(code: AXError) -> Result<()> {
    if code == K_AX_ERROR_SUCCESS {
        Ok(())
    } else {
        Err(MacosError::Accessibility(code))
    }
}
