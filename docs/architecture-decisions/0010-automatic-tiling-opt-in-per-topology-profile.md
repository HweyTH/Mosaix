# ADR 0010: Automatic tiling is opt-in per topology profile

**Status:** Accepted
**Date:** 2026-08-31

## Context

Automatic tiling continuously controls every eligible window in its scope, so enabling it unexpectedly would replace Mosaix's existing manual-snapping behavior. The activation boundary could be global, configurable per display, or tied to the existing profile that matches one display topology.

## Decision

The base configuration remains manual-snapping-only and its schema does not accept an automatic-tiling enable field. A matching profile may set `automatic_tiling.enabled = true` to opt its entire display topology into automatic tiling; when enabled, the policy applies to every display in that topology. Profiles do not provide per-display tiling overrides.

## Alternatives considered

- **Global automatic-tiling switch:** rejected because a useful docked setup could force automatic tiling onto an unrelated laptop-only or presentation setup.
- **Per-display activation inside a profile:** rejected for the first release because it adds mixed-mode state and configuration within one topology before there is evidence that the flexibility is needed.

## Consequences

- Switching display topology can intentionally switch between manual snapping and automatic tiling.
- Global automatic tiling cannot be enabled accidentally through base config; unmatched topologies are always manual.
- A tiling-enabled topology has one predictable ownership mode across all displays.
- Users who need manual placement on one display must float or exclude individual windows; they cannot disable the automatic policy for that display in the first release.
