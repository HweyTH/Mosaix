# Automatic tiling Windows MVP verification

Date: 2026-09-01

## Outcome

The implementation for issues #13 through #22 is ready: the workspace tests,
Windows adapter tests, settings tests, production build, and a real-agent smoke
test pass. Issue #23 remains partial because this machine cannot provide the
full hardware and application matrix required by that ticket.

The live run used the real `mosaix-agent.exe` and CLI with two 1920x1080,
100%-scale landscape displays. The secondary display was positioned left of
the primary, so the run exercised a negative virtual-desktop origin. Automatic
tiling was activated through a topology-specific profile with 12 px outer gaps,
6 px inner gaps, and a 3 px focus border.

## Automated verification

| Area | Result | Evidence |
| --- | --- | --- |
| Workspace behavior | Pass | `cargo test --workspace` |
| Production compilation | Pass | `cargo build` |
| Rust linting | Pass | `cargo clippy --workspace --all-targets -- -D warnings` |
| Settings UI behavior | Pass | `npm test` in `apps/mosaix-settings` |
| Settings production bundle | Pass | `npm run build` in `apps/mosaix-settings` |
| Balanced planner | Pass | Exhaustive member counts 0 through 100, containment, no overlap, exact coverage, portrait/landscape, and negative-origin properties |
| Display transitions | Pass | Engine scenarios for transfer, migration, empty observations, work-area changes, wake reconciliation, and multi-display geometry; native watcher translation tests for display-change and resume broadcasts |
| Interactive placement | Pass | Deterministic engine race scenarios plus a Windows accessibility-hook integration test for native move/resize start and end events |
| Failure isolation | Pass | Placement rejection, per-window circuit breaking, accepted-placement recovery, and Rearrange reconciliation scenarios |
| Focus border | Pass | Native window test covers geometry, negative origins, click-through/no-activate styles, visibility, and hide behavior |

## Live real-agent scenarios

| Scenario | Result | Observation |
| --- | --- | --- |
| Startup and profile activation | Pass | Agent reached `active` with two displays and no degraded windows |
| Window inventory and initial grid | Pass | 186 normalized windows were classified; eligible windows received settled placements |
| Focus tracking | Pass | OS foreground handle and engine `focused_window` matched for Notepad |
| Toggle floating | Pass | Notepad changed `eligible -> session_floating -> eligible` and the grid reflowed |
| Manual half-zone snap | Pass | Left-half snap made the target session-floating without degrading other windows |
| Directional focus and swap | Pass | Commands completed against the active grid and advanced settled state without degradation |
| Automatic-tiling suspension | Pass | State changed `active -> suspended -> active` |
| Pause | Pass | State changed to `paused`; a snap command left the revision unchanged; resume returned to `active` |
| Rearrange recovery | Pass | Inventory reconciliation completed, revision advanced, and degraded count remained zero |
| Minimize and restore | Pass | Notepad changed `eligible -> minimized -> eligible` |
| Maximize and restore | Pass | Maximizing changed Notepad to `session_floating`; restoring preserved the session override |
| Focus border | Pass | The persistent `MosaixFocusBorderWindow` was visible while an eligible Notepad window was focused |
| Elevated-window isolation | Pass | Five elevated observations were classified outside the Active tiling set; the settled grid had no degraded windows |
| Open and close | Pass | Temporary Notepad windows entered the authoritative inventory; cleanup removed them without an agent failure |

The first sandboxed live attempt could not enumerate desktop windows because
the sandbox did not have access to the interactive Windows desktop. Repeating
the same run with the agent on the interactive desktop produced a settled grid;
this was an execution-environment limitation, not an application defect.

## Second live run (2026-09-01)

An independent repeat run on the same two-display, negative-origin topology
confirmed the rows above and measured the grid geometry directly. With a
1920x1080 work area of 1032 usable pixels, 12 px outer gaps, and 6 px inner
gaps, the primary display's three cells were placed at x=12 w=622, x=646
w=628, and x=1286 w=622 (right edge exactly 1908), and the secondary
display's two cells at x=-1908 w=942 and x=-954 w=942 (right edge exactly
-12). Every cell was y=12 h=1008. Column-major preference on landscape
displays, exact integer edge allocation with no cumulative overlap, and the
negative virtual-desktop origin are therefore confirmed against real
placements rather than only against planner properties.

The default hotkeys were exercised by synthesizing real keystrokes:
`Ctrl+Alt+T` toggled `active -> suspended -> active`, `Ctrl+Alt+P` toggled
`active -> paused -> active`, `Ctrl+Alt+Space` toggled one window
`eligible -> session_floating -> eligible`, and `Ctrl+Alt+H/L` walked
directional focus across the primary display's three cells with the OS
foreground window and the engine's `focused_window` agreeing at every step
and stopping, non-wrapping, at the display edge. The Focus border window was
verified to carry `WS_EX_LAYERED | WS_EX_TRANSPARENT | WS_EX_TOPMOST |
WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE` and to sit exactly on the focused
window's bounds.

### Defect found and fixed

This run found one real defect that the automated suite did not cover:
`EngineState::focused_window` was only ever written from a foreground-*change*
notification, so a freshly started agent had no focus anchor at all until the
user next switched windows. While automatic tiling was active, that left
directional focus and directional swap as silent no-ops and the Focus border
hidden, contradicting spec #11 user stories 39 and 43 ("the feature works
immediately after profile opt-in").

The fix adds `mosaix_platform_windows::foreground_window_handle()` and has the
agent seed the engine from it during startup reconciliation, as an ordinary
`Event::WindowFocused` observation. Re-running the same scenario from a cold
start now reports the focused window immediately, shows the Focus border on
the correct cell, and makes `Ctrl+Alt+H/L` work on the first keypress.

## Acceptance-matrix limitations

These #23 rows are not claimed as live passes:

- Physical display hotplug, removal migration, taskbar/work-area mutation,
  rotation, portrait orientation, displays above/right of primary, and
  125/150/200% mixed scaling were not available on this hardware. Their policy
  paths are covered by deterministic engine/planner tests, and Windows message
  translation is covered by native adapter tests.
- Actual sleep/resume was not triggered because it would interrupt the test
  host. Resume message translation and settled wake reconciliation are covered
  automatically.
- Pointer-driven edge and non-edge drag were not injected into the user's live
  desktop. Native move/resize event translation and all interactive-placement
  race outcomes are covered automatically.
- Deliberate rejection of a real third-party window was not forced. Automated
  rejection/circuit tests and naturally elevated windows verified isolation,
  but a live degraded-to-Rearrange recovery was not manufactured.
- Office, JetBrains, and a separately controlled browser application were not
  available for this run. Native Notepad, the Electron desktop client, and the
  terminal/CLI path were exercised.
- Tray icon appearance and the settings window were covered by native/unit/UI
  tests rather than visually inspected during the live runs. CLI state,
  focus-border geometry and window styles, and the default global hotkeys
  (`Ctrl+Alt+T`, `Ctrl+Alt+P`, `Ctrl+Alt+Space`, `Ctrl+Alt+H/L`) were driven
  live in the second run.

## Release-readiness decision

The code-level Windows MVP in #11 is implemented and issues #13 through #22
meet their acceptance criteria, including the startup focus-anchor defect found
and fixed during the second live run. The complete #23 release matrix is
**partial**, so #23 remains open until the unavailable hardware/application
rows are run or explicitly accepted as release limitations.
