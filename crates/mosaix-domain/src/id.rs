//! Identity types used throughout the domain model.
//!
//! These are thin newtypes that keep native handles from leaking into
//! platform-independent code. Native handles are ephemeral and must never
//! be persisted as stable identities.

use serde::{Deserialize, Serialize};

/// Ephemeral window identity wrapping a native handle.
///
/// On Windows this holds an `HWND` cast to `isize`.
/// On macOS it would hold a `CGWindowID` or `AXUIElement` token.
///
/// Do **not** persist across sessions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WindowId(pub isize);

/// Display / monitor identity.
///
/// Populated from an `HMONITOR` (cast to `isize`, matching `WindowId`'s
/// treatment of `HWND` -- both are pointer-sized opaque handles, so a
/// narrower integer type would risk truncating two distinct handles onto
/// the same ID) on Windows, or `CGDirectDisplayID` on macOS.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct DisplayId(pub isize);

/// Application identity derived from the executable path or bundle ID.
///
/// On Windows: the executable filename (e.g. `"Code.exe"`).
/// On macOS: the bundle identifier (e.g. `"com.microsoft.VSCode"`).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ApplicationId(pub String);
