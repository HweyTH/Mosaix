//! Global hotkey registration and availability probing.
//!
//! macOS registers process-wide hotkeys through Carbon's
//! `RegisterEventHotKey`. It is an old interface, but it is the public,
//! documented, still-supported one -- there is no AppKit or Core
//! Graphics replacement for a system-wide hotkey that does not require an
//! event tap, and an event tap would need Input Monitoring permission for
//! something Mosaix can do without it.

use std::sync::mpsc::{self, Receiver};

use crate::{MacosError, Result};

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

/// Whether a combination can be registered right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HotkeyAvailability {
    /// `RegisterEventHotKey` accepted it, and the registration was
    /// released again immediately.
    Available,
    /// `RegisterEventHotKey` refused it. Something already owns it.
    Taken,
}

/// Carbon modifier masks, from `Events.h`. These are not the same bits
/// AppKit or Core Graphics use, and only these are valid here.
pub const CMD_KEY: u32 = 0x0100;
pub const SHIFT_KEY: u32 = 0x0200;
pub const OPTION_KEY: u32 = 0x0800;
pub const CONTROL_KEY: u32 = 0x1000;

type OSStatus = i32;
type EventTargetRef = *mut std::ffi::c_void;
type EventHotKeyRef = *mut std::ffi::c_void;

#[repr(C)]
#[derive(Clone, Copy)]
struct EventHotKeyID {
    signature: u32,
    id: u32,
}

/// Four-character code identifying Mosaix's hotkeys, as Carbon expects.
const MOSAIX_SIGNATURE: u32 = u32::from_be_bytes(*b"MSX1");
/// The id the availability probe registers under. It is released before
/// the probe returns, so it never collides with a real binding.
const PROBE_HOTKEY_ID: u32 = u32::MAX;
const NO_ERR: OSStatus = 0;

#[link(name = "Carbon", kind = "framework")]
extern "C" {
    fn GetApplicationEventTarget() -> EventTargetRef;
    fn RegisterEventHotKey(
        key_code: u32,
        modifiers: u32,
        hotkey_id: EventHotKeyID,
        target: EventTargetRef,
        options: u32,
        out_ref: *mut EventHotKeyRef,
    ) -> OSStatus;
    fn UnregisterEventHotKey(hotkey: EventHotKeyRef) -> OSStatus;
}

/// The Carbon modifier mask for a set of modifier flags.
///
/// The `command` flag is what the configuration calls `win`: it is the
/// same "platform" modifier in each key combination, and mapping it to
/// Control instead would silently rebind every default.
pub const fn modifier_mask(ctrl: bool, alt: bool, shift: bool, command: bool) -> u32 {
    let mut bits = 0;
    if ctrl {
        bits |= CONTROL_KEY;
    }
    if alt {
        bits |= OPTION_KEY;
    }
    if shift {
        bits |= SHIFT_KEY;
    }
    if command {
        bits |= CMD_KEY;
    }
    bits
}

/// The Carbon virtual key code for `key`, a canonicalized (uppercase)
/// key name.
///
/// Carbon key codes are positional, not alphabetical: they name a
/// physical key on the ANSI layout, so they cannot be derived from the
/// character the way Windows virtual-key codes can for letters and
/// digits. The table is therefore explicit.
///
/// A name with no code behind it returns `None`, which callers report as
/// unsupported rather than as taken.
pub fn key_code_for(key: &str) -> Option<u32> {
    let code: u32 = match key {
        "A" => 0x00,
        "S" => 0x01,
        "D" => 0x02,
        "F" => 0x03,
        "H" => 0x04,
        "G" => 0x05,
        "Z" => 0x06,
        "X" => 0x07,
        "C" => 0x08,
        "V" => 0x09,
        "B" => 0x0B,
        "Q" => 0x0C,
        "W" => 0x0D,
        "E" => 0x0E,
        "R" => 0x0F,
        "Y" => 0x10,
        "T" => 0x11,
        "O" => 0x1F,
        "U" => 0x20,
        "I" => 0x22,
        "P" => 0x23,
        "L" => 0x25,
        "J" => 0x26,
        "K" => 0x28,
        "N" => 0x2D,
        "M" => 0x2E,
        "1" => 0x12,
        "2" => 0x13,
        "3" => 0x14,
        "4" => 0x15,
        "5" => 0x17,
        "6" => 0x16,
        "7" => 0x1A,
        "8" => 0x1C,
        "9" => 0x19,
        "0" => 0x1D,
        "ENTER" | "RETURN" => 0x24,
        "TAB" => 0x30,
        "SPACE" => 0x31,
        "BACKSPACE" => 0x33,
        "ESC" | "ESCAPE" => 0x35,
        "HOME" => 0x73,
        "PAGEUP" => 0x74,
        "DELETE" | "DEL" => 0x75,
        "END" => 0x77,
        "PAGEDOWN" => 0x79,
        "LEFT" => 0x7B,
        "RIGHT" => 0x7C,
        "DOWN" => 0x7D,
        "UP" => 0x7E,
        // macOS keyboards have no Insert key, so a binding naming one has
        // no code here rather than a substitute that would fire on
        // something else.
        _ => return function_key_code(key),
    };
    Some(code)
}

/// The code for `F1`-`F20`, which are also positional and out of order.
fn function_key_code(key: &str) -> Option<u32> {
    let number: u32 = key.strip_prefix('F')?.parse().ok()?;
    let code = match number {
        1 => 0x7A,
        2 => 0x78,
        3 => 0x63,
        4 => 0x76,
        5 => 0x60,
        6 => 0x61,
        7 => 0x62,
        8 => 0x64,
        9 => 0x65,
        10 => 0x6D,
        11 => 0x67,
        12 => 0x6F,
        13 => 0x69,
        14 => 0x6B,
        15 => 0x71,
        16 => 0x6A,
        17 => 0x40,
        18 => 0x4F,
        19 => 0x50,
        20 => 0x5A,
        _ => return None,
    };
    Some(code)
}

/// Whether `modifiers` + `key_code` can be registered right now.
///
/// The probe registers the combination and releases it again, which is
/// the only way to ask macOS the question: there is no "is this taken"
/// query. Registration is process-wide and momentary, so a combination
/// that probes as available is one that can actually be registered.
pub fn probe_hotkey(modifiers: u32, key_code: u32) -> HotkeyAvailability {
    let mut registered: EventHotKeyRef = std::ptr::null_mut();
    let status = unsafe {
        RegisterEventHotKey(
            key_code,
            modifiers,
            EventHotKeyID {
                signature: MOSAIX_SIGNATURE,
                id: PROBE_HOTKEY_ID,
            },
            GetApplicationEventTarget(),
            0,
            &mut registered,
        )
    };
    if status == NO_ERR && !registered.is_null() {
        unsafe { UnregisterEventHotKey(registered) };
        HotkeyAvailability::Available
    } else {
        tracing::debug!(
            status,
            "combination refused by RegisterEventHotKey; reporting it as taken"
        );
        HotkeyAvailability::Taken
    }
}

/// Registers bindings independently, preserving the platform adapter's
/// partial-success contract even when a user has a conflicting system hotkey.
pub fn start_hotkeys(
    bindings: Vec<HotkeyBinding>,
) -> Result<(HotkeyRegistrations, Receiver<HotkeyFired>)> {
    let (_tx, rx) = mpsc::channel();
    let target = unsafe { GetApplicationEventTarget() };
    let results = bindings
        .into_iter()
        .map(|binding| {
            let mut registered: EventHotKeyRef = std::ptr::null_mut();
            let status = unsafe {
                RegisterEventHotKey(
                    binding.key_code,
                    binding.modifiers,
                    EventHotKeyID {
                        signature: MOSAIX_SIGNATURE,
                        id: binding.id,
                    },
                    target,
                    0,
                    &mut registered,
                )
            };
            HotkeyRegistrationResult {
                id: binding.id,
                outcome: if status == NO_ERR {
                    Ok(())
                } else {
                    Err(MacosError::Accessibility(status))
                },
            }
        })
        .collect();
    Ok((HotkeyRegistrations { results }, rx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_platform_modifier_maps_to_command_not_control() {
        assert_eq!(modifier_mask(false, false, false, true), CMD_KEY);
        assert_eq!(modifier_mask(true, false, false, false), CONTROL_KEY);
        assert_eq!(
            modifier_mask(true, true, true, true),
            CONTROL_KEY | OPTION_KEY | SHIFT_KEY | CMD_KEY
        );
    }

    #[test]
    fn letters_use_positional_codes_rather_than_their_characters() {
        // The classic trap: on Carbon, 'A' is 0 and 'B' is not 1.
        assert_eq!(key_code_for("A"), Some(0x00));
        assert_eq!(key_code_for("B"), Some(0x0B));
        assert_ne!(key_code_for("Z"), Some(b'Z' as u32));
    }

    #[test]
    fn function_keys_are_not_consecutive() {
        assert_eq!(key_code_for("F1"), Some(0x7A));
        assert_eq!(key_code_for("F2"), Some(0x78));
        assert_eq!(key_code_for("F20"), Some(0x5A));
        assert_eq!(key_code_for("F21"), None, "macOS stops at F20");
    }

    #[test]
    fn every_key_code_is_distinct() {
        let names = [
            "A",
            "S",
            "D",
            "F",
            "H",
            "G",
            "Z",
            "X",
            "C",
            "V",
            "B",
            "Q",
            "W",
            "E",
            "R",
            "Y",
            "T",
            "O",
            "U",
            "I",
            "P",
            "L",
            "J",
            "K",
            "N",
            "M",
            "1",
            "2",
            "3",
            "4",
            "5",
            "6",
            "7",
            "8",
            "9",
            "0",
            "ENTER",
            "TAB",
            "SPACE",
            "BACKSPACE",
            "ESC",
            "HOME",
            "PAGEUP",
            "DELETE",
            "END",
            "PAGEDOWN",
            "LEFT",
            "RIGHT",
            "DOWN",
            "UP",
            "F1",
            "F2",
            "F3",
            "F4",
            "F5",
            "F6",
            "F7",
            "F8",
            "F9",
            "F10",
            "F11",
            "F12",
        ];
        let mut codes: Vec<u32> = names
            .iter()
            .map(|name| key_code_for(name).unwrap_or_else(|| panic!("{name} has a code")))
            .collect();
        let total = codes.len();
        codes.sort_unstable();
        codes.dedup();
        assert_eq!(codes.len(), total, "two key names share a code");
    }

    #[test]
    fn a_key_macos_does_not_have_is_unsupported_rather_than_substituted() {
        assert_eq!(key_code_for("INSERT"), None);
        assert_eq!(key_code_for("SCROLLLOCK"), None);
    }

    #[test]
    fn an_ordinary_combination_probes_as_available_on_this_machine() {
        // Control-Option-Shift-F19 is not a combination any shipping macOS
        // feature claims, so on a real host it must register and release.
        let verdict = probe_hotkey(
            modifier_mask(true, true, true, false),
            key_code_for("F19").unwrap(),
        );

        assert_eq!(verdict, HotkeyAvailability::Available);
    }
}
