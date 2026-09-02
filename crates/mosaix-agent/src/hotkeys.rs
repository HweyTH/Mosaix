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

use mosaix_config::{Command, KeyCombo, ResolvedConfig};
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
        Command::Rearrange => unreachable!("rearrange is not a zone-snap command"),
        Command::ToggleAutomaticTiling => {
            unreachable!("tiling toggle is not a zone-snap command")
        }
        Command::ToggleFloating => unreachable!("floating toggle is not a zone-snap command"),
        Command::FocusLeft
        | Command::FocusRight
        | Command::FocusUp
        | Command::FocusDown
        | Command::SwapLeft
        | Command::SwapRight
        | Command::SwapUp
        | Command::SwapDown
        | Command::TogglePause => unreachable!("tiling command is not a zone-snap command"),
    }
}

/// The `RegisterHotKey` ids allocated for one registration pass, mapped
/// back to the commands they fire.
///
/// Ids used to come from a static command-to-integer match and its hand-
/// written inverse. That bijection cannot survive a command that carries a
/// layout name, because layout names are user-created and unbounded (ADR
/// 0019), so ids are now allocated as bindings are built and the reverse
/// lookup reads this registry.
///
/// A registry belongs to exactly one registration. Re-registering --
/// which is what a profile switch or a config reload already does --
/// builds a fresh one, so a stale id can never resolve to a command that
/// is no longer bound.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HotkeyRegistry {
    commands: BTreeMap<i32, Command>,
}

impl HotkeyRegistry {
    /// The command hotkey `id` belongs to, `None` if this registry never
    /// allocated `id` or the binding that owned it failed to register.
    /// Callers log the `None` case: it means a `WM_HOTKEY` arrived for a
    /// binding this registration doesn't own.
    pub fn command_for(&self, id: i32) -> Option<Command> {
        self.commands.get(&id).copied()
    }

    /// Drops `id`'s entry, so a binding the OS refused holds none and its
    /// id resolves to nothing rather than to the command it would have
    /// fired.
    pub fn forget(&mut self, id: i32) -> Option<Command> {
        self.commands.remove(&id)
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
/// bindings [`mosaix_platform_windows::start_hotkeys`] should register,
/// paired with the [`HotkeyRegistry`] resolving the ids it allocated. A
/// command whose key name doesn't translate to a known virtual-key code is
/// skipped (and logged) rather than failing the whole set -- the same
/// partial-success posture ADR 0002 already applies to OS-level
/// registration conflicts -- and consumes no id.
pub fn bindings_from_resolved(
    hotkeys: &BTreeMap<Command, KeyCombo>,
) -> (Vec<HotkeyBinding>, HotkeyRegistry) {
    let mut bindings = Vec::new();
    let mut registry = HotkeyRegistry::default();
    for (command, combo) in hotkeys {
        let Some(vk) = vk_from_key_name(&combo.key) else {
            tracing::error!(
                ?command,
                key = %combo.key,
                "resolved hotkey binding names an unrecognized key; this command will have no hotkey"
            );
            continue;
        };
        // Ids start at 1 because `RegisterHotKey` treats 0 as a valid but
        // unremarkable id, and a zero default is exactly the value a bug
        // elsewhere would produce.
        let id = bindings.len() as i32 + 1;
        bindings.push(HotkeyBinding {
            id,
            modifiers: modifiers_from_combo(combo),
            vk,
        });
        registry.commands.insert(id, *command);
    }
    (bindings, registry)
}

pub fn runtime_hotkeys(config: &ResolvedConfig) -> BTreeMap<Command, KeyCombo> {
    config
        .hotkeys
        .iter()
        .filter(|(command, _)| {
            config.automatic_tiling_enabled
                || matches!(
                    command,
                    Command::SnapLeft
                        | Command::SnapRight
                        | Command::SnapTop
                        | Command::SnapBottom
                        | Command::TogglePause
                )
        })
        .map(|(command, combo)| (*command, combo.clone()))
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

        let (mut bindings, registry) = bindings_from_resolved(&hotkeys);
        bindings.sort_by_key(|binding| binding.id);

        assert_eq!(bindings.len(), 4);
        assert_eq!(
            registry.command_for(bindings[0].id),
            Some(Command::SnapLeft)
        );
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

        let (mut bindings, _registry) = bindings_from_resolved(&hotkeys);
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

        let (bindings, registry) = bindings_from_resolved(&hotkeys);

        assert_eq!(bindings.len(), 1);
        assert_eq!(
            registry.command_for(bindings[0].id),
            Some(Command::SnapLeft)
        );
    }

    fn every_command() -> [Command; 16] {
        [
            Command::SnapLeft,
            Command::SnapRight,
            Command::SnapTop,
            Command::SnapBottom,
            Command::Rearrange,
            Command::ToggleAutomaticTiling,
            Command::ToggleFloating,
            Command::FocusLeft,
            Command::FocusDown,
            Command::FocusUp,
            Command::FocusRight,
            Command::SwapLeft,
            Command::SwapDown,
            Command::SwapUp,
            Command::SwapRight,
            Command::TogglePause,
        ]
    }

    #[test]
    fn every_registered_binding_resolves_back_to_its_own_command() {
        let hotkeys: BTreeMap<Command, KeyCombo> = every_command()
            .into_iter()
            .enumerate()
            .map(|(index, command)| (command, combo(&format!("ctrl+alt+f{}", index + 1))))
            .collect();

        let (bindings, registry) = bindings_from_resolved(&hotkeys);

        assert_eq!(bindings.len(), every_command().len());
        for (binding, (command, _)) in bindings.iter().zip(&hotkeys) {
            assert_eq!(registry.command_for(binding.id), Some(*command));
        }
    }

    #[test]
    fn an_id_outside_the_registry_resolves_to_nothing() {
        let hotkeys = BTreeMap::from([(Command::SnapLeft, combo("ctrl+alt+left"))]);

        let (bindings, registry) = bindings_from_resolved(&hotkeys);

        assert_eq!(registry.command_for(bindings[0].id + 1), None);
        assert_eq!(registry.command_for(0), None);
    }

    #[test]
    fn a_forgotten_binding_no_longer_resolves() {
        let hotkeys = BTreeMap::from([
            (Command::SnapLeft, combo("ctrl+alt+left")),
            (Command::SnapRight, combo("ctrl+alt+right")),
        ]);

        let (bindings, mut registry) = bindings_from_resolved(&hotkeys);
        assert_eq!(registry.forget(bindings[0].id), Some(Command::SnapLeft));

        assert_eq!(registry.command_for(bindings[0].id), None);
        assert_eq!(
            registry.command_for(bindings[1].id),
            Some(Command::SnapRight)
        );
    }

    #[test]
    fn re_registering_a_smaller_binding_set_leaves_no_stale_id() {
        let (_, before) = bindings_from_resolved(&BTreeMap::from([
            (Command::SnapLeft, combo("ctrl+alt+left")),
            (Command::SnapRight, combo("ctrl+alt+right")),
        ]));
        let (_, after) =
            bindings_from_resolved(&BTreeMap::from([(Command::SnapTop, combo("ctrl+alt+up"))]));

        assert_eq!(before.command_for(2), Some(Command::SnapRight));
        // The id the profile switch dropped resolves to nothing rather
        // than to the command the previous registration bound it to.
        assert_eq!(after.command_for(2), None);
        assert_eq!(after.command_for(1), Some(Command::SnapTop));
    }

    #[test]
    fn manual_topology_does_not_register_automatic_tiling_commands() {
        let mut config = mosaix_config::fallback_config();
        let hotkeys = runtime_hotkeys(&config);

        assert!(hotkeys.contains_key(&Command::SnapLeft));
        assert!(hotkeys.contains_key(&Command::TogglePause));
        assert!(!hotkeys.contains_key(&Command::FocusLeft));
        assert!(!hotkeys.contains_key(&Command::ToggleAutomaticTiling));

        config.automatic_tiling_enabled = true;
        assert!(runtime_hotkeys(&config).contains_key(&Command::FocusLeft));
    }
}
