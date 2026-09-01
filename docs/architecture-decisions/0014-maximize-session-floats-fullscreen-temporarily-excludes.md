# ADR 0014: Maximize session-floats; full-screen temporarily excludes

**Status:** Accepted
**Date:** 2026-08-31

## Context

Maximize is an explicit user placement, while application full-screen is a temporary presentation state. Treating both as ordinary tiled geometry would cause grid reflow to fight the user's action; treating both as persistent floating would make a window unexpectedly stay outside the grid after leaving full-screen.

## Decision

Explicitly maximizing a tiled window makes it session-floating and reflows the remaining active tiling set. Entering application full-screen instead makes the window temporarily ineligible while preserving its visual window order slot; exiting full-screen returns it to that slot and triggers one grid reflow. Neither state reserves an empty grid cell.

## Alternatives considered

- **Immediately retile maximized or full-screen windows:** rejected because automatic reflow would override an explicit user-visible state.
- **Treat maximize as a monocle layout:** deferred with monocle and other post-MVP policies.
- **Make full-screen persistently session-floating:** rejected because leaving a temporary application state should restore normal tiling without another command.

## Consequences

- Maximize behaves like other explicit manual placements and requires `toggle-floating` to return during the same session.
- Full-screen applications can present uninterrupted and rejoin the grid automatically on exit.
- The platform observation model must distinguish full-screen from ordinary maximized state.
