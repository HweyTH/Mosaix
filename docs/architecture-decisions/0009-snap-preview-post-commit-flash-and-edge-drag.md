# ADR 0009: Snap preview overlay — post-commit flash + edge-triggered drag-to-snap

**Status:** Accepted  
**Date:** 2026-08-27  
**Context:** Feature 34 (Tier 7 minimal visual feedback)

## Context

Mosaix needs a semi-transparent overlay showing where a window will go so users can tell snapping is working. Triggers considered:

1. **Hold-to-preview while a snap hotkey is held** — `RegisterHotKey` / `WM_HOTKEY` (ADR 0002) only reports press (with auto-repeat). There is no key-up event without a low-level hook, which ADR 0002 deliberately avoided.
2. **Predict the target rect in the agent before the engine commits** — duplicates zone-cycle and gap logic already owned by the reducer; drifts when paused, circuit-open, or cycle state differs.
3. **Flash the committed placement after the engine applies a snap** — accurate, zero duplicated layout math, works with hotkey auto-repeat through the zone cycle.
4. **Drag preview** — `EVENT_SYSTEM_MOVESIZESTART` / `END` already fire from the event hooks. Preview-only (no commit on drop) misleads; commit-on-drop is real drag-to-snap.

## Decision

1. **Hotkey path:** After enqueueing `Event::ZoneSnapRequested`, the agent captures the pre-send `EngineState.revision` and asks the overlay controller to flash once `revision` advances. The flash shows the focused window’s committed bounds for ~600 ms. If the event no-ops (paused, no focus, circuit open), revision does not advance and nothing is shown.
2. **Drag path:** While an interactive move/resize is in progress, poll the cursor. If it is within a fixed edge threshold of a display work-area edge, show the matching half-zone (with gaps). On move/resize end, if still in a zone, commit via existing `Event::WindowPlaced` so restore, the placement executor, and the circuit breaker all apply unchanged.
3. **One overlay writer:** A single controller thread owns the layered window and serializes flash vs drag so they never fight. Drag takes precedence over an in-progress flash.
4. **Edge zones only for drag:** Left/right/top/bottom halves within `edge_threshold` px of the work-area edge (horizontal edges win at corners). No full-screen “always preview a half” — that would keep the overlay up for the entire drag.

## Consequences

- Holding a snap hotkey still feels continuous because `RegisterHotKey` auto-repeats and each step flashes the next cycle size.
- Drag-to-snap is limited to half-zones (matching the four hotkey directions); quarters remain hotkey-less and out of drag scope for v1.
- A circuit-breaker-open window may preview on drag but not move (`WindowPlaced` is suppressed); breaker reset remains an explicit zone-snap hotkey only.
- Overlay appearance and thresholds are constants (no config knobs in v1).
