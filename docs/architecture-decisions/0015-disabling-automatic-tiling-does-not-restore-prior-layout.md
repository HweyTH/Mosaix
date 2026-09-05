# ADR 0015: Disabling automatic tiling does not restore the prior layout

**Status:** Accepted
**Date:** 2026-08-31

## Context

Activating automatic tiling replaces many window placements at once. Restoring the exact earlier arrangement on suspension, profile deactivation, or topology change would require a persistent multi-window snapshot with identity, staleness, partial-failure, and cross-topology semantics that overlap the later saved-scenes feature.

## Decision

When a tiling-enabled profile becomes active at startup, after config reload, or after a display-topology change, Mosaix performs one reconciliation and grid reflow of all eligible windows. When automatic tiling becomes inactive or suspended, Mosaix stops grid reflow and leaves windows at their current bounds; it does not restore pre-tiling geometry.

## Alternatives considered

- **Restore every pre-tiling placement:** deferred because reliable multi-window restoration requires identity and partial-success semantics outside the Balanced grid MVP.
- **Wait for an explicit rearrange after activation:** rejected because a profile that opts into automatic tiling should take effect predictably without another command.

## Consequences

- Enabling automatic tiling has an immediate, visible effect.
- Suspending or leaving a tiling-enabled topology is not an undo operation.
- Users can manually snap or move the retained geometry after deactivation; saved arrangement restoration remains separate future work.
