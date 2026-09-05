# ADR 0013: Automatic-tiling MVP ships on Windows first

**Status:** Accepted
**Date:** 2026-08-31

## Context

Mosaix targets Windows and macOS, but the current agent and complete native event, placement, topology, tray, and overlay paths are Windows-specific while `mosaix-platform-macos` is still an empty crate shell. Requiring adapter parity before validating automatic tiling would combine a new policy with a second platform implementation.

## Decision

The automatic-tiling MVP ships on Windows first. Its planner, configuration, reducer state, commands, and behavioral tests remain platform-neutral behind the existing adapter boundary. macOS support is deferred to a separately specified adapter-parity effort using public APIs.

## Alternatives considered

- **Block the MVP on simultaneous Windows and macOS delivery:** rejected because failures in the tiling policy and failures in a new adapter would be difficult to isolate and would delay testing on the mature platform path.
- **Implement automatic tiling directly in the Windows adapter:** rejected because it would prevent later macOS parity and violate the authoritative platform-neutral engine boundary.

## Consequences

- Windows users can validate the policy and interaction model first.
- Shared automatic-tiling behavior must not depend on Win32 types or semantics.
- The first release is not feature-parity complete with the architecture's long-term platform target; macOS requires explicit follow-up work and the common adapter behavioral suite.
