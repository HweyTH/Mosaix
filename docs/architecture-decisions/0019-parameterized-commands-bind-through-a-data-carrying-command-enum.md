# ADR 0019: Parameterized commands bind through a data-carrying `Command` enum

**Status:** Accepted
**Date:** 2026-09-02

## Context

Saved layouts are named by the user (ADR 0018), so a binding must name a string the schema cannot know in advance. `crates/mosaix-config/src/schema.rs:27` defines `Command` as unit variants only, and `schema.rs:216` binds them as `hotkeys: BTreeMap<Command, KeyCombo>`, which cannot express two bindings of the same verb with different arguments.

`docs/research/hotkey-binding-foundations.md` surveyed how five managers solve this. All five key bindings on a **parsed command string** -- GlazeWM goes as far as implementing `Deserialize` for a `clap::Parser` enum by fabricating an argv (`app_command.rs:265-281`). The survey attributes that unanimity to needing one grammar across a CLI, an IPC surface, and a config file at once. Mosaix has all three (`mosaix-cli`, `mosaix-ipc`, `mosaix-config`), so that pressure applies here too, and it already carries the cost as three separate command types.

What the survey also shows is that every command-string implementation degrades error reporting. GlazeWM rejects the whole file on one typo, i3 does not validate commands at load at all (`config.spec:463` types the field as `string`) and surfaces a typo as an i3-nagbar on first keypress, and whkd and skhd never inspect the command. Mosaix's TOML-plus-serde form already rejects an unknown command at load with a file and line, which is better than any of them.

## Decision

`Command` becomes a data-carrying enum. The fifteen existing verbs stay unit variants and keep their flat form; a parameterized verb gains a payload and a nested form:

```toml
[hotkeys]
snap-left = "ctrl+alt+left"

[hotkeys.apply-layout]
writing = "ctrl+alt+1"
```

Bindings continue to live in one keyspace, `BTreeMap<Command, KeyCombo>`. `Command::ApplyLayout { name: "writing" }` and `Command::ApplyLayout { name: "code" }` are distinct keys, so one map still holds everything and the existing merge, diff, and duplicate-detection paths keep operating on a single set.

Mosaix does not build a command-string parser. Serde continues to reject an unknown verb at load, naming the file and line, and a typo never becomes a silent no-op or a press-time dialog.

A binding naming a layout that does not exist is a **validation error**, reported through `mosaix-config`'s existing `ValidationError` machinery alongside `DuplicateBinding`, naming the file, the binding, and the missing layout. This follows skhd, which reports a reference to an undeclared mode as a load-time `"undeclared identifier"` error, and requires that layout names be declared inside the config directory `validate()` already walks. Layout names are themselves validated where they are defined: empty, whitespace-only, and duplicate-modulo-case names are rejected.

## Alternatives considered

- **A second top-level `[layout-hotkeys]` table with `Command` left closed** (this ADR's own first draft): rejected because it splits one keyspace into two, forcing duplicate detection, diffing, and merge to be taught about both, and because it needs a third table for the next parameterized verb.
- **A parsed command string, as all five surveyed managers use:** rejected because it means building and testing a grammar purely to serve a file TOML already parses, and inheriting a strictly worse error class -- whole-file rejection on a typo (GlazeWM), press-time nagbar (i3), or no validation at all (whkd, skhd).
- **Positional slots (`layout-1` .. `layout-9`):** rejected because a binding would address a position rather than a layout, so inserting a layout would silently re-point every hotkey after it.
- **Create-on-demand when a binding names an unknown layout,** as AeroSpace does for workspaces (`Workspace.get(byName:)` inserts on miss): rejected because that works only for an entity that is definitionally empty until used. A saved layout has geometry; a typo cannot conjure one.
- **Silently ignoring a binding whose layout is missing,** as komorebi does (`if let Some(..)` with no `else`): rejected outright as the silent-misbehavior failure `CLAUDE.md` forbids.

## Consequences

- `Command` stops being a unit-only enum, so its serde representation, `defaults.rs`, `diff.rs`, `validate.rs`, and the `IpcRequest` mapping all change together; `PROTOCOL_VERSION` must be bumped so an older agent reports `VersionMismatch` rather than failing on an unknown tag.
- Duplicate detection stays where it is -- after overlay merge, in `validate.rs` -- which the survey places ahead of GlazeWM and skhd (both silent first-wins) and level with i3 and AeroSpace. A parameterized command's payload must participate in the comparison, so two bindings of `apply-layout("writing")` collide while `apply-layout("writing")` and `apply-layout("code")` do not.
- Validation gains a referential check from binding to layout name, which is only possible while layouts are declared in the config directory. If layouts ever move outside that walk, this rule degrades to a load-time warning plus a visible press-time failure, and never to silence.
- Mosaix keeps a load-time error quality no surveyed manager achieves, which is the property this decision exists to protect.
