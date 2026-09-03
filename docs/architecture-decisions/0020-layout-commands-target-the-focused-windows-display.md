# ADR 0020: Layout commands target the focused window's display

**Status:** Accepted
**Date:** 2026-09-02

## Context

Applying a saved layout needs a target display, since a layout describes cells on one display (ADR 0018). Every display-scoped command Mosaix already ships -- directional focus, directional swap -- is defined in `CONTEXT.md` as display-local and derives its display from the focused window.

An earlier draft of this design introduced explicit focused-display state, because per-display workspaces could leave a display active but empty, with no focused window to derive a target from. Workspaces are deferred, so that case no longer exists: there is no way to make a display hold zero windows while remaining the command target.

## Decision

Layout commands target the display of the focused managed window, matching directional focus and directional swap. Mosaix does not track focused display as separate state.

When no managed window is focused -- focus rests on an excluded window, on the desktop, or nowhere -- the command is rejected with an explicit error naming that reason. It does not fall back to the cursor's display, the primary display, or the last-known target.

## Alternatives considered

- **Explicit focused-display state:** rejected as unnecessary once workspaces were deferred. It would add state to keep correct across display disconnection and would need its own visual affordance, since focused display and focused window could disagree with nothing on screen to show it.
- **The display under the cursor:** rejected because the cursor drifts independently of attention, so a layout could land on a display the user is not working on, and because it would require mouse tracking the agent does not otherwise need.
- **Falling back to the primary display when nothing is focused:** rejected as silent misbehavior -- the command would rearrange a display the user did not indicate, and would look like it worked.
- **Applying a layout to every display at once:** rejected because it contradicts the display-local guarantee the existing commands make, and because a layout's cells are sized to one display's work area.

## Consequences

- No new engine state, and layout commands behave like the display-local commands users already know.
- Applying a layout while focus sits on the settings window or the desktop fails loudly and does nothing, which is correct but must be surfaced in the CLI and the hotkey path rather than swallowed.
- Should workspaces return, focused-display state may need to be reconsidered; this ADR is scoped to the deferral, not a permanent rejection.
