# ADR 0028: Logical workspaces use a global explicit lifecycle

**Status:** Accepted
**Date:** 2026-09-03

## Context

Once logical workspace switching was retained as an experiment, Mosaix needed to choose whether monitors own independent workspace lists or share identities, how workspace names come into existence, and whether focus, topology changes, or configuration may silently move or create them. These choices affect persistent trees, rules, multi-monitor predictability, and recovery.

## Decision

Logical workspaces form one global pool of unique names. Every tiled or floating managed window belongs to exactly one workspace; excluded windows belong to none. A workspace owns one container-tree root, appears on at most one monitor, and may move between monitors without changing identity.

Configuration or an explicit `workspace create` command creates a workspace. Focus, move, and rule targets must resolve an existing name and return a typed error rather than create one implicitly. Deletion is allowed only for an undisplayed workspace with no live or dormant leaves.

Focusing a workspace already displayed on another monitor transfers focus to its last-focused live window there; it does not move the workspace. Cross-monitor workspace movement is a separate command. A disconnected monitor's workspace becomes hidden and safely parked without displacing workspaces on surviving monitors, and reconnection does not reveal it without an explicit command or resolved topology-profile preference.

Experimental switching is enabled only by a matched topology profile. The resolved profile must map a distinct workspace to every active monitor, and the complete multi-monitor mapping applies atomically or not at all. The engine never invents workspace names to satisfy an invalid mapping.

## Alternatives considered

- **Independent workspace lists per monitor:** rejected because `dev` would become a different identity on each display and could not move cleanly between laptop and docked layouts.
- **Create unknown names on focus or rule evaluation:** rejected because a typo would silently create persistent state.
- **Move an already displayed workspace to the command's current monitor:** rejected because an ordinary focus command would unexpectedly rearrange two displays.
- **Move a disconnected workspace onto a surviving display:** rejected because it would displace that display's current workspace during a topology event.
- **Apply profile mappings display by display:** rejected because partial application contradicts atomic configuration semantics and can mix two intended workspace sets.

## Consequences

- Workspace identity is topology-independent while display assignment remains profile-specific.
- Rules can safely refer to stable workspace names but cannot provision them accidentally.
- Empty workspaces require the explicit `Focused display` state defined by ADR 0027.
- Settings must validate names, display assignments, and safe deletion before submitting commands.
