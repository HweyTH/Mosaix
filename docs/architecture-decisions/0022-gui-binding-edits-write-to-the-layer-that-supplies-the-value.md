# ADR 0022: GUI binding edits write to the layer that supplies the value

**Status:** Accepted
**Date:** 2026-09-02

## Context

Configuration is layered: base config plus sparse per-topology profile overlays that merge field by field and may override any field, including hotkeys (ADR 0004). A GUI hotkey editor must therefore choose which file receives a write, and the value it displays comes from `ResolvedConfig` -- the merge result -- not from either file directly.

`docs/research/hotkey-binding-foundations.md` found no precedent. No surveyed manager has both a layered config and a UI that writes to it: i3 and skhd support includes but ship no GUI and no config-writing command, and GlazeWM's `wm-update-workspace-config` mutates configuration in memory and deliberately never writes the file, so runtime edits are lost on the next reload. That is defensible for a runtime tweak and not for an editor whose purpose is persistence.

## Decision

A binding edited in the GUI is written to the layer that currently supplies its resolved value. If the matched profile overrides that binding, the write lands in the profile; otherwise it lands in base config.

The editor displays the destination filename before saving, and offers an explicit control to redirect the write to base config instead. The destination is never inferred silently: whenever a profile is active, the value on screen and the file that would receive the write are different objects, and the user is told which.

Writes are performed by the agent, not the settings frontend, consistent with the settings app's existing rule that the frontend does not edit configuration files. A write produces a candidate the whole-directory atomic validation of ADR 0007 accepts or rejects as a unit, and the resulting change returns through the normal debounced hot-reload path (ADR 0008).

## Alternatives considered

- **Always write base config:** rejected because editing a binding the active profile overrides would appear to succeed and change nothing -- the silent misbehavior `CLAUDE.md` forbids.
- **Always write the matched profile:** rejected because every edit would silently become topology-specific, so a binding set at a desk would vanish when the display topology changed.
- **Prompt for the destination on every save:** rejected as a modal on the most common action; the destination is shown rather than asked, with an override available.
- **Mutate in memory only, as GlazeWM does:** rejected because the edit would be lost on the next reload, which defeats the feature.
- **Flatten the layers and drop profile overrides for hotkeys:** rejected because per-topology hotkeys are an existing shipped capability (ADR 0004, ADR 0005 live rebind on profile switch).

## Consequences

- `ResolvedConfig` must carry provenance -- which layer supplied each binding -- which it does not today; it currently holds merged values with no record of their origin. The merge in `mosaix-config` has to preserve that and expose it over IPC for the editor to display.
- Editing while a profile is matched can write a file the user has never opened, so naming the destination in the UI is load-bearing rather than cosmetic.
- The editor will see its own write return through hot reload and must treat that as expected rather than as an external change.
- A GUI write that would produce an invalid configuration directory is rejected whole, so the editor needs to surface a validation error from the agent rather than assume its write succeeded.
