//! Translating `mosaix-config`'s platform-neutral resolved hotkey bindings
//! into `mosaix-platform-windows::HotkeyBinding`s to register, mapping a
//! fired hotkey id back to the command it represents, and turning that
//! command into the engine event it asks for.
//!
//! Bindings come from `EngineState::resolved_config.hotkeys`, so they are
//! hot-editable and per-profile. Only the platform-specific pieces stay
//! here -- key-name -> virtual-key translation and modifier-flag assembly
//! -- since `mosaix-config` is deliberately platform-neutral.

use std::collections::BTreeMap;

use mosaix_config::{Command, KeyCombo, ResolvedConfig};
use mosaix_engine::{CardinalDirection, Event, ZoneSnapDirection};
use mosaix_platform_windows::HotkeyBinding;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    HOT_KEY_MODIFIERS, MOD_ALT, MOD_CONTROL, MOD_SHIFT, MOD_WIN, VK_BACK, VK_DELETE, VK_DOWN,
    VK_END, VK_ESCAPE, VK_F1, VK_HOME, VK_INSERT, VK_LEFT, VK_NEXT, VK_PRIOR, VK_RETURN, VK_RIGHT,
    VK_SPACE, VK_TAB, VK_UP,
};

/// Whether `command` is one of the four zone snaps -- the only commands
/// the snap-preview overlay has a single committed rectangle to flash.
///
/// Applying a saved layout is excluded deliberately: it commits a whole
/// display's worth of rectangles at once, which is not the shape that
/// overlay draws.
pub fn is_zone_snap(command: &Command) -> bool {
    matches!(
        command,
        Command::SnapLeft | Command::SnapRight | Command::SnapTop | Command::SnapBottom
    )
}

/// The engine event a fired hotkey turns into.
///
/// `paused` comes from the caller's snapshot, because `toggle-pause` is
/// the one command whose meaning depends on current state rather than on
/// the command alone.
///
/// Total over [`Command`] and free of engine or platform handles, so the
/// mapping a keypress goes through is the thing under test rather than a
/// branch buried in a thread that only exists on Windows. Every variant is
/// matched explicitly so it stays that way as the enum grows.
pub fn event_for_command(command: &Command, paused: bool) -> Event {
    match command {
        Command::SnapLeft => Event::ZoneSnapRequested {
            direction: ZoneSnapDirection::Left,
        },
        Command::SnapRight => Event::ZoneSnapRequested {
            direction: ZoneSnapDirection::Right,
        },
        Command::SnapTop => Event::ZoneSnapRequested {
            direction: ZoneSnapDirection::Top,
        },
        Command::SnapBottom => Event::ZoneSnapRequested {
            direction: ZoneSnapDirection::Bottom,
        },
        Command::Rearrange => Event::RearrangeRequested,
        Command::ToggleAutomaticTiling => Event::ToggleAutomaticTilingRequested,
        Command::ToggleFloating => Event::ToggleFloatingRequested,
        Command::FocusLeft => Event::DirectionalFocusRequested {
            direction: CardinalDirection::Left,
        },
        Command::FocusRight => Event::DirectionalFocusRequested {
            direction: CardinalDirection::Right,
        },
        Command::FocusUp => Event::DirectionalFocusRequested {
            direction: CardinalDirection::Up,
        },
        Command::FocusDown => Event::DirectionalFocusRequested {
            direction: CardinalDirection::Down,
        },
        Command::SwapLeft => Event::DirectionalSwapRequested {
            direction: CardinalDirection::Left,
        },
        Command::SwapRight => Event::DirectionalSwapRequested {
            direction: CardinalDirection::Right,
        },
        Command::SwapUp => Event::DirectionalSwapRequested {
            direction: CardinalDirection::Up,
        },
        Command::SwapDown => Event::DirectionalSwapRequested {
            direction: CardinalDirection::Down,
        },
        Command::ResizeLeft => Event::TreeResizeRequested {
            direction: CardinalDirection::Left,
        },
        Command::ResizeRight => Event::TreeResizeRequested {
            direction: CardinalDirection::Right,
        },
        Command::ResizeUp => Event::TreeResizeRequested {
            direction: CardinalDirection::Up,
        },
        Command::ResizeDown => Event::TreeResizeRequested {
            direction: CardinalDirection::Down,
        },
        Command::TogglePause => {
            if paused {
                Event::ResumeRequested
            } else {
                Event::PauseRequested
            }
        }
        // The layout name travels with the command from the registry, so a
        // binding to a user-named layout needs nothing looked up here.
        Command::ApplyLayout { name } => Event::SavedLayoutApplyRequested { name: name.clone() },
        Command::FocusWorkspace { name } => Event::WorkspaceFocusRequested { name: name.clone() },
    }
}

/// The `RegisterHotKey` ids allocated for one registration pass, mapped
/// back to the commands they fire.
///
/// Ids used to come from a static command-to-integer match and its hand-
/// written inverse. That bijection cannot survive a command that carries a
/// layout name, because layout names are user-created and unbounded, so
/// ids are now allocated as bindings are built and the reverse lookup
/// reads this registry.
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
        self.commands.get(&id).cloned()
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

/// The modifier flags and virtual-key code `combo` registers as, or
/// `None` when Mosaix has no virtual-key code for its key name.
///
/// The one translation from a platform-neutral `KeyCombo` to what
/// `RegisterHotKey` takes. Registration and the availability probe both
/// go through it, so a combination the probe reports free is one
/// registration can actually take.
pub fn binding_parts(combo: &KeyCombo) -> Option<(HOT_KEY_MODIFIERS, u32)> {
    vk_from_key_name(&combo.key).map(|vk| (modifiers_from_combo(combo), vk))
}

/// Translates a resolved config's hotkey bindings into the platform
/// bindings [`mosaix_platform_windows::start_hotkeys`] should register,
/// paired with the [`HotkeyRegistry`] resolving the ids it allocated. A
/// command whose key name doesn't translate to a known virtual-key code is
/// skipped (and logged) rather than failing the whole set -- the same
/// partial-success posture that applies to OS-level registration conflicts
/// -- and consumes no id.
pub fn bindings_from_resolved(
    hotkeys: &BTreeMap<Command, KeyCombo>,
) -> (Vec<HotkeyBinding>, HotkeyRegistry) {
    let mut bindings = Vec::new();
    let mut registry = HotkeyRegistry::default();
    for (command, combo) in hotkeys {
        let Some((modifiers, vk)) = binding_parts(combo) else {
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
        bindings.push(HotkeyBinding { id, modifiers, vk });
        registry.commands.insert(id, command.clone());
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
                        | Command::ApplyLayout { .. }
                        | Command::FocusWorkspace { .. }
                )
        })
        .map(|(command, combo)| (command.clone(), combo.clone()))
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

    fn every_command() -> [Command; 20] {
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
            Command::ResizeLeft,
            Command::ResizeRight,
            Command::ResizeUp,
            Command::ResizeDown,
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
            assert_eq!(registry.command_for(binding.id).as_ref(), Some(command));
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

    /// `Event` carries observation batches and rule lists, so it is not
    /// `PartialEq`. Its `Debug` output separates every variant and every
    /// payload these tests turn on, which is enough to assert against.
    fn dispatched(command: Command, paused: bool) -> String {
        format!("{:?}", event_for_command(&command, paused))
    }

    fn rendered(event: Event) -> String {
        format!("{event:?}")
    }

    #[test]
    fn a_layout_hotkey_asks_the_engine_to_apply_that_named_layout() {
        assert_eq!(
            dispatched(
                Command::ApplyLayout {
                    name: "writing".to_owned()
                },
                false
            ),
            rendered(Event::SavedLayoutApplyRequested {
                name: "writing".to_owned()
            }),
            "the layout name must reach the engine, which resolves the \
             focused window's display for it"
        );
    }

    #[test]
    fn two_layout_hotkeys_ask_for_their_own_layouts() {
        assert_ne!(
            dispatched(
                Command::ApplyLayout {
                    name: "writing".to_owned()
                },
                false
            ),
            dispatched(
                Command::ApplyLayout {
                    name: "coding".to_owned()
                },
                false
            )
        );
    }

    #[test]
    fn the_unit_verbs_keep_dispatching_where_they_did() {
        assert_eq!(
            dispatched(Command::SnapLeft, false),
            rendered(Event::ZoneSnapRequested {
                direction: ZoneSnapDirection::Left
            })
        );
        assert_eq!(
            dispatched(Command::SwapDown, false),
            rendered(Event::DirectionalSwapRequested {
                direction: CardinalDirection::Down
            })
        );
        assert_eq!(
            dispatched(Command::TogglePause, false),
            rendered(Event::PauseRequested)
        );
        assert_eq!(
            dispatched(Command::TogglePause, true),
            rendered(Event::ResumeRequested)
        );
    }

    #[test]
    fn only_a_zone_snap_flashes_the_snap_preview_overlay() {
        assert!(is_zone_snap(&Command::SnapTop));
        assert!(!is_zone_snap(&Command::Rearrange));
        assert!(!is_zone_snap(&Command::ApplyLayout {
            name: "writing".to_owned()
        }));
    }

    #[test]
    fn a_layout_binding_gets_an_id_from_the_registry_like_any_other() {
        let hotkeys = BTreeMap::from([
            (Command::SnapLeft, combo("ctrl+alt+left")),
            (
                Command::ApplyLayout {
                    name: "writing".to_owned(),
                },
                combo("ctrl+alt+1"),
            ),
            (
                Command::ApplyLayout {
                    name: "coding".to_owned(),
                },
                combo("ctrl+alt+2"),
            ),
        ]);

        let (bindings, registry) = bindings_from_resolved(&hotkeys);

        assert_eq!(bindings.len(), 3);
        // No static match could produce these: the names are the user's.
        let resolved: Vec<Option<Command>> = bindings
            .iter()
            .map(|binding| registry.command_for(binding.id))
            .collect();
        assert!(resolved.contains(&Some(Command::ApplyLayout {
            name: "writing".to_owned(),
        })));
        assert!(resolved.contains(&Some(Command::ApplyLayout {
            name: "coding".to_owned(),
        })));
    }

    #[test]
    fn re_registering_after_a_reload_repoints_a_layout_id_at_its_new_command() {
        // The same combo, bound to a different layout after the user edits
        // config. The id is reused, and must resolve to the new layout.
        let (_, before) = bindings_from_resolved(&BTreeMap::from([(
            Command::ApplyLayout {
                name: "writing".to_owned(),
            },
            combo("ctrl+alt+1"),
        )]));
        let (bindings, after) = bindings_from_resolved(&BTreeMap::from([(
            Command::ApplyLayout {
                name: "coding".to_owned(),
            },
            combo("ctrl+alt+1"),
        )]));

        assert_eq!(bindings.len(), 1);
        assert_eq!(
            before.command_for(bindings[0].id),
            Some(Command::ApplyLayout {
                name: "writing".to_owned(),
            })
        );
        assert_eq!(
            after.command_for(bindings[0].id),
            Some(Command::ApplyLayout {
                name: "coding".to_owned(),
            })
        );
    }

    #[test]
    fn a_layout_binding_is_registered_whether_or_not_automatic_tiling_is_on() {
        let mut config = mosaix_config::fallback_config();
        let layout = Command::ApplyLayout {
            name: "writing".to_owned(),
        };
        config.hotkeys.insert(layout.clone(), combo("ctrl+alt+1"));

        assert!(
            runtime_hotkeys(&config).contains_key(&layout),
            "applying a layout is an explicit placement, available in manual mode"
        );

        config.automatic_tiling_enabled = true;
        assert!(runtime_hotkeys(&config).contains_key(&layout));
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

    #[test]
    fn a_workspace_binding_fires_the_workspace_focus_event_with_its_name() {
        let command = Command::FocusWorkspace {
            name: "chat".to_owned(),
        };

        assert!(matches!(
            event_for_command(&command, false),
            Event::WorkspaceFocusRequested { name } if name == "chat"
        ));
        let hotkeys = BTreeMap::from([(command.clone(), combo("ctrl+alt+2"))]);
        let manual = ResolvedConfig {
            hotkeys,
            ..ResolvedConfig::default()
        };
        assert!(
            runtime_hotkeys(&manual).contains_key(&command),
            "a workspace can be focused under manual tiling too"
        );
    }
}
