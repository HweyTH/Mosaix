//! Minimal, ownership-safe boundary around the public Accessibility API.
//!
//! Every element this module hands out owns its native reference and
//! releases it on drop, so no caller has to reason about Core Foundation
//! retain counts. Nothing here uses a private interface: the whole
//! surface is `AXUIElementCreateApplication`,
//! `AXUIElementCopyAttributeValue`, `AXUIElementSetAttributeValue`, and
//! the `AXValue` boxing helpers.

use std::ffi::c_void;

use core_foundation::array::CFArray;
use core_foundation::base::{CFRelease, CFTypeRef, TCFType};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::geometry::{CGPoint, CGSize};

use crate::{MacosError, Result};

pub type AXUIElementRef = *const c_void;
pub type AXValueRef = *const c_void;
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

/// `AXValue` box types, from `AXValue.h`.
const K_AX_VALUE_CG_POINT_TYPE: u32 = 1;
const K_AX_VALUE_CG_SIZE_TYPE: u32 = 2;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    fn AXValueCreate(value_type: u32, value_ptr: *const c_void) -> AXValueRef;
    fn AXValueGetValue(value: AXValueRef, value_type: u32, value_ptr: *mut c_void) -> bool;
}

/// An owned Accessibility element. Dropping it releases the native
/// reference, so nothing here can leak a `CFTypeRef` on an error path.
#[derive(Debug)]
pub struct Element(AXUIElementRef);

impl Drop for Element {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0 as CFTypeRef) };
        }
    }
}

impl Element {
    /// Adopts a reference this process already owns (a "create rule"
    /// reference). The element releases it on drop.
    ///
    /// # Safety
    ///
    /// `element` must be a non-null, owned `AXUIElementRef` that no other
    /// value will release.
    pub unsafe fn from_owned(element: AXUIElementRef) -> Self {
        Self(element)
    }

    pub fn as_ptr(&self) -> AXUIElementRef {
        self.0
    }

    /// Reads an attribute, or `None` when the element does not carry it.
    ///
    /// An application that is busy or has gone away answers with an error
    /// rather than a value, which is a missing attribute as far as every
    /// caller here is concerned.
    fn copy_attribute(&self, attribute: &str) -> Option<CFTypeRef> {
        let key = CFString::new(attribute);
        let mut value: CFTypeRef = std::ptr::null();
        let code =
            unsafe { AXUIElementCopyAttributeValue(self.0, key.as_concrete_TypeRef(), &mut value) };
        (code == K_AX_ERROR_SUCCESS && !value.is_null()).then_some(value)
    }

    /// The element's `AXTitle`, when it has one.
    pub fn title(&self) -> Option<String> {
        let value = self.copy_attribute(K_AX_TITLE_ATTRIBUTE)?;
        let title = unsafe { CFString::wrap_under_create_rule(value as _) };
        Some(title.to_string())
    }

    /// A string-valued attribute such as `AXRole` or `AXSubrole`.
    pub fn string_attribute(&self, attribute: &str) -> Option<String> {
        let value = self.copy_attribute(attribute)?;
        let string = unsafe { CFString::wrap_under_create_rule(value as _) };
        Some(string.to_string())
    }

    /// A boolean-valued attribute such as `AXMinimized` or `AXFullScreen`.
    pub fn bool_attribute(&self, attribute: &str) -> Option<bool> {
        let value = self.copy_attribute(attribute)?;
        let boolean = unsafe { CFBoolean::wrap_under_create_rule(value as _) };
        Some(boolean.into())
    }

    /// The element's `AXPosition`, in Core Graphics' top-left global
    /// space. Accessibility, unlike Core Graphics' window list, already
    /// reports positions from the top left, so no flip belongs here.
    pub fn position(&self) -> Option<(i32, i32)> {
        let value = self.copy_attribute(K_AX_POSITION_ATTRIBUTE)?;
        let mut point = CGPoint::new(0.0, 0.0);
        let read = unsafe {
            AXValueGetValue(
                value as AXValueRef,
                K_AX_VALUE_CG_POINT_TYPE,
                &mut point as *mut CGPoint as *mut c_void,
            )
        };
        unsafe { CFRelease(value) };
        read.then(|| (point.x.round() as i32, point.y.round() as i32))
    }

    /// The element's `AXSize`.
    pub fn size(&self) -> Option<(i32, i32)> {
        let value = self.copy_attribute(K_AX_SIZE_ATTRIBUTE)?;
        let mut size = CGSize::new(0.0, 0.0);
        let read = unsafe {
            AXValueGetValue(
                value as AXValueRef,
                K_AX_VALUE_CG_SIZE_TYPE,
                &mut size as *mut CGSize as *mut c_void,
            )
        };
        unsafe { CFRelease(value) };
        read.then(|| (size.width.round() as i32, size.height.round() as i32))
    }

    /// Moves the element to `(x, y)`.
    ///
    /// Setting `AXPosition` does not raise or activate the window, which
    /// is what makes parking without activation possible on macOS at all.
    pub fn set_position(&self, x: i32, y: i32) -> Result<()> {
        let point = CGPoint::new(x as f64, y as f64);
        let boxed = unsafe {
            AXValueCreate(
                K_AX_VALUE_CG_POINT_TYPE,
                &point as *const CGPoint as *const c_void,
            )
        };
        if boxed.is_null() {
            return Err(MacosError::Accessibility(-1));
        }
        let key = CFString::new(K_AX_POSITION_ATTRIBUTE);
        let code =
            unsafe { AXUIElementSetAttributeValue(self.0, key.as_concrete_TypeRef(), boxed as _) };
        unsafe { CFRelease(boxed as CFTypeRef) };
        ax_result(code)
    }

    /// Resizes the element to `(width, height)`.
    pub fn set_size(&self, width: i32, height: i32) -> Result<()> {
        let size = CGSize::new(width as f64, height as f64);
        let boxed = unsafe {
            AXValueCreate(
                K_AX_VALUE_CG_SIZE_TYPE,
                &size as *const CGSize as *const c_void,
            )
        };
        if boxed.is_null() {
            return Err(MacosError::Accessibility(-1));
        }
        let key = CFString::new(K_AX_SIZE_ATTRIBUTE);
        let code =
            unsafe { AXUIElementSetAttributeValue(self.0, key.as_concrete_TypeRef(), boxed as _) };
        unsafe { CFRelease(boxed as CFTypeRef) };
        ax_result(code)
    }

    /// Sets a boolean attribute such as `AXMinimized` or `AXFullScreen`.
    pub fn set_bool_attribute(&self, attribute: &str, value: bool) -> Result<()> {
        let key = CFString::new(attribute);
        let boolean = CFBoolean::from(value);
        let code = unsafe {
            AXUIElementSetAttributeValue(self.0, key.as_concrete_TypeRef(), boolean.as_CFTypeRef())
        };
        ax_result(code)
    }

    /// The application's windows, as owned elements.
    pub fn windows(&self) -> Vec<Element> {
        let Some(value) = self.copy_attribute(K_AX_WINDOWS_ATTRIBUTE) else {
            return Vec::new();
        };
        let array: CFArray<*const c_void> = unsafe { CFArray::wrap_under_create_rule(value as _) };
        array
            .iter()
            .map(|window| {
                // The array owns its entries, so each one is retained
                // before it is adopted by an owning `Element`.
                let element = *window;
                unsafe { core_foundation::base::CFRetain(element as CFTypeRef) };
                unsafe { Element::from_owned(element) }
            })
            .collect()
    }
}

/// An app-scoped Accessibility element.
#[derive(Debug)]
pub struct ApplicationElement(Element);

impl ApplicationElement {
    pub fn for_process(pid: u32) -> Result<Self> {
        let element = unsafe { AXUIElementCreateApplication(pid as i32) };
        if element.is_null() {
            Err(MacosError::AccessibilityPermissionDenied)
        } else {
            Ok(Self(unsafe { Element::from_owned(element) }))
        }
    }

    pub fn as_ptr(&self) -> AXUIElementRef {
        self.0.as_ptr()
    }

    /// The application's top-level windows.
    pub fn windows(&self) -> Vec<Element> {
        self.0.windows()
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
