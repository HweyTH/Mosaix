# ADR 0030: Window rules live in base config only

**Status:** Accepted
**Date:** 2026-09-10

## Context

`[[rules]]` in `config.toml` was refused with `unknown field 'rules'`. The
cause went deeper than the schema: nothing anywhere sent
`Event::RulesChanged`, and nothing outside `mosaix-rules` referenced
`RuleConfig`, so the rules engine was complete, tested, and unreachable.
Making it reachable meant choosing which configuration layer owns rules.

ADR 0004 established that a profile is a sparse overlay matched by display
topology, and `CONTEXT.md`'s **Profile** entry states that a profile "can
override anything base config has, including hotkey bindings and saved
layouts". Read literally, that includes rules.

A rule decides whether a window is tiled, floated, or excluded from
management altogether, and which logical workspace it joins. That decision
is about the window and the application that owns it, not about the
monitors currently attached.

## Decision

Rules are declared in `config.toml` only. `BaseConfig` carries them,
`merge` copies them unchanged into every `ResolvedConfig`, and a profile
that declares `[[rules]]` is refused at validation with a message saying
where they belong -- the exact mirror of how base config already refuses
`[workspace_switching]`, which is profile-only for the opposite reason.

This is a deliberate exception to the "a profile can override anything"
sentence in `CONTEXT.md`'s **Profile** entry, which that entry now records.

Validation compiles every rule through `Rule`'s own `TryFrom`, so an
unparseable regex or an unknown role rejects the directory rather than
yielding a rule that silently never matches. Duplicate rule ids are refused
too, because a rule id is what the evaluation trace names when it explains
why a window was floated.

## Alternatives considered

- **Let a profile override rules, like every other field:** rejected
  because a window's management decision would then change under it when a
  monitor is unplugged. A window that was excluded on the docked topology
  would become tiled on the laptop one, and the user would see windows they
  had deliberately taken out of management rearrange themselves on hotplug.
- **Let a profile add rules while base config keeps its own:** rejected as
  the same hazard in a quieter form, plus an ordering question with no good
  answer -- whether a profile's rule of equal priority beats base config's
  is a coin flip a user would have to memorise.
- **Accept `[[rules]]` in a profile and ignore it:** rejected outright.
  Silently ignoring configuration a user wrote is the failure mode this
  repository refuses everywhere else.
- **Keep rules out of configuration and expose them over IPC only:**
  rejected because rules are exactly the kind of durable preference a user
  wants in a file they can read, diff, and copy between machines.

## Consequences

- A user who wants per-topology management has to express it another way:
  workspaces, or an explicit float toggle. No mechanism replaces it today.
- `mosaix-config` now depends on `mosaix-rules`. There is no cycle:
  `mosaix-rules` depends only on `mosaix-domain`.
- `ResolvedConfig` holds rules in their config form rather than compiled. A
  compiled `Rule` owns a `Regex`, which has no `PartialEq`, and
  `ResolvedConfig` equality is what lets the reducer tell a real config
  change from a rewrite of identical content. The reducer compiles them on
  startup and on every `ConfigChanged`.
- Should per-topology rules ever be wanted, this ADR is what has to be
  superseded, and the refusal message is the thing that tells a user the
  decision exists.
