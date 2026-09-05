# ADR 0017: Record observed window bounds separately from intended placement

**Status:** Accepted
**Date:** 2026-09-02

## Context

`WindowPlacement::bounds` holds the placement Mosaix last *intended* for a window, and `Event::WindowBoundsObserved` compares an incoming observation against it to decide whether something other than Mosaix moved the window -- a mismatch is what resets cycle state (ADR 0001). Keeping `bounds` stale is therefore load-bearing, not an oversight.

The consequence is that nothing recorded where a window actually ended up. The observation was read for the comparison and discarded. For a window Mosaix owns the position of this rarely matters, since a reflow puts it back. It matters for exactly the windows Mosaix deliberately does not place: `Float`-ruled and session-floating windows, which their own application or a native OS snap can move freely.

The focus border (issue #22) surfaced this. Drawing an outline from `bounds` leaves it behind any window something else repositioned -- precisely the floating windows CONTEXT.md's "Focus border" says the border must cover.

## Decision

`WindowPlacement` gains `observed_bounds`, written on every `WindowBoundsObserved` and initialised to the intended bounds wherever a placement is created. `bounds` keeps its existing meaning untouched.

This makes explicit in state the expected-vs-actual distinction architecture doc section 8.3 already commits to. Readers that need to know where a window *is* -- anything drawing on or around it -- read `observed_bounds`; the correlation machinery keeps reading `bounds`.

`StateReader::revision()` is added alongside it, reading the revision without cloning the state, so a reader that polls to find out *whether* anything changed does not deep-clone every window, display, and rule to answer that question.

## Considered Options

- **Overwrite `bounds` on observation.** The obvious one-line fix, and wrong: it makes every subsequent observation match, so ADR 0001's external-move detection never fires and snap cycling silently breaks. It would have fixed the border and broken a shipped feature.

- **Read `inventory[id].window.bounds` instead.** Fresher than `bounds` for externally-moved windows, but only as fresh as the last full enumeration, and it is not refreshed by a placement -- so it is stale in the other direction immediately after a snap. Trading one staleness for another.

- **Ask the platform for the window's current rectangle at draw time.** Correct but puts a blocking Win32 call in a polling loop and pushes platform knowledge into what should be pure policy over engine state.

## Consequences

`Event::WindowBoundsObserved` now bumps the revision whenever observed bounds actually change, where it previously bumped only on a cycle-step reset. A window that really moved is a real change and watchers must be able to see it. An observation that merely echoes Mosaix's own placement still bumps nothing, so the common case stays quiet.

This supersedes the assertion in `apply_bounds_observed_mismatch_with_no_cycle_step_does_not_bump_the_revision`, which held that such a move "is not a real change". Under this ADR it is one, and the test was rewritten rather than the behaviour weakened.

The focus border reads `observed_bounds`, and gained two further conditions that its original implementation lacked: it hides for a focused window that is minimized, hidden, or cloaked -- those stay in `inventory` as temporarily ineligible rather than unmanaged, so membership alone would leave a border painted on bare desktop -- and for a window mid-drag, whose placement the engine defers (ADR 0016). Maximized and full-screen windows are ineligible for a grid cell but visible, and keep their border.

Any future feature that draws on or near a window -- a focus animation, a drag ghost, a per-window badge -- now has a correct source to draw from, rather than rediscovering this trap.
