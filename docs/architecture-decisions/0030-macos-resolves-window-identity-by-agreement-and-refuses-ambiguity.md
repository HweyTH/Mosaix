# ADR 0030: macOS resolves window identity by agreement, and refuses ambiguity

**Status:** Accepted
**Date:** 2026-09-05

## Context

ADR 0023 requires that a parked window be recoverable: the recovery ledger records a native handle plus enough process and window evidence that a later session — or the out-of-process restore command after a crash — can decide whether the handle still names the same window before moving anything.

On Windows this is direct. The handle is an `HWND`, every placement API takes an `HWND`, and `IsWindow`, `GetWindowThreadProcessId`, and `GetClassNameW` answer what it names now. Verification and action use the same handle.

macOS splits the two. The window server identifies a window by a `CGWindowID`, which is what enumeration observes and what the ledger can durably record. But nothing can be *moved* through a `CGWindowID`: placement goes through Accessibility, which addresses windows by `AXUIElement`. **There is no public API that maps a `CGWindowID` to its `AXUIElement`.** The known routes are `_AXUIElementCreateWithRemoteToken` and the `CGS` private window list — precisely the private interfaces ADR 0023 forbids, and which would also add an OS-version obligation the project has refused to take on.

A durable handle is also required in a second place: the out-of-process restore command runs *after* the agent has died, so whatever identifies a window has to survive as a number in SQLite. A retained `AXUIElement` held in an adapter-side registry cannot.

## Decision

The macOS adapter keeps the `CGWindowID` as the durable handle, and resolves the Accessibility element at the moment of action by **agreement between the two views of the same window**.

To act on a handle, the adapter asks the window server what it knows about that window id — owning pid, frame, and name — then asks that process for its Accessibility windows and keeps the ones whose frame agrees within a small sampling tolerance, and whose title agrees when *both* sides can report one. `kCGWindowName` requires Screen Recording permission, which Mosaix does not ask for, so an absent title is treated as absent evidence and never as a mismatch.

**Exactly one survivor is an identification. Everything else is a refusal.** No candidate means the window is gone or unaddressable. More than one means two windows of the same application are indistinguishable through public API. Both return an error and move nothing.

Handle verification for the ledger is separate and stronger. It rests on the process instance — pid paired with the kernel's start time from `proc_pidinfo(PROC_PIDTBSDINFO)` — which is the exact analogue of the Windows `GetProcessTimes` pairing. The macOS probe reports `native_class: None` rather than a class it cannot observe, and `verdict_for` decides on the process instance alone.

## Alternatives considered

- **Private APIs (`_AXUIElementCreateWithRemoteToken`, `CGSCopyWindowsWithOptionsAndTags`):** rejected. ADR 0023 draws the public-API boundary, and #62 restates it. These also break between macOS releases, which is a permanent maintenance obligation on the riskiest code in the product.
- **Make the macOS handle an AX-backed registry key:** rejected as the primary identity. It is exact within a session, but it cannot be written to the ledger as a number, so crash recovery — the case the ledger exists for — would still need a resolution rule. It solves the easy half and leaves the hard half.
- **Match on frame alone:** rejected. Two windows of one application at the same size and position is uncommon but real (a duplicated document window, a freshly opened cascade). Title is free additional evidence whenever it is observable.
- **Match on the best-scoring candidate:** rejected outright. A scored match always returns an answer, and the failure mode is silently moving a window the user did not ask us to touch. Ambiguity here must be loud.
- **Declare parking unsupported on macOS:** viable under #62's own terms, and the honest answer if resolution proves unreliable in the live matrix. Rejected for now because the refusal path makes the failure safe: the worst case is a window that will not park, not a window that moves unexpectedly.

## Consequences

- Parking and restoring on macOS can fail for reasons that have no Windows equivalent, and the capability surface must say so in the user's own words rather than reporting a generic error.
- `MacosError::AmbiguousWindow` is a *normal* outcome, not a bug. It must be counted and reported in the live acceptance matrix (#63), because its frequency is the real measure of whether this approach is good enough to graduate.
- Full-screen windows report no pre-full-screen geometry through public API, so a window parked out of full screen records the frame it had, and the limitation is recorded rather than guessed.
- Re-entering full screen on restore animates and takes the foreground. There is no public way to avoid it — the same class of measured limitation as `SetWindowPlacement` activating on Windows.
- If the macOS live matrix shows ambiguity or resolution failure at a material rate, the fallback is the alternative above: report parking as refused on macOS and leave switching experimental, without removing SQLite durability, logical workspace identities, or container-tree tiling.
