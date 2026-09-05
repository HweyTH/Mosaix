# Research: Layout-editor persistence scope and apply-display targeting

> **TL;DR**: Save editor-authored layouts to Mosaix's base `config.toml` by default. A saved layout is normalized, reusable geometry; Mosaix profiles are intentionally opt-in, topology-specific overrides, so silently creating or changing one would make an ordinary save unexpectedly conditional. For this first bridge, apply to the focused managed window's display exactly as the product specification says. It is a safe, deterministic target that needs no new monitor-discovery or stale-display-identity protocol. Add explicit display selection only as a subsequent, deliberate UI/API feature; FancyZones demonstrates that it is useful for a monitor-first editor, but its model also has explicit monitor assignment and a much broader display-management surface.

## Findings

### Saved layouts and topology-specific configuration are separate concerns

PowerToys FancyZones stores reusable custom zone definitions in one shared file, `custom-layouts.json`; monitor assignment and layout hotkeys live in other files in the same FancyZones configuration directory. Its documentation explicitly describes the custom-layout file as exportable across devices, while treating monitor settings separately. This is a useful precedent for keeping normalized layout shapes portable rather than making every newly-created layout conditional on one monitor arrangement. [FancyZones documentation](https://learn.microsoft.com/en-us/windows/powertoys/fancyzones#quickly-switch-between-custom-layouts)

FancyZones then makes assignment a distinct action: selecting a monitor makes that monitor the target for the selected layout. It also defines orientation-based defaults for newly changed display configurations. In other words, it separates *authoring a reusable layout* from *binding it to a particular display*. [FancyZones editor documentation](https://learn.microsoft.com/en-us/windows/powertoys/fancyzones#get-started-with-the-editor)

Other tiling managers do model monitor-specific state, but explicitly. komorebi's static configuration is an ordered `monitors` array whose workspaces carry their layout; when stable monitor targeting is desired, its documentation asks the user to configure `display_index_preferences` using a monitor serial or device identifier, and recommends doing so for multi-monitor setups. [komorebi multi-monitor setup](https://github.com/LGUG2Z/komorebi/blob/master/docs/common-workflows/multi-monitor-setup.md)

GlazeWM similarly makes monitor binding an explicit workspace property: `bind_to_monitor` is optional, has a defined left-to-right index, and applies only if that monitor exists. Its normal configuration remains a single YAML file. [GlazeWM official configuration documentation](https://github.com/glzr-io/glazewm#config-workspaces)

Mosaix already has the equivalent distinction. Its base config applies when no profile matches; a profile is an opt-in sparse overlay keyed by the full display-topology fingerprint. ADR 0004 further says that a new topology falls back to base config and that profiles are not auto-created. [Mosaix terminology](../../CONTEXT.md#language), [ADR 0004](../architecture-decisions/0004-profile-files-matched-by-fingerprint-not-filename.md)

### Recommendation for question 2: default editor saves to base config

Use `config.toml` as the only write target in this feature.

This matches Mosaix's existing profile contract and prevents a surprising outcome: a user drawing a normalized layout while docked should not discover that it disappears whenever they undock or move to an otherwise unmatched topology. Because cells are normalized to a work area, base persistence is naturally portable across resolutions. It also keeps the first write path unambiguous: no profile needs to be selected, created, named, fingerprinted, or atomically added alongside the layout update.

Do not infer profile intent from the currently connected monitors. A future editor can expose an explicit persistence scope such as **All topologies (base)** and **This topology (profile: office)**. That feature should follow the planned write-target-provenance ADR, because profile creation/selection needs a visible user choice and a stable current topology identity. Until then, manual profile editing remains the advanced escape hatch described in the feature brief.

### Focused-target and explicit-target application models

FancyZones supports both ideas, but in different interaction paths. In the layout picker, the user explicitly selects a monitor and that monitor becomes the target of the selected layout. For quick layout switching, its layout hotkey applies to the active screen. [FancyZones editor and quick-switch documentation](https://learn.microsoft.com/en-us/windows/powertoys/fancyzones#get-started-with-the-editor)

The distinction fits FancyZones because it operates a persistent zone map: users choose which monitors get a zone definition, then individual windows snap into those zones. Its documented settings also include optional cross-monitor zones, cross-monitor zone cycling, and a setting for displaying zones on all monitors. [FancyZones settings](https://learn.microsoft.com/en-us/windows/powertoys/fancyzones#settings)

By contrast, komorebi and GlazeWM make ordinary commands contextual. komorebi binds workspace layouts to monitor configurations, while GlazeWM's IPC command API says that a command without an explicit subject operates on the currently focused container; it can only target another container when a caller explicitly provides that subject. [komorebi multi-monitor setup](https://github.com/LGUG2Z/komorebi/blob/master/docs/common-workflows/multi-monitor-setup.md), [GlazeWM IPC API](https://github.com/glzr-io/glazewm-js#readme)

Mosaix's proposed operation is more consequential than changing a FancyZones assignment: it immediately rearranges every managed window on a display. The existing engine already resolves normal zone-snap commands from the focused window's display, while its settings app currently exposes only a single display summary/canvas, not a real monitor picker. [Mosaix engine](../../crates/mosaix-engine/src/lib.rs), [settings editor](../../apps/mosaix-settings/src-tauri/src/editor.rs)

### Recommendation for question 3: focused managed window's display for v1

Keep the initial `ApplyLayoutDraft` and `ApplyLayoutByName` semantics strict: apply to the display containing the focused managed window; if there is no focused managed window, return a clear no-op/rejection result. Do not add an explicit display selector in this implementation.

This follows the approved feature brief, matches Mosaix's existing focus-based snap semantics, and minimizes accidental cross-display rearrangement. It also avoids expanding protocol v2 with a display identity, display enumeration in `GetState`, target-selection UI, and behavior for a target that vanishes between the editor selection and agent execution. Those are material product and reliability decisions, not just a visual control.

When Mosaix later grows multi-display editor state, add an explicit **Apply to display** choice as a separate feature, defaulting to **Focused managed window**. The agent should reject a selected display that is no longer present rather than silently falling back to the focused display; that preserves the user's intent and prevents a stale selection from rearranging another screen. This preserves FancyZones' useful deliberate-selection workflow without weakening Mosaix's safe contextual default.

## Sources

- [PowerToys FancyZones documentation](https://learn.microsoft.com/en-us/windows/powertoys/fancyzones) — official editor behavior, shared custom-layout storage, explicit monitor targeting, active-screen quick switching, and multi-monitor settings.
- [PowerToys FancyZones developer documentation](https://github.com/microsoft/PowerToys/blob/main/doc/devdocs/modules/fancyzones.md) — first-party description of editor configuration writes and runtime refresh after a data-update event.
- [komorebi multi-monitor setup](https://github.com/LGUG2Z/komorebi/blob/master/docs/common-workflows/multi-monitor-setup.md) — first-party monitor configuration and stable display-identity mapping.
- [GlazeWM README/config documentation](https://github.com/glzr-io/glazewm#config-workspaces) — first-party optional monitor binding for workspaces.
- [GlazeWM IPC API](https://github.com/glzr-io/glazewm-js#readme) — first-party IPC command targeting defaults and explicit subject targeting.
- [Mosaix context](../../CONTEXT.md#language) and [ADR 0004](../architecture-decisions/0004-profile-files-matched-by-fingerprint-not-filename.md) — current base/profile and topology-fingerprint contracts.
