# ADR 0012: Runtime tiling toggle does not edit profiles

**Status:** Accepted
**Date:** 2026-08-31

## Context

A tiling-enabled profile is the persistent declaration that one display topology uses automatic tiling. Users also need an immediate escape hatch from continuous reflow without disabling manual snap commands or unexpectedly rewriting a profile file behind their editor.

## Decision

`toggle-automatic-tiling` creates or clears an automatic-tiling suspension for the current session. The suspension stops grid reflows but leaves manual snapping active and never edits the matched profile. It clears when the agent restarts or the display topology changes, at which point the newly resolved profile is authoritative again.

## Alternatives considered

- **Persist the toggle into the matched profile:** rejected because a runtime command would silently mutate the source configuration and could race with file-based edits or hot reload.
- **Use the existing global pause:** rejected because pause suppresses manual placement commands as well as automatic reflow.
- **Provide no runtime escape hatch:** rejected because users need a fast way to stop continuous management without locating and editing a profile first.

## Consequences

- Persistent activation has one source of truth: profile configuration.
- A suspended topology intentionally resumes its configured behavior after restart or topology change.
- UI and diagnostics must distinguish automatic-tiling suspension from global pause.
