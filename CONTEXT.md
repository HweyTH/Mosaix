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
