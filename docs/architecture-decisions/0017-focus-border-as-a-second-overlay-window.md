# ADR 0017: Focus border as a second overlay window, driven by engine revision

**Status:** Accepted
**Date:** 2026-09-01

## Context

CONTEXT.md's "Focus border" calls for a persistent, click-through outline around the focused managed window, tiled or floating, shown only while automatic tiling is active, customizable in enabled state, color, and thickness.

A snap preview overlay already exists (ADR 0009), so the obvious move is to reuse it. It does not fit. `PreviewOverlay` paints `FillRect` at ~30% alpha across its whole client area, which would wash the focused window in translucent blue rather than outline it. Worse, its controller's modes deliberately preempt each other -- `Mode::Dragging` hides an in-flight flash, and flash requests are dropped during a drag -- because all three modes share one window. A persistent border folded into that machine would be extinguished by every snap and every drag.

Nothing pushes "the border should change now" either. Focus, a grid reflow, a config reload, pause, the tiling toggle, and a display hotplug can all change it.

## Decision

The border is drawn by its own layered window -- `mosaix_platform_windows::FocusBorderOverlay`, its own thread, its own window class -- and driven by a controller that polls `StateReader::revision()`.

Two overlays coexist by construction: `create_overlay_window` already ignores `RegisterClassW` failure because "a prior overlay in the same process may have already registered this class", and each overlay owns its thread and its `thread_local` pending-state slot.

The outline is hollow because the window's *region* excludes its middle (`SetWindowRgn` over a full-size region with an inset region subtracted), so what shows through is the untouched pixels of the window beneath, at any thickness. It is painted *inside* the target bounds, so it never reaches into a gap or over a neighbor whatever `[gaps]` is set to.

Visibility reuses an existing invariant rather than restating it: `automatic_tiling_active && !paused`, the same guard the reducer applies before a grid reflow. Since `automatic_tiling_active` already means `automatic_tiling_enabled && !automatic_tiling_suspended`, CONTEXT.md's "hidden in manual, suspended, and paused states" falls out of one condition and cannot drift from what tiling itself considers live.

## Considered Options

- **A fourth mode of the snap-preview controller.** Rejected for the two reasons in Context: wrong paint, and modes that preempt each other.

- **Color-key transparency (`LWA_COLORKEY`) instead of a region.** Rejected -- it makes one color unusable for the border itself and leaves fringing where the keyed color meets anti-aliased content beneath. A region is exact at any thickness.

- **An outset border drawn around the outside of the window.** Rejected -- correct only when the gap is at least the border thickness. At `gaps = 0` it paints over the neighbor; at a work-area edge it paints off-screen. Insetting needs no clamping.

- **Pushing border updates from each call site that can change it.** Rejected -- six-plus call sites across `main.rs` and the reducer, where missing one leaves a border stuck around the wrong window. Polling one counter cannot miss a case.

- **Polling `StateReader::snapshot()` directly.** Rejected -- it deep-clones every window, display, and rule, far too costly at 33 ms. `StateReader::revision()` was added for this: same lock, one `u64`. The state is cloned only when the number moves, and the overlay touched only when the computed target differs.

- **Reading `WindowPlacement::bounds` for the border's rectangle.** Rejected, and this is the subtle one. `bounds` is the placement Mosaix last *intended*; `Event::WindowBoundsObserved` compares observed-against-intended to detect an external move and reset cycle state (ADR 0001), so it must stay stale on purpose. A border drawn from it trails any window something else repositioned -- exactly the floating windows this feature is meant to cover. `WindowPlacement` therefore gained `observed_bounds`, written on every observation, and the border reads that.

## Consequences

`EngineState` now records intent and observation separately, making explicit the expected-vs-actual distinction architecture doc section 8.3 already committed to. `Event::WindowBoundsObserved` bumps the revision when observed bounds actually change, where before it bumped only on a cycle-step reset; a no-op observation still bumps nothing.

The border is hidden while the focused window is the subject of an interactive placement session, since the engine defers its placement for the duration (ADR 0016). A drag of any other window leaves it alone.

It is also hidden for minimized, hidden, and cloaked windows. Those stay in `inventory` as temporarily ineligible rather than unmanaged, so membership alone would leave a border painted on bare desktop after a minimize. Maximized and full-screen windows are ineligible for a grid cell but visible, and keep their border.

Gating on tiling means no border under manual-snapping-only topologies, which is what CONTEXT.md specifies but not obviously right -- focus can be lost while manually snapping too. That was inherited from the glossary entry rather than argued from a felt need, and it is one predicate in `focus_border_target` to revisit.

`[focus_border]`'s three fields each carry an independent serde default, so a file setting only `color` still parses. `validate` rejects a non-`#RRGGBB` color or a thickness outside 1..=40 for the whole candidate directory (ADR 0007).

`FocusBorderOverlay` duplicates the thread/lifecycle plumbing of `PreviewOverlay`. This ADR justifies two windows, not two copies of that scaffolding; extracting a shared layered-overlay thread is deferred, not decided against.

Overlay startup failure is logged and degrades to no border, matching the snap preview and tray.
