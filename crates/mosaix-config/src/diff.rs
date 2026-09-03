//! Pure diff between two resolved hotkey binding sets, mirroring
//! `mosaix_engine::diff_placements`'s shape (ADR 0005): both let a
//! poll-driven executor ask "what changed since I last acted on this?"
//! against committed reducer state, rather than reacting to individual
//! events. `mosaix-agent`'s hotkey-rebind poller is [`diff_bindings`]'s
//! only caller today.

use std::collections::BTreeMap;

use crate::schema::{Command, KeyCombo};

/// The commands whose binding differs between `previous` and `current`:
/// `Some(combo)` for a command newly bound or rebound to `combo` in
/// `current`, `None` for a command that was bound in `previous` but has no
/// binding in `current`. Empty when the two binding sets are identical --
/// the signal a caller like `mosaix-agent`'s poller uses to decide whether
/// a re-registration is needed at all.
///
/// Unlike [`mosaix_engine::diff_placements`], a removal is reported (as
/// `None`) rather than ignored: an unbound command needs its hotkey
/// actually unregistered, whereas a placement diff has nothing sensible to
/// do with a window that disappeared.
pub fn diff_bindings(
    previous: &BTreeMap<Command, KeyCombo>,
    current: &BTreeMap<Command, KeyCombo>,
) -> Vec<(Command, Option<KeyCombo>)> {
    let mut changed: Vec<(Command, Option<KeyCombo>)> = Vec::new();

    for (command, combo) in current {
        if previous.get(command) != Some(combo) {
            changed.push((command.clone(), Some(combo.clone())));
        }
    }
    for command in previous.keys() {
        if !current.contains_key(command) {
            changed.push((command.clone(), None));
        }
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;

    fn combo(raw: &str) -> KeyCombo {
        KeyCombo::parse(raw).expect("fixture combo should parse")
    }

    fn bindings(pairs: &[(Command, &str)]) -> BTreeMap<Command, KeyCombo> {
        pairs
            .iter()
            .map(|(command, raw)| (command.clone(), combo(raw)))
            .collect()
    }

    #[test]
    fn identical_binding_sets_produce_no_diff() {
        let set = bindings(&[(Command::SnapLeft, "ctrl+alt+left")]);
        assert!(diff_bindings(&set, &set).is_empty());
    }

    #[test]
    fn a_rebound_command_is_reported_with_its_new_combo() {
        let previous = bindings(&[(Command::SnapLeft, "ctrl+alt+left")]);
        let current = bindings(&[(Command::SnapLeft, "ctrl+shift+left")]);

        assert_eq!(
            diff_bindings(&previous, &current),
            vec![(Command::SnapLeft, Some(combo("ctrl+shift+left")))]
        );
    }

    #[test]
    fn a_newly_bound_command_is_reported_with_its_combo() {
        let previous = BTreeMap::new();
        let current = bindings(&[(Command::SnapLeft, "ctrl+alt+left")]);

        assert_eq!(
            diff_bindings(&previous, &current),
            vec![(Command::SnapLeft, Some(combo("ctrl+alt+left")))]
        );
    }

    #[test]
    fn a_removed_command_is_reported_as_none() {
        let previous = bindings(&[(Command::SnapLeft, "ctrl+alt+left")]);
        let current = BTreeMap::new();

        assert_eq!(
            diff_bindings(&previous, &current),
            vec![(Command::SnapLeft, None)]
        );
    }

    #[test]
    fn unchanged_commands_are_left_out_of_a_mixed_diff() {
        let previous = bindings(&[
            (Command::SnapLeft, "ctrl+alt+left"),
            (Command::SnapRight, "ctrl+alt+right"),
            (Command::SnapTop, "ctrl+alt+up"),
        ]);
        let current = bindings(&[
            (Command::SnapLeft, "ctrl+alt+left"),
            (Command::SnapRight, "ctrl+shift+right"),
            (Command::SnapBottom, "ctrl+alt+down"),
        ]);

        let mut result = diff_bindings(&previous, &current);
        result.sort_by(|(left, _), (right, _)| left.cmp(right));

        let mut expected = vec![
            (Command::SnapRight, Some(combo("ctrl+shift+right"))),
            (Command::SnapTop, None),
            (Command::SnapBottom, Some(combo("ctrl+alt+down"))),
        ];
        expected.sort_by(|(left, _), (right, _)| left.cmp(right));

        assert_eq!(result, expected);
    }
}
