# Mosaix

Mosaix augments the native Windows/macOS window manager with manual snapping, automatic tiling, and per-monitor workspaces (see ARCHITECTURE.md for the full design). This glossary tracks domain terms specific to that design as they're pinned down.

## Language

**Zone**:
A named rectangular region a window can be snapped to, expressed as a normalized fraction of a container (typically a display's work area) -- `HalfZone`, `QuarterZone`, and `ThirdZone` in `mosaix-layout`.
_Avoid_: Region, area (when a specific named zone is meant)

**Zone cycle**:
The sequence a horizontal snap command steps through on repeated presses of the same command: half -> third -> two-thirds, wrapping back to half. Only the four horizontal directions (snap-left, snap-right) cycle; snap-up/snap-down stay single-shot, since `ThirdZone` has no vertical equivalent.
_Avoid_: Repeat, resize loop

**Cycle step**:
A window's current position within its zone cycle -- which of half/third/two-thirds it's currently on for the direction it was last snapped in. Tracked per window in the engine, reset to the first step whenever the window is moved by anything other than Mosaix's own last placement (see Placement transaction correlation).

**Placement transaction correlation**:
The mechanism (ARCHITECTURE.md section 8.3) by which the engine matches an incoming window bounds-changed notification against the transaction it expects from its own last placement command, to tell "Mosaix moved this" apart from "something else moved this" (a manual drag, another app, a native OS snap). Described in the architecture doc but not yet implemented -- `mosaix-engine` doesn't handle bounds-changed events at all yet, and `mosaix-platform-windows`'s `LocationChanged` OS event isn't wired to it. Cycle step invalidation depends on this landing first.
_Avoid_: Transaction (alone, without "placement" -- too generic)

**Base config**:
The settings in `config.toml` -- hotkeys, gaps, behavior flags -- that apply whenever the current display topology matches no saved profile. Not itself called a "profile"; profiles are overlays *on top of* base config, not siblings of it.

**Profile**:
A named file under `profiles/` (e.g. `profiles/office.toml`) that overrides some or all of base config's fields for one specific display topology. Matched by comparing its stored `fingerprint` field against the current `topology_fingerprint()` output -- never by filename, since a real fingerprint contains characters (`|`, spaces) that aren't valid in a Windows filename (ADR 0004). A profile is a sparse overlay: any field it doesn't mention falls through to base config (field-level merge, ADR 0004), and it can override anything base config has, including hotkey bindings.
_Avoid_: Layout (alone -- this repo also has zone layouts and, eventually, workspace layouts; say "layout profile" or "profile" with the monitor-topology sense established in context)

**Resolved config**:
The merged result of base config plus whichever profile (if any) matches the current topology -- the actual set of hotkeys/gaps/behavior-flags in effect at a given moment. What `Event::ConfigChanged` (ADR 0005) carries into `EngineState`.

**Gap** (outer / inner):
Configurable inset applied to a computed zone `Rect` after `mosaix-layout`'s zone functions produce it (ADR 0006). *Outer gap* insets edges that touch the display's work-area boundary; *inner gap* insets edges that would border a neighboring zone, even under manual-snap-only Phase 1 where no second window is actually being placed. Applied by `apply_gaps`, kept separate from the pure zone-fraction functions (`snap_to_half` etc.), which stay gap-unaware.
_Avoid_: Padding (as a synonym for gap in code/docs -- ARCHITECTURE.md section 12.1 uses "gaps, padding" as two words together; keep "gap" specific to this outer/inner inset concept and don't use "padding" interchangeably for it)
