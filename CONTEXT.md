# Mosaix

Mosaix augments the native Windows/macOS window manager with manual snapping and automatic tiling (see ARCHITECTURE.md for the full design). This glossary tracks domain terms specific to that design as they're pinned down.

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

**Persistent undo**:
A history of the newest 100 committed undo transactions from the last seven days that remains available after the agent restarts and identifies its target windows without relying on native window handles. An entry whose target cannot be matched confidently is refused rather than applied to a guessed window.
_Avoid_: Restore (the existing command remembers only one in-session placement), rollback

**Undo transaction**:
The complete reversible state change and window placements committed by one explicit placement-changing user command on one display topology, reversed as one operation even when it moved several windows or changed the displayed workspace. Passive reflows create no undo transaction; a topology mismatch or any missing or uncertain target refuses the whole transaction before any window moves, without skipping to older history.
_Avoid_: Undo entry (when referring to an individual window placement), placement transaction

**Base config**:
The settings in `config.toml` -- hotkeys, gaps, behavior flags -- that apply whenever the current display topology matches no saved profile. Not itself called a "profile"; profiles are overlays *on top of* base config, not siblings of it.

**Profile**:
A named file under `profiles/` (e.g. `profiles/office.toml`) that overrides some or all of base config's fields for one specific display topology. Matched by comparing its stored `fingerprint` field against the current `topology_fingerprint()` output -- never by filename, since a real fingerprint contains characters (`|`, spaces) that aren't valid in a Windows filename (ADR 0004). A profile is a sparse overlay: any field it doesn't mention falls through to base config (field-level merge, ADR 0004), and it can override anything base config has, including hotkey bindings and saved layouts. Its `layouts` table is keyed by name, so a profile replaces the layouts it names and inherits the rest.
_Avoid_: Layout (alone -- this repo also has zone layouts and, eventually, workspace layouts; say "layout profile" or "profile" with the monitor-topology sense established in context)

**Tiling-enabled profile**:
A profile that opts its entire matched display topology into automatic tiling. Individual displays cannot opt out, and base config cannot enable tiling, so unmatched topologies retain manual-snapping-only behavior.
_Avoid_: Per-display tiling profile, global tiling mode

**Automatic-tiling suspension**:
A session-only override that stops grid reflows for the current display topology while leaving manual snapping and the matched profile unchanged. It clears when the agent restarts or the display topology changes.
_Avoid_: Pause (which stops all placement commands), disabling the profile

**Resolved config**:
The merged result of base config plus whichever profile (if any) matches the current topology -- the actual set of hotkeys/gaps/behavior-flags in effect at a given moment. What `Event::ConfigChanged` (ADR 0005) carries into `EngineState`. It also carries *provenance*: which layer supplied each hotkey binding, and the profile file it came from, so a GUI edit can be written to the layer that supplies the value it shows (ADR 0022).
_Avoid_: Effective config, active config (either is fine in prose, but the type is `ResolvedConfig` and the glossary term is "resolved config").

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

**Constraint-overflow window**:
A managed tiled window temporarily shown outside the tree arrangement because the display cannot satisfy every live leaf's minimum size after gaps reach zero. Newer insertions overflow first and automatically reclaim their saved tree positions when space permits.
_Avoid_: Session-floating window, placement rejection, dormant tree leaf

**Visual window order**:
The stable order used to assign a display's windows to balanced-grid cells. Startup seeds it from observed positions top-to-bottom then left-to-right (native window ID breaks final ties); new eligible windows append, temporarily ineligible windows retain their slot, and only directional swap deliberately changes it.
_Avoid_: Enumeration order, handle order

**Container tree**:
A normalized hierarchy that owns the ordered tiled windows and nested layout groups for one automatic-tiling surface. It is independent of whether Mosaix exposes multiple switchable workspaces.
_Avoid_: Workspace tree, layout (alone)

**BSP insertion**:
Adding a tiled window by dividing the focused tiled leaf into equal siblings along that leaf's longer axis. With no focused tiled leaf, the largest leaf is divided, with visual window order breaking size ties.
_Avoid_: Append, preselection, insertion direction

**Dormant tree leaf**:
A persisted window position whose window is closed or not currently matched, retained without consuming screen space so a later confident match can reclaim its structural position. A window that is still open but leaves the tree -- floated, minimized, or transferred to another display -- leaves no dormant leaf behind. It expires after seven days unless a saved scene retains it (scene retention arrives with issue #28; until then expiry is unconditional), and an explicit remove-position command deletes it immediately.
_Avoid_: Empty tile, placeholder window, missing window

**Tree resize**:
Moving the closest container-tree divider facing the requested direction by five percentage points per command while keeping every affected window at or above its minimum size. A resize never pays for itself by reducing gaps or overflowing a window: when the full step would, the largest smaller whole-point step that does not is taken, and none if none fits. The resulting placements, and the tree as it stood before, form one undo transaction.
_Avoid_: Window resize (alone), free resize, pixel resize

**Logical workspace**:
A member of Mosaix's global pool of uniquely named managed-window groups, created through configuration or an explicit command, owning one container-tree root and displayed on at most one monitor at a time. Every tiled or floating managed window belongs to exactly one; excluded windows belong to none, and focus, move, or rule targets never create one implicitly.
_Avoid_: Virtual desktop, Space, saved workspace

**Workspace focus**:
Displaying a hidden logical workspace on the focused monitor, or focusing its last-focused live window when it is already displayed on another monitor. It never moves a displayed workspace between monitors.
_Avoid_: Workspace move, display transfer, workspace switch (alone)

**Workspace move**:
An explicit command that transfers a displayed logical workspace to another monitor, exchanging it with whatever that monitor showed so nothing becomes hidden. Identity, membership, and the container tree travel with it; the last-focused window is retained. It is the only way a displayed workspace changes monitor: `Workspace focus` never moves one.
_Avoid_: Workspace switch, display transfer (which moves one window)

**Unfilled display**:
A display for which the workspace pool has no hidden, empty workspace to display. It arranges the windows physically on it exactly as it did before workspaces existed, those windows belong to no workspace, and published state reports both. The engine never invents a workspace name to fill it; declaring or creating one and focusing it there is the remedy.
_Avoid_: Default workspace, anonymous workspace

**Focused display**:
The display targeted by display-scoped commands, following the focused managed window when one exists and otherwise retaining the last explicitly targeted display. It remains defined when the displayed logical workspace is empty.
_Avoid_: Primary display, cursor display, focused window's display

**Window parking**:
The reversible relocation of windows from a non-displayed logical workspace to a recoverable edge position while their workspace membership remains unchanged. On Windows the position is a parking site beyond the virtual screen (ADR 0029); the window keeps its show state, styles, taskbar button, and Alt-Tab entry, and is never activated on the way out or back. A maximized window is taken to its normal size first and re-maximized on restore; a minimized window is left minimized and unmoved.
_Avoid_: Hide, cloak, minimize

**Parking site**:
The place the platform adapter has verified, for the current topology, that a parked window can be moved to without any monitor covering it: beyond one edge of the virtual screen by a fixed margin, chosen from the reported display geometry and confirmed against the live desktop with `MonitorFromRect` (ADR 0029). It is re-validated on every topology change, and its absence is a typed refusal that parks nothing rather than a fallback to another mechanism.
_Avoid_: Off-screen corner, hiding position, parking edge (when the whole validated site is meant)

**Recovery ledger**:
A small SQLite file beside the state database, written before any window is parked, that records the native handle, the owning process instance (process id plus kernel creation time), the window class, the original display, the visible and normal bounds, and the show state. An entry is acknowledged durable before the engine authorises the parking effect it describes. Startup and the out-of-process `restore-windows` command read it before any identity reconciliation and touch only a handle whose live evidence still matches; a stale, reused, or ambiguous handle is reported and left alone. It is never cross-session identity: the state database holds no native handle.
_Avoid_: Undo history, session state, handle cache

**Workspace switch transaction**:
An all-or-nothing change of the logical workspace displayed on one monitor, durably recording recovery data before parking or restoring windows. Any placement failure cancels the switch and compensates completed moves; success creates one undo transaction covering both assignment and placements.
_Avoid_: Placement transaction, partial workspace switch

**Workspace switching status**:
One of four states published for the current topology: disabled (no matched profile requests switching; base config cannot), requested (the matched profile's complete workspace-to-display mapping is in effect, but switching waits on a verified parking site or on persistence recovering), unavailable (the profile requests it but its mapping could not be applied, so the previous displayed assignment stands), or experimental (mapping in effect and parking authorised). A profile mapping applies to every display atomically or not at all, and the engine invents no workspace name to complete one.
_Avoid_: Enabled (which does not say whether parking is authorised), workspace mode

**Workspace-switch degraded**:
A health condition in which compensation for a failed workspace switch could not restore every moved window. Further switching remains blocked until the explicit restore action reconciles the affected windows.
_Avoid_: Persistence-degraded, degraded tiling

**Window identity match**:
A scored comparison between durable evidence and live managed windows that yields one of three outcomes: confident, ambiguous, or no match. Only a confident outcome may authorize a persisted placement.
_Avoid_: Handle match, title match, best guess

**Directional focus**:
A display-local command that focuses the nearest eligible window in a requested cardinal direction without changing layout state. In balanced-grid mode candidates occupy active grid cells; in container-tree mode candidates occupy live, actively arranged tiled leaves. It does not wrap or cross a display boundary when no neighbor exists.
_Avoid_: Focus cycle, directional navigation

**Directional swap**:
A display-local command that exchanges the focused managed window with the same neighbor that `Directional focus` would select. In balanced-grid mode it exchanges their places in visual window order. In container-tree mode both endpoints must be live, actively arranged tiled leaves; it exchanges only their window bindings while preserving every container, split axis, weight, and dormant leaf. Focus follows the same window identity to its new place, the exchange is one undo transaction, and floating or constraint-overflow windows do not participate. It does not wrap or cross a display boundary when no neighbor exists.
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

**Persistence-degraded**:
A health condition in which live window management continues but the latest committed state is not durable. New persistent-undo entries and workspace parking remain unavailable until persistence recovers.
_Avoid_: Degraded tiling, read-only mode, paused

**Snap preview overlay**:
A single layered, click-through rectangle drawn by the agent to show where a window will land (ADR 0009). Two triggers: (1) *post-commit flash* — after a zone-snap hotkey, the overlay shows the engine's committed placement for a short dwell; (2) *edge-triggered drag-to-snap* — during an interactive move/resize, the overlay shows the half-zone under the cursor when near a work-area edge, and dropping there commits via `Event::WindowPlaced`. In automatic tiling, either explicit zone-placement path also makes the window session-floating. Not a settings surface and not a full FancyZones-style always-on zone map.
_Avoid_: Preview (alone), ghost window, highlight

**Focus border**:
A persistent, click-through outline around the focused managed window, including tiled and floating windows, shown only while automatic tiling is active. Its only first-release customization is enabled state, color, and thickness; it is hidden in manual, suspended, and paused states. Also hidden whenever there is nothing to outline: a focused window that is minimized, hidden, or cloaked, and one currently in an interactive placement session, whose position is not yet settled. A maximized or full-screen window keeps its border. Follows where the window actually is rather than where Mosaix last placed it (ADR 0017), and is drawn by its own overlay, distinct from the snap preview.
_Avoid_: Snap preview overlay, focus animation, window decoration

**Saved layout**:
A named set of zone rectangles for one display that a user can apply on demand, stored in configuration and overridable per display topology like any other config field (ADR 0004). Written either by hand or from the settings application, which asks the agent to perform the write rather than editing a file itself (ADR 0022). It records *shape only*: applying it fills its cells with whichever managed windows exist, in visual window order, and never identifies a particular window (ADR 0018). Restoring a layout to specific windows by identity is deferred (issue #28).
_Avoid_: Arrangement, scene, saved workspace. ARCHITECTURE.md uses "arrangement" in two senses -- "saved arrangements" (section 1) for this concept, and "the requested arrangement" (section 9.3) for a planner's current output -- so prefer "saved layout" for the stored artifact and leave "arrangement" to the planner sense.

**Hotkey capture**:
Reading a key combination by having the user press it in the settings hotkey editor, rather than typing it as text. Because `RegisterHotKey` is OS-arbitrated and gives Mosaix no way to swallow a keystroke (ADR 0002), capture requires the agent to unregister every binding for as long as the editor window is open, bounded by the editor's IPC connection so a crash re-registers (ADR 0021). The capture buffer itself is armed only while a single capture dialog is frontmost.
_Avoid_: Capture (alone). "Capture" also names the deferred idea of recording the current on-screen arrangement as a saved layout (issue #28); say "hotkey capture" for this one and "capture-from-current" for that one.
