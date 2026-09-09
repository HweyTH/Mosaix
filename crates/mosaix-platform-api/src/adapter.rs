//! Platform adapter trait defining the contract for OS-specific implementations.
//!
//! Every platform (Windows, macOS) provides a concrete type that
//! implements `PlatformAdapter`. The rest of the application depends only
//! on this trait, keeping platform-specific code behind the adapter
//! boundary.

use mosaix_domain::Window;

/// The primary trait that each platform adapter must implement.
///
/// Additional methods (`enumerate_displays`, `focused_window`, `subscribe`,
/// `apply`) will be added as those features are built.
pub trait PlatformAdapter {
    /// Enumerate all top-level windows that are candidates for management.
    ///
    /// The adapter is responsible for filtering out windows that should never
    /// be managed: hidden, cloaked, tool windows, popups, child windows,
    /// known system surfaces, and zero-size windows.
    ///
    /// # Errors
    ///
    /// Returns an error if the underlying OS enumeration call fails.
    fn enumerate_windows(&self) -> anyhow::Result<Vec<Window>>;
}
