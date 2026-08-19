//! Windows platform adapter: Win32 window management, event hooks, and DPI handling.

#[cfg(windows)]
pub mod adapter;
#[cfg(windows)]
pub mod enumeration;
#[cfg(windows)]
pub mod win32_helpers;

#[cfg(windows)]
pub use adapter::WindowsPlatformAdapter;
