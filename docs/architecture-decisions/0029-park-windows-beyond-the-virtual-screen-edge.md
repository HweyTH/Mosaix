# ADR 0029: Park windows beyond a validated virtual-screen edge

**Status:** Accepted
**Date:** 2026-09-04

## Context

ADR 0023 committed experimental workspace switching to public-API window parking and left the mechanism to a live Windows prototype (issue #59). The research in `docs/research/workspace-switching-mechanisms.md` found two public-API shapes in production use: GlazeWM and AeroSpace push a window into a monitor corner with a one-pixel sliver left visible, and komorebi offers minimize as a documented but degraded alternative. Neither cloaking nor `SW_HIDE` is available to Mosaix: the first is undocumented and the second is deprecated by both maintainers who shipped it.

The prototype had to choose where a parked window goes, how the adapter proves that place is safe for the current topology, and what happens to maximized and minimized windows, whose show state the shell manages.

## Decision

A parked window is moved wholly outside every connected display, to a *parking site* beyond one edge of the virtual screen: the bounding box of every display, plus a 256-pixel margin so a frame or shadow cannot straddle the edge. Nothing else about the window changes. Its show state, styles, owner, z-order slot, taskbar button, and Alt-Tab entry are untouched, and no window is ever minimized, hidden, or cloaked as a fallback.

The adapter validates the site in two stages that must agree. Pure geometry over the reported displays proposes candidate edges in a fixed preference order (right, bottom, left, top), rejecting any whose 4096-pixel probe block overlaps a display or leaves the 16-bit coordinate range window messages carry. Each surviving candidate is then confirmed against the live desktop with `MonitorFromRect`; the first the desktop agrees with is the site. When none survives, the adapter reports parking as refused with the reason, the engine authorises no parking, and no window moves. The site is re-validated on every topology change and after wake, because a display connected beyond the chosen edge would make parked windows visible.

Parking never activates a window. A normal window moves with `SetWindowPos` and `SWP_NOACTIVATE`. A maximized window is first taken to its normal size where it stands with `SetWindowPlacement` and `SW_SHOWNOACTIVATE`, then moved like a normal window; its maximized state is in the recovery ledger for the way back. Handing `SetWindowPlacement` an off-screen normal position does not work, because the shell adjusts the position to keep the window reachable. A minimized window is left minimized and unmoved: it occupies no screen, and restoring it to park it would change a state the user chose. After every park the adapter verifies with `MonitorFromWindow` that no monitor covers the window, and reports the park as failed otherwise.

## Alternatives considered

- **Corner parking with a visible sliver (GlazeWM, AeroSpace):** rejected for Windows because the sliver is a visible artifact, it needs a per-monitor free corner, and Mosaix already has a stronger recovery story than a draggable pixel: the ledger and `mosaix restore-windows`.
- **Minimize as the hiding mechanism:** rejected because it is user-visible state that applications observe and react to, it changes taskbar semantics, and komorebi documents it as unreliable under frequent switching.
- **Un-maximize by moving the normal position off-screen in one `SetWindowPlacement` call:** rejected because the shell clamps the position back onto a monitor; the prototype measured this.
- **Validate the site from reported geometry alone:** rejected because a display the enumeration missed would leave a parked window visible; the live `MonitorFromRect` check is what catches it.
- **Restore a minimized window in order to park it:** rejected because it forces a show-state change the user did not ask for and would flash the window.

## Consequences

- A parked window keeps its taskbar button and Alt-Tab entry. Task View and Alt-Tab therefore still list windows of hidden workspaces; selecting one there brings a parked window to the foreground off-screen, which the lifecycle-hardening ticket (#61) must reconcile.
- The adapter reports `ParkingCapability` as verified or refused with a reason on every topology; the engine treats anything but verified as a refusal to park.
- Topologies that enclose the virtual screen on all four sides, or that reach the coordinate limit, cannot use experimental switching and say so.
- `mosaix workspace park` and `mosaix workspace restore` exist as the experimental command surface the prototype and later diagnostics use; they go through the same ledger-first authorisation as a workspace switch.
