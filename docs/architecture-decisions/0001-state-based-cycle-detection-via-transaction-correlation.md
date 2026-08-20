# State-based repeat-cycle detection via placement-transaction correlation

**Status:** accepted

Repeatedly pressing a horizontal snap command (snap-left, snap-right) cycles the focused window through half -> third -> two-thirds -> back to half (`mosaix-layout`'s `HalfZone`/`ThirdZone`; vertical directions stay single-shot since `ThirdZone` only divides width, with no vertical-thirds equivalent to cycle into). Detecting "is this a repeat of the last snap, or a fresh snap" is state-based: the engine tracks `(last zone command, step index)` per window alongside the existing `WindowPlacement` struct. That state resets to step 1 the moment a `WindowBoundsChanged`/`LocationChanged` OS event arrives for the window that doesn't correlate to Mosaix's own last placement transaction (ARCHITECTURE.md section 8.3) -- i.e. something else moved or resized it.

## Considered Options

- **Bounds comparison** (Rectangle's approach): compare the window's live bounds to what the last snap step produced; treat a mismatch as an external move. Rejected -- exact-equality bounds comparison is fragile under DPI/rounding, and Rectangle itself only needed an epsilon in one gap-aware corner-cycle path, meaning even its own authors found exact equality insufficient outside the plain half/third case.
- **Mouse-up invalidation** (Loop's approach): reset cycle state only on mouse-up after a manual drag. Rejected -- misses non-drag external moves (a native OS snap, another app repositioning the window via its own API).
- **Placement-transaction correlation** (chosen): reuse the expected-vs-actual transaction correlation ARCHITECTURE.md section 8.3 already commits Mosaix to building for placement in general, rather than inventing a second, less precise invalidation path specific to cycling.

## Consequences

This makes cycling depend on the transaction-correlation mechanism existing first, and it is not yet built: `mosaix-engine` currently only handles `Event::WindowPlaced`/`WindowRestoreRequested`/`WindowThrowToDisplayRequested`, with no bounds-changed handling at all, and `mosaix-platform-windows`'s `LocationChanged` OS event (which already fires for both programmatic and interactive moves -- see `events.rs`) isn't wired into the engine yet. This was chosen deliberately over a cruder heuristic (e.g. reset-on-focus-change only) to avoid a later rewrite once transaction correlation lands anyway for other reasons (reconciliation, restore-across-external-changes, etc.).

See `docs/research/cycle-and-hotkeys.md` for the primary-source comparison of Loop, Rectangle, and other prior art this decision is based on.
