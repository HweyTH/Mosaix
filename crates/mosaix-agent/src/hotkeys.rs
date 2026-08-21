//! Translating `mosaix-config`'s platform-neutral resolved hotkey bindings
//! into `mosaix-platform-windows::HotkeyBinding`s to register, and mapping
//! a fired hotkey id back to the zone-snap command it represents.
//!
//! Replaces this crate's former hardcoded `keybindings.rs` table (ADR
//! 0003, superseded by ADR 0005): the resolved bindings themselves now
//! come from `EngineState::resolved_config.hotkeys`, hot-editable and
//! per-profile. Only the platform-specific pieces stay here -- key-name ->
//! virtual-key translation and modifier-flag assembly -- since
//! `mosaix-config` is deliberately platform-neutral (its `KeyCombo` type
//! doc comment).

use std::collections::BTreeMap;

use mosaix_config::{Command, KeyCombo};
use mosaix_engine::ZoneSnapDirection;
use mosaix_platform_windows::HotkeyBinding;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VK_BACK, VK_DELETE, VK_DOWN,
    VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT,
    VK_SPACE, VK_TAB, VK_UP,
};

/// The zone-snap direction `command` dispatches to when its hotkey fires --
/// a 1:1 mirror of [`Command`]'s four variants (ADR 0005).
pub fn direction_for_command(command: Command) -> ZoneSnapDirection {
    match command {
        Command::SnapLeft => ZoneSnapDirection::Left,
        Command::SnapRight => ZoneSnapDirection::Right,
        Command::SnapTop => ZoneSnapDirection::Top,
        Command::SnapBottom => ZoneSnapDirection::Bottom,
    }
}

/// A stable `RegisterHotKey` id for `command`. Fixed per command (not
/// assigned by position in the resolved map) so a re-registration after a
/// rebind keeps mapping the same command to the same id even though the
/// set of bound commands, and the order `BTreeMap` iterates them in, can
/// change across a hot-edit or profile switch.
fn hotkey_id(command: Command) -> i32 {
    match command {
        Command::SnapLeft => 1,
        Command::SnapRight => 2,
        Command::SnapTop => 3,
        Command::SnapBottom => 4,
    }
}

/// The command a fired hotkey `id` belongs to, `None` if `id` doesn't
/// belong to this table. Every id [`bindings_from_resolved`] registers
/// comes from [`hotkey_id`], so in practice this only returns `None` for a
/// stray `WM_HOTKEY` this process didn't itself register.
pub fn command_for_hotkey_id(id: i32) -> Option<Command> {
    match id {
        1 => Some(Command::SnapLeft),
        2 => Some(Command::SnapRight),
        3 => Some(Command::SnapTop),
        4 => Some(Command::SnapBottom),
        _ => None,
    }
}

fn modifiers_from_combo(combo: &KeyCombo) -> HOT_KEY_MODIFIERS {
    let mut bits = 0u32;
    if combo.ctrl {
        bits |= MOD_CONTROL.0;
    }
    if combo.alt {
        bits |= MOD_ALT.0;
    }
    if combo.shift {
        bits |= MOD_SHIFT.0;
    }
    if combo.win {
        bits |= MOD_WIN.0;
    }
    HOT_KEY_MODIFIERS(bits)
}

/// The virtual-key code for `key`, a [`KeyCombo::key`] name (already
/// canonicalized to uppercase by [`KeyCombo::parse`]). Covers the arrow
/// keys, single letters/digits, function keys F1-F24, and the handful of
/// named keys common to window-manager hotkeys. Any other name (an
/// unrecognized name or a typo) returns `None`.
fn vk_from_key_name(key: &str) -> Option<u32> {
    let named = match key {
        "LEFT" => Some(VK_LEFT.0),
        "RIGHT" => Some(VK_RIGHT.0),
        "UP" => Some(VK_UP.0),
        "DOWN" => Some(VK_DOWN.0),
        "SPACE" => Some(VK_SPACE.0),
        "TAB" => Some(VK_TAB.0),
        "ENTER" | "RETURN" => Some(VK_RETURN.0),
        "ESC" | "ESCAPE" => Some(VK_ESCAPE.0),
        "HOME" => Some(VK_HOME.0),
        "END" => Some(VK_END.0),
        "PAGEUP" => Some(VK_PRIOR.0),
        "PAGEDOWN" => Some(VK_NEXT.0),
        "INSERT" => Some(VK_INSERT.0),
        "DELETE" | "DEL" => Some(VK_DELETE.0),
        "BACKSPACE" => Some(VK_BACK.0),
        _ => None,
    };
    if let Some(vk) = named {
        return Some(vk as u32);
    }

    let bytes = key.as_bytes();
    if bytes.len() == 1 && (bytes[0].is_ascii_uppercase() || bytes[0].is_ascii_digit()) {
        return Some(bytes[0] as u32);
    }

    if let Some(rest) = key.strip_prefix('F') {
        if let Ok(number) = rest.parse::<u32>() {
            if (1..=24).contains(&number) {
                return Some(VK_F1.0 as u32 + (number - 1));
            }
        }
    }

    None
}

/// Translates a resolved config's hotkey bindings into the platform
/// bindings [`mosaix_platform_windows::start_hotkeys`] should register. A
/// command whose key name doesn't translate to a known virtual-key code is
/// skipped (and logged) rather than failing the whole set -- the same
/// partial-success posture ADR 0002 already applies to OS-level
/// registration conflicts.
pub fn bindings_from_resolved(hotkeys: &BTreeMap<Command, KeyCombo>) -> Vec<HotkeyBinding> {
    hotkeys
        .iter()
        .filter_map(|(command, combo)| match vk_from_key_name(&combo.key) {
            Some(vk) => Some(HotkeyBinding {
                id: hotkey_id(*command),
                modifiers: modifiers_from_combo(combo),
                vk,
            }),
            None => {
                tracing::error!(
                    ?command,
                    key = %combo.key,
                    "resolved hotkey binding names an unrecognized key; this command will have no hotkey"
                );
                None
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(raw: &str) -> KeyCombo {
        KeyCombo::parse(raw).expect("fixture combo should parse")
    }

    #[test]
    fn bindings_from_resolved_translates_the_default_ctrl_alt_arrow_table() {
        let hotkeys = BTreeMap::from([
            (Command::SnapLeft, combo("ctrl+alt+left")),
            (Command::SnapRight, combo("ctrl+alt+right")),
            (Command::SnapTop, combo("ctrl+alt+up")),
            (Command::SnapBottom, combo("ctrl+alt+down")),
        ]);

        let mut bindings = bindings_from_resolved(&hotkeys);
        bindings.sort_by_key(|binding| binding.id);

        assert_eq!(bindings.len(), 4);
        assert_eq!(bindings[0].id, hotkey_id(Command::SnapLeft));
        assert_eq!(bindings[0].vk, VK_LEFT.0 as u32);
        assert_eq!(
            bindings[0].modifiers,
            HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_ALT.0)
        );
    }

    #[test]
    fn bindings_from_resolved_translates_a_letter_and_a_function_key() {
        let hotkeys = BTreeMap::from([
            (Command::SnapLeft, combo("ctrl+shift+a")),
            (Command::SnapRight, combo("win+f5")),
        ]);

        let mut bindings = bindings_from_resolved(&hotkeys);
        bindings.sort_by_key(|binding| binding.id);

        assert_eq!(bindings[0].vk, b'A' as u32);
        assert_eq!(
            bindings[0].modifiers,
            HOT_KEY_MODIFIERS(MOD_CONTROL.0 | MOD_SHIFT.0)
        );
        assert_eq!(bindings[1].vk, VK_F1.0 as u32 + 4);
        assert_eq!(bindings[1].modifiers, HOT_KEY_MODIFIERS(MOD_WIN.0));
    }

    #[test]
    fn bindings_from_resolved_skips_an_unrecognized_key_name_without_dropping_the_rest() {
        let hotkeys = BTreeMap::from([
            (Command::SnapLeft, combo("ctrl+alt+left")),
            (Command::SnapRight, combo("ctrl+alt+nonsense")),
        ]);

        let bindings = bindings_from_resolved(&hotkeys);

        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].id, hotkey_id(Command::SnapLeft));
    }

    #[test]
    fn hotkey_id_and_command_for_hotkey_id_round_trip_for_every_command() {
        for command in [
            Command::SnapLeft,
            Command::SnapRight,
            Command::SnapTop,
            Command::SnapBottom,
        ] {
            assert_eq!(command_for_hotkey_id(hotkey_id(command)), Some(command));
        }
    }
}
