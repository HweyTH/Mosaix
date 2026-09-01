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

**Tiling-enabled profile**:
A profile that opts its entire matched display topology into automatic tiling. Individual displays cannot opt out, and base config cannot enable tiling, so unmatched topologies retain manual-snapping-only behavior.
_Avoid_: Per-display tiling profile, global tiling mode

**Automatic-tiling suspension**:
A session-only override that stops grid reflows for the current display topology while leaving manual snapping and the matched profile unchanged. It clears when the agent restarts or the display topology changes.
_Avoid_: Pause (which stops all placement commands), disabling the profile

**Resolved config**:
The merged result of base config plus whichever profile (if any) matches the current topology -- the actual set of hotkeys/gaps/behavior-flags in effect at a given moment. What `Event::ConfigChanged` (ADR 0005) carries into `EngineState`.

**Gap** (outer / inner):
Configurable inset applied to a computed zone `Rect` after `mosaix-layout`'s zone functions produce it (ADR 0006). *Outer gap* insets edges that touch the display's work-area boundary; *inner gap* insets edges that would border a neighboring zone, even under manual-snap-only Phase 1 where no second window is actually being placed. Applied by `apply_gaps`, kept separate from the pure zone-fraction functions (`snap_to_half` etc.), which stay gap-unaware.
_Avoid_: Padding (as a synonym for gap in code/docs -- ARCHITECTURE.md section 12.1 uses "gaps, padding" as two words together; keep "gap" specific to this outer/inner inset concept and don't use "padding" interchangeably for it)

**Balanced grid**:
The sole automatic-tiling policy in the first release: each display's active tiling set is arranged into deterministic, aspect-aware cells that collectively cover its work area before gaps. The planner favors more columns on wider displays and more rows on taller displays; for non-factorable window counts, some cells grow rather than leaving empty grid slots. Other automatic policies remain post-MVP work.
_Avoid_: Automatic layout (when the specific balanced-grid policy is meant), retile

**Grid reflow**:
A recomputation of balanced-grid membership and placements after a relevant window, display, floating-state, or resolved-config change. Bursts of related observations produce one final plan rather than a sequence of visible intermediate arrangements.
_Avoid_: Retile, refresh, rearrange (the last is the explicit recovery command)

**Interactive placement session**:
The interval between native move/resize start and end for one window. Automatic placements for that window and grid reflows for its display are deferred until the session resolves, while other displays continue normally; a display-topology change cancels the session and discards its pending drop target.
_Avoid_: Drag (when resize is also possible), placement transaction

**Rearrange**:
An explicit recovery command that re-enumerates observed windows, resets open placement circuits once, and attempts one fresh grid reflow without changing rules or session-floating state.
_Avoid_: Retile, refresh, reset

**Managed window**:
A top-level window kept under Mosaix management after capability checks and ordered rules. `Tile` windows may enter the active tiling set, `Float` windows remain observed outside the grid, and `Exclude` windows stay outside management; elevated and open-circuit windows are temporarily ineligible rather than permanently unmanaged.
_Avoid_: Tileable window, tracked window (which may include observed but currently excluded windows)

**Active tiling set**:
The `Tile` managed windows currently eligible to occupy cells in a display's balanced grid. Minimized, full-screen, elevated, open-circuit, and session-floating windows remain observed but do not reserve grid cells until they become eligible again.
_Avoid_: Window inventory (which also contains currently excluded windows)

**Session-floating window**:
A managed window temporarily removed from the active tiling set by an explicit manual placement, such as a zone snap or maximize, while automatic tiling is active. It remains observed and manually placeable, returns through `toggle-floating`, and does not persist across an agent restart unless a rule also makes it float.
_Avoid_: Floating rule, excluded window, unmanaged window

**Visual window order**:
The stable order used to assign a display's windows to balanced-grid cells. Startup seeds it from observed positions top-to-bottom then left-to-right (native window ID breaks final ties); new eligible windows append, temporarily ineligible windows retain their slot, and only directional swap deliberately changes it.
_Avoid_: Enumeration order, handle order

**Directional focus**:
A display-local command that focuses the managed window occupying the nearest grid cell in a requested cardinal direction without changing visual window order. It does not wrap or cross a display boundary when no neighbor exists.
_Avoid_: Focus cycle, directional navigation

**Directional swap**:
A display-local command that exchanges the focused managed window's place in visual window order with its directional neighbor, then recomputes their balanced-grid placements. It does not wrap or cross a display boundary when no neighbor exists.
_Avoid_: Directional move, window move

**Display migration**:
The reassignment of windows whose previous display disappeared to the nearest surviving display before balanced grids are recomputed. Windows on surviving displays keep their assignment, and connecting a new display does not redistribute existing windows into it.
_Avoid_: Redistribution, retile (when specifically describing cross-display reassignment)

**Display transfer**:
An explicit user command that moves a managed window from its source display to another display, removes it from the source visual window order, appends it to the destination order, and triggers one grid reflow on each display.
_Avoid_: Display migration, directional swap, redistribution

**Usable topology snapshot**:
A successful display observation containing at least one active display. Failed or empty observations during sleep, wake, or hotplug do not replace the last-known topology and instead trigger reconciliation retries.
_Avoid_: Empty topology (an observation with no displays is not authoritative state)

**Placement rejection**:
A placement whose platform call fails, or whose observed bounds remain more than two pixels per edge from the target after a 500 ms settling period. Smaller differences are treated as coordinate-rounding noise.
_Avoid_: Resize failure (a placement may fail while moving, resizing, or both)

**Degraded tiling**:
A health condition in which automatic tiling remains active for eligible windows while one or more managed windows are temporarily excluded by a placement or adapter failure. Diagnostics retain each affected window and exclusion reason; degraded tiling is neither pause nor automatic-tiling suspension.
_Avoid_: Failed tiling, paused, suspended

**Snap preview overlay**:
A single layered, click-through rectangle drawn by the agent to show where a window will land (ADR 0009). Two triggers: (1) *post-commit flash* — after a zone-snap hotkey, the overlay shows the engine's committed placement for a short dwell; (2) *edge-triggered drag-to-snap* — during an interactive move/resize, the overlay shows the half-zone under the cursor when near a work-area edge, and dropping there commits via `Event::WindowPlaced`. In automatic tiling, either explicit zone-placement path also makes the window session-floating. Not a settings surface and not a full FancyZones-style always-on zone map.
_Avoid_: Preview (alone), ghost window, highlight

**Focus border**:
A persistent, click-through outline around the focused managed window, including tiled and floating windows, shown only while automatic tiling is active. Its only first-release customization is enabled state, color, and thickness; it is hidden in manual, suspended, and paused states. Also hidden whenever there is nothing to outline: a focused window that is minimized, hidden, or cloaked, and one currently in an interactive placement session, whose position is not yet settled. A maximized or full-screen window keeps its border. Follows where the window actually is rather than where Mosaix last placed it (ADR 0017), and is drawn by its own overlay, distinct from the snap preview.
_Avoid_: Snap preview overlay, focus animation, window decoration
