//! Default hotkey -> zone-snap-command table (ADR 0003).
//!
//! `mosaix-config` has no schema or loading code yet, so bindings are a
//! fixed table owned by this binary rather than anything user-configurable
//! (deferred until `mosaix-config` exists). Uses Ctrl+Alt+Arrow, which
//! avoids the OS-reserved combos called out in
//! `docs/research/cycle-and-hotkeys.md` (bare Win-key chords, Win+L,
//! Ctrl+Alt+Del, F12) and gives each command a distinct combination.

use mosaix_engine::ZoneSnapDirection;
use mosaix_platform_windows::HotkeyBinding;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, VK_DOWN, VK_LEFT, VK_RIGHT, VK_UP,
};

/// One default hotkey: the binding to register and the zone-snap direction
/// it dispatches to when it fires.
#[derive(Debug, Clone, Copy)]
pub struct DefaultBinding {
    pub binding: HotkeyBinding,
    pub direction: ZoneSnapDirection,
}

fn ctrl_alt() -> HOT_KEY_MODIFIERS {
    HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0)
}

/// The default command -> keybinding table: snap-left/right/top/bottom on
/// Ctrl+Alt+Arrow. Each [`HotkeyBinding::id`] is unique and stable -- it's
/// how a `HotkeyFired` firing is matched back to its direction in
/// [`direction_for_id`].
pub fn default_bindings() -> Vec<DefaultBinding> {
    vec![
        DefaultBinding {
            binding: HotkeyBinding {
                id: 1,
                modifiers: ctrl_alt(),
                vk: VK_LEFT.0 as u32,
            },
            direction: ZoneSnapDirection::Left,
        },
        DefaultBinding {
            binding: HotkeyBinding {
                id: 2,
                modifiers: ctrl_alt(),
                vk: VK_RIGHT.0 as u32,
            },
            direction: ZoneSnapDirection::Right,
        },
        DefaultBinding {
            binding: HotkeyBinding {
                id: 3,
                modifiers: ctrl_alt(),
                vk: VK_UP.0 as u32,
            },
            direction: ZoneSnapDirection::Top,
        },
        DefaultBinding {
            binding: HotkeyBinding {
                id: 4,
                modifiers: ctrl_alt(),
                vk: VK_DOWN.0 as u32,
            },
            direction: ZoneSnapDirection::Bottom,
        },
    ]
}

/// The zone-snap direction bound to hotkey `id` within `bindings`. `None`
/// if `id` doesn't belong to the table.
pub fn direction_for_id(bindings: &[DefaultBinding], id: i32) -> Option<ZoneSnapDirection> {
    bindings
        .iter()
        .find(|entry| entry.binding.id == id)
        .map(|entry| entry.direction)
}
