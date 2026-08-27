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
The mechanism (ARCHITECTURE.md section 8.3) by which Mosaix compares an observed window placement with its expected placement, distinguishing its own move from an external move or a rejected placement.
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

**Balanced grid**:
The initial automatic-tiling policy: each display's managed windows are arranged into a deterministic, near-square grid covering that display's work area, and the grid is recomputed whenever its managed-window inventory or work area changes.
_Avoid_: Automatic layout (when the specific balanced-grid policy is meant), retile

**Managed window**:
A top-level window that Mosaix automatically includes in tiling because the platform reports it as movable and resizable. Elevated windows and windows with an open placement circuit are temporarily excluded rather than treated as permanently unmanaged.
_Avoid_: Tileable window, tracked window (which may include observed but currently excluded windows)

**Active tiling set**:
The managed windows currently eligible to occupy cells in a display's balanced grid. Minimized, elevated, and open-circuit windows remain observed but do not reserve grid cells until they become eligible again.
_Avoid_: Window inventory (which also contains currently excluded windows)

**Visual window order**:
The deterministic startup order for a display's balanced grid, derived from observed window positions from top to bottom and then left to right, with native window ID used only as a final tie-breaker.
_Avoid_: Enumeration order, handle order

**Display migration**:
The reassignment of windows whose previous display disappeared to the nearest surviving display before balanced grids are recomputed. Windows on surviving displays keep their assignment, and connecting a new display does not redistribute existing windows into it.
_Avoid_: Redistribution, retile (when specifically describing cross-display reassignment)

**Usable topology snapshot**:
A successful display observation containing at least one active display. Failed or empty observations during sleep, wake, or hotplug do not replace the last-known topology and instead trigger reconciliation retries.
_Avoid_: Empty topology (an observation with no displays is not authoritative state)

**Placement rejection**:
A placement whose platform call fails, or whose observed bounds remain more than two pixels per edge from the target after a 500 ms settling period. Smaller differences are treated as coordinate-rounding noise.
_Avoid_: Resize failure (a placement may fail while moving, resizing, or both)

**Snap preview overlay**:
A single layered, click-through rectangle drawn by the agent to show where a window will land (ADR 0009). Two triggers: (1) *post-commit flash* — after a zone-snap hotkey, the overlay shows the engine's committed placement for a short dwell; (2) *edge-triggered drag-to-snap* — during an interactive move/resize, the overlay shows the half-zone under the cursor when near a work-area edge, and dropping there commits via `Event::WindowPlaced`. Not a settings surface and not a full FancyZones-style always-on zone map.
_Avoid_: Preview (alone), ghost window, highlight
