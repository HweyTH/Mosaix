# ADR 0011: Manual zone placement session-floats under automatic tiling

**Status:** Accepted
**Date:** 2026-08-31

## Context

The existing zone-snap hotkeys and drag-to-snap interaction promise that a window stays in the selected zone. Automatic tiling continuously recomputes placements for its active tiling set, so leaving a manually placed window tiled would overwrite that promise on the next reflow.

## Decision

While automatic tiling is active, either an explicit zone-snap command or dropping a managed window in an edge snap zone commits the requested zone placement and makes the window session-floating. The remaining active tiling set reflows around it. A drag that ends outside an edge snap zone does not adopt arbitrary geometry; the window returns to its balanced-grid cell. `toggle-floating` explicitly returns a session-floating window to tiling, and session-floating state resets on agent restart unless a persistent rule also makes the window float.

## Alternatives considered

- **Leave manually placed windows tiled:** rejected because the next reflow would silently undo the requested placement.
- **Float after any manual drag:** rejected because incidental movement would remove windows from the grid without a clear commit signal.
- **Interpret dragging as grid reordering:** deferred because it requires a separate grid-target interaction and does not preserve the existing zone-placement contract.

## Consequences

- Manual snapping and automatic tiling have a single, deterministic placement owner after the command or drop.
- Users get immediate keyboard and mouse escape hatches from the grid without creating a persistent rule.
- Non-edge drags cannot preserve arbitrary geometry while a window remains tiled.
