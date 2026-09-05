# ADR 0027: Explicit focused-display state supports empty workspaces

**Status:** Accepted
**Date:** 2026-09-03

## Context

ADR 0020 rejected explicit focused-display state while workspaces were deferred because every display-scoped command could derive its target from a focused managed window. Logical workspaces reintroduce the case ADR 0020 named: a displayed workspace may be empty while its monitor remains the intended target.

## Decision

The engine tracks `Focused display` explicitly. Focusing a managed window updates it to that window's display, and explicit display-targeting commands update it directly. When no managed window is focused, the state retains the last explicitly targeted display, allowing workspace and layout commands to target an empty displayed workspace.

If the focused display disconnects, choose the nearest surviving display using the existing display-migration geometry, with the primary display breaking an exact tie. If no usable topology exists, display-scoped commands refuse until reconciliation supplies one.

This supersedes ADR 0020. Display-scoped commands use `Focused display`; they no longer universally reject merely because no managed window is focused.

## Alternatives considered

- **Continue deriving the target from focused window:** rejected because an empty displayed workspace has no such window.
- **Use the cursor's display:** rejected because pointer position drifts independently of the user's explicit workspace target.
- **Always use the primary display:** rejected because it would silently redirect commands away from the display the user last targeted.
- **Retain a disconnected display target:** rejected because commands could mutate invisible state associated with hardware no longer present.

## Consequences

- Engine snapshots, IPC, CLI state, and relevant UI surfaces must expose focused display independently from focused window.
- Empty workspaces remain addressable without inventing a placeholder window.
- Display disconnection updates focused-display state in the same reconciliation pass that performs display migration.
