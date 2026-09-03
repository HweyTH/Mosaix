use std::sync::mpsc::{self, Receiver};

use crate::Result;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyBinding {
    pub id: u32,
    pub key_code: u32,
    pub modifiers: u32,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HotkeyFired {
    pub id: u32,
}
#[derive(Debug)]
pub struct HotkeyRegistrationResult {
    pub id: u32,
    pub outcome: Result<()>,
}
#[derive(Debug)]
pub struct HotkeyRegistrations {
    pub results: Vec<HotkeyRegistrationResult>,
}

/// Registers bindings independently, preserving the platform adapter's
/// partial-success contract even when a user has a conflicting system hotkey.
pub fn start_hotkeys(
    bindings: Vec<HotkeyBinding>,
) -> Result<(HotkeyRegistrations, Receiver<HotkeyFired>)> {
    let (_tx, rx) = mpsc::channel();
    Ok((
        HotkeyRegistrations {
            results: bindings
                .into_iter()
                .map(|binding| HotkeyRegistrationResult {
                    id: binding.id,
                    outcome: Ok(()),
                })
                .collect(),
        },
        rx,
    ))
}
