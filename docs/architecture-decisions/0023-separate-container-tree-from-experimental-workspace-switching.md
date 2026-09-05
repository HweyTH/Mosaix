# ADR 0023: Separate container-tree tiling from experimental workspace switching

**Status:** Accepted
**Date:** 2026-09-03

## Context

The original architecture made the workspace and normalized container tree one Phase 3 feature. Research found that the tree itself needs no private OS behavior, while seamless workspace visibility does: Windows cannot switch native virtual desktops through the public `IVirtualDesktopManager` interface, macOS does not expose public control of Spaces, and mature window managers either use undocumented APIs or accept visible and behavioral compromises. See `docs/research/workspace-switching-mechanisms.md` and `docs/research/workspace-feature-strategy.md`.

Mosaix also assigns workspace trees, undo and placement history, scene metadata, migrations, and onboarding state to an unimplemented SQLite database. Switching raises loss of that state from an inconvenience to a recovery hazard because a failed manager can leave windows apparently missing.

## Decision

Mosaix retains the logical workspace and normalized container-tree model but separates their delivery. SQLite and recovery foundations come first, followed by supported container-tree tiling over one visible root per monitor.

Actual named-workspace switching begins only as an explicitly experimental capability. It uses public OS APIs to park non-displayed windows at reversible edge positions, never private APIs, native Windows Virtual Desktop or macOS Spaces control, process injection, or reduced platform security. It requires a current-session recovery ledger and an out-of-process restore command before it may park a window.

The adapter must verify a recoverable parking edge for the active topology before activation or switching. If it cannot, the operation refuses rather than changing mechanism to minimize, hide, or cloak.

Logical workspaces form one global pool of unique names. A workspace is displayed on at most one monitor and may move between monitors without becoming a different workspace; monitors do not own separate workspace lists.

Switching graduates to supported status only after the cross-platform recovery and compatibility matrix in `docs/research/workspace-feature-strategy.md` passes. If it does not pass, switching remains experimental or is removed without removing container-tree tiling.

Startup is recovery-first: verify every prior-session ledger handle against its recorded process instance and window evidence, restore verified windows to visible pre-park geometry, and never touch a stale or reused handle. Only then reconcile durable identities and reapply saved workspace display assignments as fresh guarded transactions.

## Alternatives considered

- **Drop workspaces and the container tree together:** rejected because the tree independently enables BSP, stack, weights, structural commands, and future restoration seams.
- **Use undocumented cloaking or native-desktop APIs:** rejected because this creates permanent OS-version, recovery, security, and distribution obligations that contradict the public-API boundary.
- **Ship public-API parking immediately as supported:** rejected because monitor topology, task-switcher leakage, focus, owned windows, application compatibility, and force-kill recovery are unproven product semantics.
- **Keep workspace switching as a prerequisite for tree tiling:** rejected because it makes the riskiest adapter behavior block a platform-neutral layout capability.

## Consequences

- `CONTEXT.md` no longer promises per-monitor workspaces as a current product capability.
- Phase 3 is split into supported container-tree tiling and a separately gated switching experiment.
- The SQLite state database moves ahead of both tree persistence and switching recovery, and also unblocks later identity-based scene restoration.
- Native desktop or Spaces cooperation may be explored later as an optional adapter capability, but it cannot define the cross-platform workspace contract.
