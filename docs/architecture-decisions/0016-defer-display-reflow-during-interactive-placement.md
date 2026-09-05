# ADR 0016: Defer display reflow during interactive placement

**Status:** Accepted
**Date:** 2026-08-31

## Context

Native move/resize events can overlap window lifecycle, configuration, and topology observations that would normally trigger grid reflow. Applying placements to the window under the pointer, or moving its neighboring cells mid-drag, would create a visible fight between the user and the manager.

## Decision

During an interactive placement session, Mosaix suppresses automatic placements for the dragged window and defers pending grid reflows for that display. Other displays continue processing normally. When the session ends, the edge-zone decision either session-floats the window or returns it to tiling, and all coalesced changes produce one final grid reflow. A display-topology change is the safety exception: it cancels the session immediately, discards any pending edge-drop target, and runs topology reconciliation and Display migration without waiting for a stale move/resize end event.

## Alternatives considered

- **Continue reflowing the dragged display:** rejected because placements could move the target or its neighbors underneath the pointer.
- **Freeze automatic tiling on every display:** rejected because the interaction and pending changes are local to one display.

## Consequences

- Window openings, closures, and eligibility changes on the dragged display may remain visually pending until release.
- The session end is a synchronization point that must resolve all coalesced state before issuing placements.
- Independent displays remain responsive during a long drag.
- Topology changes can interrupt a drag, but cannot leave a window assigned to a disappeared display or commit geometry calculated against stale work-area bounds.
