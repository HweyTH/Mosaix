# Windows public-API parking prototype

Date: 2026-09-04
Issue: #59 (parent #45)

## Recommendation

**Proceed** with production Windows switching on the parking mechanism ADR 0029 records: move the window wholly beyond a validated edge of the virtual screen, without activation, after the recovery ledger holds its way back.

Every measured property the ticket asked for held on this machine across eight applications: the window leaves every monitor, the foreground never changes on park or on restore, styles and show state are untouched while parked, geometry and maximized state come back exactly, and every recovery path (out-of-process restore after a force kill, startup recovery after a restart, and the clean-exit pass) put every window back. The mechanism refused the two cases it must refuse, a full-screen window and a topology with no safe edge, without moving anything.

Three limitations are real and belong to the lifecycle-hardening ticket (#61), not to the mechanism: a parked window keeps its taskbar button and Alt-Tab entry, so the task switcher can still raise it off-screen; an owned modal dialog does not follow its parked owner and stays visible; and sleep/wake, display disconnect, and DPI or resolution changes were not exercised live, only through the adapter's topology re-validation code and its tests.

## Environment

- Windows 11 Pro 10.0.26200, two 1920x1080 displays at 100% scale, secondary display left of the primary (virtual screen origin -1920,0, size 3840x1080).
- Verified parking site: the right edge, so parked windows sit at x = 2176 (virtual-screen right edge 1920 plus the 256-pixel margin).
- Real `mosaix-agent.exe` and `mosaix.exe` from a debug build; no topology profile, so automatic tiling stayed off and only the windows the run launched were moved.
- Applications: Google Chrome and Microsoft Edge (Chromium), Visual Studio Code (Electron, also the IDE row), Windows Terminal (terminal), Word and Excel (Office), Notepad (native Win32/WinUI), Calculator (UWP, `ApplicationFrameWindow`). No media application was available in the matrix; that row is recorded as not exercised.

## Automated verification

| Area | Result | Evidence |
| --- | --- | --- |
| Workspace tests | Pass | `cargo test --workspace` |
| Lint | Pass | `cargo clippy --workspace --all-targets -- -D warnings` |
| Site planning without a monitor | Pass | `plan_parking_sites`: side-by-side displays yield all four edges in preference order; a display beyond an edge removes that edge; no displays and a coordinate-limit topology refuse with typed reasons |
| Live site validation | Pass | `find_parking_site` against this machine's displays agrees with `MonitorFromRect`; `parking_capability` reports verified |
| Park without activation | Pass | Real sample window parked with `SWP_NOACTIVATE`; `GetForegroundWindow` unchanged; size untouched; `MonitorFromWindow` null |
| Restore to recorded geometry | Pass | Parked sample window restored by the recovery path to its exact visible bounds |
| Maximized park and restore | Pass | Sample window maximized, parked at normal size (`Unmaximized`), restored maximized |
| Minimized and destroyed handles | Pass | Minimized window left minimized; destroyed handle refused with `InvalidWindow` |
| Engine authorisation | Pass | Refusals for unverified or refused site, unmanaged, full-screen, and minimized windows; ledger acknowledgement gates the effect; failed park leaves the entry unparked; failed restore keeps the window parked; a parked window that closes releases its entry; restore-all emits one effect per parked window |
| IPC and CLI | Pass | `ParkWindow` answers a typed `ParkWindowResult`; `RestoreParkedWindows` answers the windows asked for; the recovery snapshot publishes the last parking failure with its stage |

## Live matrix: park and restore through the command surface

Each row: launch the application, read its geometry, `mosaix workspace park <hwnd>`, wait 1.5 s, measure, `mosaix workspace restore`, wait 1.5 s, measure again. "Foreground" compares `GetForegroundWindow` before and after each step.

| Application | Class | Before | Parked | Off every monitor | Ledger parked | Foreground unchanged (park / restore) | Styles unchanged | Restored exactly | Ledger clear |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Chrome | `Chrome_WidgetWin_1` | 3,10 1500x1050 | 2176,0 1500x1050 | Yes | Yes | Yes / Yes | Yes | Yes | Yes |
| Edge (maximized) | `Chrome_WidgetWin_1` | -1928,-8 1936x1048 | 2176,0 945x1020 | Yes | Yes | Yes / Yes | `WS_MAXIMIZE` cleared while parked, restored | Yes, re-maximized | Yes |
| Visual Studio Code | `Chrome_WidgetWin_1` | 352,140 1216x808 | off right edge | Yes | Yes | Yes / see note | Yes | Yes | Yes |
| Windows Terminal | `CASCADIA_HOSTING_WINDOW_CLASS` | 18,26 1129x635 | off right edge | Yes | Yes | Yes / see note | Yes | Yes | Yes |
| Word (maximized) | `OpusApp` | -8,-8 1936x1048 | 2176,0 1440x753 | Yes | Yes | Yes / see note | `WS_MAXIMIZE` cleared while parked, restored | Yes, re-maximized | Yes |
| Excel | `XLMAIN` | 104,104 1440x753 | 2176,0 1440x753 | Yes | Yes | Yes / Yes | Yes | Yes | Yes |
| Notepad | `Notepad` | 26,26 1440x753 | 2176,0 1440x753 | Yes | Yes | Yes / Yes | Yes | Yes | Yes |
| Calculator | `ApplicationFrameWindow` | 40,32 336x541 | 2176,0 336x541 | Yes | Yes | Yes / Yes | Yes | Yes | Yes |

Note on the first three rows: in the first run the agent had a visible console window, and the foreground after `restore` was that console rather than the window before it. The second run, with the agent's console hidden and the same restore path, left the foreground unchanged for every application including the maximized Edge window, so the change is attributed to the console, not to `SetWindowPlacement`. Production agents run without a console.

Every parked window kept its `WS_VISIBLE` style and its taskbar button; the taskbar screenshot taken while Visual Studio Code, Windows Terminal, and Word were parked shows all three buttons with running indicators. Alt-Tab and Task View were not driven programmatically; because nothing about the window changes except its position, they list parked windows, and selecting one raises it off-screen. That is the leakage ADR 0023 anticipated and #61 must reconcile.

## Live matrix: recovery

| Scenario | Steps | Result |
| --- | --- | --- |
| Force kill, out-of-process restore | Park Notepad and Calculator; `taskkill /F` the agent; confirm both still off every monitor; `mosaix restore-windows --json` | Both entries `Verified` and `restored: true`; both windows back at their exact prior bounds; foreground unchanged; a second `restore-windows` reports `[]` |
| Force kill, restart | Park Notepad; `taskkill /F`; start a new agent | Startup recovery restored the window before observation (`verdict: verified, restored: true` in `mosaix state`); window at its exact prior bounds; `restore-windows --force` afterwards reports `[]` |
| Graceful exit | Park Notepad; send Ctrl+C to the agent | Log: `clean exit restored parked windows restored=1 total=1`; window back at its prior bounds; ledger empty |
| Closed while parked | Engine scenario | Entry released as history; nothing left for a later session to probe |
| Agent console closed by hand mid-run | Incidental during the first pass | Shutdown signal `source="console"`; ledger left empty because everything had already been restored |

## Live matrix: show states, dialogs, refusals

| Scenario | Result |
| --- | --- |
| Maximized (Word, Edge) | Parked at normal size with `SetWindowPlacement` + `SW_SHOWNOACTIVATE` then `SetWindowPos`; restored maximized; no activation. Handing `SetWindowPlacement` an off-screen normal position does not park: the shell adjusts it back onto a monitor. This was measured and is why the park is two steps. |
| Minimized (Notepad) | Left minimized and unmoved. The first run marked such a window parked because the platform reported `LeftMinimized` as success; the engine now refuses a minimized window up front (`minimized` refusal) and the executor reports a window found minimized as a park failure, so its entry is never marked parked. |
| Full-screen (Chrome, F11) | `mosaix workspace park` refused: "window is full-screen and is never forced out of it"; nothing moved. |
| Owned modal dialog (Notepad Save As, `#32770`) | Parking the owner moved the owner only; the dialog stayed at 58,52 960x540, visible and foreground. Restore put the owner back exactly; the dialog never moved. An owned window does not follow its owner, so hiding a workspace that has a modal dialog open leaves the dialog on screen. Deferred to #61. |
| No safe edge | Not reproducible on this topology; covered by the pure planner tests (surrounded and coordinate-limit topologies refuse with typed reasons) and by the engine's refusal path. |

## Not exercised live

- **Sleep/wake, display disconnect, resolution or DPI change, monitor rearrangement.** These need hardware or display-settings changes on the developer machine during the run. The agent re-validates the parking site on every `DisplayTopologyChanged` and `WakeReconciliation` event and re-reports capability; the geometry is unit-tested. A rearrangement that puts a display beyond the parking edge while windows are parked would leave them visible there until the next switch; #61 owns that reconciliation.
- **Media applications.** None installed in the matrix environment.
- **Alt-Tab and Task View.** Reasoned from unchanged styles, not driven.

## Reproducing

1. Build: `cargo build`.
2. Start `target\debug\mosaix-agent.exe` (with `RUST_LOG=info` for the parking log lines).
3. `mosaix workspace switching` reports `parking site: verified`.
4. Launch an application, find its top-level handle, `mosaix workspace park <hwnd>`.
5. `mosaix state --json` lists it under `recovery.parked_windows`; `MonitorFromWindow` returns null for it.
6. `mosaix workspace restore` puts it back; or `taskkill /F` the agent and run `mosaix restore-windows`.
