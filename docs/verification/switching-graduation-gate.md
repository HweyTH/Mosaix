# Cross-platform workspace-switching graduation gate

Date: 2026-09-06
Issue: #63 (parent #45)
Builds on: #59 (`windows-parking-prototype.md`), #60/#61
(`windows-workspace-switching.md`), and #62, which brought the macOS
adapter to parity.

## Decision

**Switching does not graduate. It remains experimental.**

Not because a matrix row failed, but because **neither matrix has been
run**. Spec #45 is explicit that unexplained partials do not pass, and an
unrun matrix is the emptiest partial there is. The safety contract below
and the two matrices are the gate; filling them in is the remaining work
of this ticket, and it needs a Windows machine and a macOS machine whose
sessions can be disturbed.

What *is* complete is everything the gate needs in order to be run and
judged: the settings surface, the machine- and human-readable status, the
honest user-facing description, and the automated evidence. Those are
recorded below.

Per spec #45 and this ticket's own acceptance criteria, the consequence of
not graduating is that switching stays clearly experimental. It has **not**
been removed, and neither its activation surface nor anything beneath it
has been touched: SQLite durability, logical workspace identities, and
container-tree tiling are all independent of this decision (ADR 0023).

## The safety contract

A platform matrix passes only if **every** clause holds for **every** row
run on it. These are drawn from spec #45's implementation decisions, not
invented here.

1. **No window is ever lost.** After any row -- including crash, force
   kill, and uninstall -- every managed window is either on screen or
   recoverable by a documented command, and the row records which.
2. **Recovery precedes risk.** The recovery ledger is durable before the
   first native move of any switch. No park happens while persistence is
   degraded.
3. **Public APIs only.** No undocumented interface, no cloaking, no
   private virtual-desktop or Spaces control, no injection, no weakened
   SIP, and no `SW_HIDE` or minimize used as a hiding fallback.
4. **All-or-nothing.** A failed switch is cancelled and every completed
   move compensated. Two workspaces are never intentionally left mixed.
5. **A failed compensation is loud.** It publishes
   `workspace-switch-degraded`, blocks further switching, and names the
   repair. It is never conflated with `persistence-degraded` or with
   ordinary degraded tiling.
6. **Out-of-process recovery works.** `mosaix restore-windows` puts back
   verified windows with no agent running, and refuses to touch a stale,
   reused, or ambiguous handle.
7. **Show state is preserved, never forced.** Full-screen refuses
   preflight; maximized round-trips through normal bounds; minimized is
   left alone.
8. **Focus is never stolen.** The foreground changes only to the target
   workspace's last-focused window.
9. **The description matches the behaviour.** Nothing in the product calls
   this native virtual desktops or Spaces.

Clauses 1-9 are testable per row. A row that cannot be run for
environmental reasons is recorded as **unavailable** with the reason --
never as a pass.

## Automated verification

Run on macOS 15 (Darwin 25.3.0), commit at branch point `e2fca8a`.

| Area | Result | Evidence |
| --- | --- | --- |
| Workspace tests | Pass | `cargo test --workspace`: 820 passed, 0 failed |
| Formatting | Pass | `cargo fmt --all --check` |
| Lint, changed crates | Pass | `cargo clippy -p mosaix-ipc -p mosaix-cli --all-targets -- -D warnings` |
| Lint, whole workspace | **Fails, pre-existing** | `cargo clippy --workspace --all-targets -- -D warnings` reports dead code in `mosaix-agent` (`DirectoryConfigStore`, `PlatformHotkeyProbe`) and `mosaix-settings` (`format_color`, `summarize`, three `AgentError` variants). All are constructed only under `#[cfg(windows)]`, so they read as dead on macOS. Verified pre-existing by stashing this branch's changes and re-running: identical output. Not introduced or worsened here. |
| Settings TypeScript | Pass | `npx tsc --noEmit` in `apps/mosaix-settings` |
| Settings UI tests | **Could not run** | `npx vitest run` in `apps/mosaix-settings` fails every worker with "Timeout waiting for worker to respond". The cause is that importing `jsdom@29.1.1` takes **about 31 minutes** on this machine (measured: 1,836,386 ms for a bare `node -e "import('jsdom')"`, which does eventually succeed), far beyond vitest's worker-startup timeout. Pre-existing and unrelated to this ticket -- it reproduces on the untouched `startup-error.test.ts`, on Node 22 and Node 25, and under both the `forks` and `threads` pools; the same file starts its worker in seconds under `--environment=node`, failing only for want of a DOM. The frontend module's pure functions were verified directly instead (see below). |

The status surfaces this ticket adds are covered where they are built:

| Property | Evidence |
| --- | --- |
| Parking capability is published at the top level, not only inside the switching section | `a_refused_parking_site_keeps_its_reason_at_the_top_level` (`mosaix-ipc`) |
| A refused site keeps the adapter's reason, which the switching section flattens away | The same test |
| Constraint overflow is rolled up across every display | `constraint_overflow_is_rolled_up_across_every_display` (`mosaix-ipc`) |
| A healthy agent asks nothing of the user | `a_healthy_agent_has_nothing_waiting_on_a_person` (`mosaix-ipc`) |
| A degraded switch asks for the reconcile that unblocks switching | `a_degraded_switch_asks_for_the_reconcile_that_unblocks_switching` (`mosaix-ipc`) |
| A failed *park* is not a repair; a failed *restore* is | `a_failed_park_asks_for_nothing_but_a_failed_restore_does` (`mosaix-ipc`) |
| A previous session's unrestored entries ask for the out-of-process restore | `startup_recovery_that_left_a_window_parked_asks_for_the_out_of_process_restore` (`mosaix-ipc`) |
| Repairs are ordered by how much is at stake while they wait | `outstanding_repairs_are_listed_by_how_much_is_at_stake_while_they_wait` (`mosaix-ipc`) |
| Status renders all ten facts spec #45 requires | `status_reports_every_fact_a_release_decision_rests_on` (`mosaix-cli`) |
| Status never calls parking a native virtual desktop | `status_never_calls_parking_a_native_virtual_desktop` (`mosaix-cli`) |
| A refused site is rendered with the adapter's reason | `a_refused_parking_site_is_reported_with_the_adapters_reason` (`mosaix-cli`) |
| Each repair is named with the command that performs it | `status_names_each_repair_and_the_command_that_performs_it` (`mosaix-cli`) |
| A blocked undo says the transaction is kept to retry | `a_blocked_undo_says_the_transaction_is_kept_to_retry` (`mosaix-cli`) |
| Status leads with the condition every other client leads with | `status_leads_with_the_condition_every_other_client_leads_with` (`mosaix-cli`) |
| Settings names the profile that activates switching | `workspace_status_names_the_profile_that_activates_switching` (`mosaix-settings`) |
| Settings reports a complete mapping as complete | `workspace_status_reports_a_mapping_that_covers_every_display_as_complete` (`mosaix-settings`) |
| A mapping missing a display is not reported complete | `a_mapping_missing_a_display_is_not_reported_complete` (`mosaix-settings`) |
| Settings carries the reason a site was refused | `workspace_status_carries_the_reason_a_parking_site_was_refused` (`mosaix-settings`) |
| Settings lists the repair waiting on the user | `workspace_status_lists_the_repair_waiting_on_the_user` (`mosaix-settings`) |
| A repair asks the agent and reports only what it confirmed | `a_repair_asks_the_agent_and_reports_only_what_it_answered` (`mosaix-settings`) |
| A repair the agent could not confirm is an error, not a claim | `a_repair_the_agent_could_not_confirm_is_an_error_not_a_claim` (`mosaix-settings`) |

The settings frontend panel is covered by `src/workspace-status.test.ts`,
which the broken `jsdom` install prevents running here. Its pure functions
-- the four-state activation sentence, the parking description, the
mapping completeness notice, the repair descriptions, and the rendered
panel -- were verified by executing the bundled module directly under
`node` with equivalent assertions, all of which passed. That is weaker
evidence than the suite and is recorded as such: **the settings frontend
suite is unrun on this machine.**

## Windows matrix

Not run. Every row is **pending**.

Environment to record when run: Windows build, display topology
(resolutions, scaling, arrangement), and the profile used to activate
switching.

### Lifecycle and recovery

| # | Row | Result | Evidence |
| --- | --- | --- | --- |
| W1 | Graceful exit with a workspace hidden | Pending | |
| W2 | Agent crash with a workspace hidden | Pending | |
| W3 | Force kill (`taskkill /F`) with a workspace hidden | Pending | |
| W4 | Upgrade over a running agent | Pending | |
| W5 | Disable switching while a workspace is hidden | Pending | |
| W6 | Uninstall with windows parked | Pending | |
| W7 | `mosaix restore-windows` with no agent running | Pending | |
| W8 | `mosaix restore-windows` against a stale handle | Pending | |
| W9 | `mosaix restore-windows` against a reused handle | Pending | |
| W10 | Sleep and wake with a workspace hidden | Pending | |
| W11 | Monitor disconnect while its workspace is displayed | Pending | |
| W12 | Monitor reconnect (must reveal nothing on its own) | Pending | |
| W13 | Resolution change with windows parked | Pending | |
| W14 | DPI/scaling change with windows parked | Pending | |
| W15 | Monitor rearrangement with windows parked | Pending | |
| W16 | A topology offering no parking site | Pending | |
| W17 | Failed compensation, then `workspace restore-switch` | Pending | |

### Task switching, focus, and show states

| # | Row | Result | Evidence |
| --- | --- | --- | --- |
| W18 | Taskbar entries for parked windows | Pending | |
| W19 | Alt-Tab entries for parked windows | Pending | |
| W20 | Task View behaviour with windows parked | Pending | |
| W21 | Activating a parked window from the taskbar | Pending | |
| W22 | Foreground never changes except to the target's last-focused window | Pending | |
| W23 | Application self-activation while its workspace is hidden | Pending | |
| W24 | Modal dialogs owned by a parked window | Pending | |
| W25 | Owned and transient windows follow their owner | Pending | |
| W26 | Minimized member stays minimized and unmoved | Pending | |
| W27 | Minimized member restored by its application while hidden (flashing expected) | Pending | |
| W28 | Maximized member round-trips through normal bounds | Pending | |
| W29 | Full-screen member refuses preflight, nothing moves | Pending | |

### Applications

Each under rapid repeated switching. Record any application that is not
available rather than leaving the row blank.

| # | Application class | Result | Evidence |
| --- | --- | --- | --- |
| W30 | Native Win32 (Notepad, Explorer) | Pending | |
| W31 | Chromium browser | Pending | |
| W32 | Electron application | Pending | |
| W33 | IDE (JetBrains or Visual Studio) | Pending | |
| W34 | Terminal | Pending | |
| W35 | Office (Word, Excel) | Pending | |
| W36 | Media player | Pending | |
| W37 | Multi-window application (several top-level windows) | Pending | |

## macOS matrix

Not run. Every row is **pending**. The rows mirror the Windows matrix so
the two are judged against the same contract; the platform-specific ones
are named as such.

Environment to record when run: macOS version, display topology, whether
"Displays have separate Spaces" is on, and the Accessibility and Screen
Recording permissions granted.

### Lifecycle and recovery

| # | Row | Result | Evidence |
| --- | --- | --- | --- |
| M1 | Graceful quit with a workspace hidden | Pending | |
| M2 | Agent crash with a workspace hidden | Pending | |
| M3 | Force kill (`kill -9`) with a workspace hidden | Pending | |
| M4 | Upgrade over a running agent | Pending | |
| M5 | Disable switching while a workspace is hidden | Pending | |
| M6 | Uninstall with windows parked | Pending | |
| M7 | `mosaix restore-windows` with no agent running | Pending | |
| M8 | `mosaix restore-windows` against a stale AX element | Pending | |
| M9 | `mosaix restore-windows` against an ambiguous match (ADR 0030) | Pending | |
| M10 | Sleep and wake with a workspace hidden | Pending | |
| M11 | Display disconnect while its workspace is displayed | Pending | |
| M12 | Display reconnect (must reveal nothing on its own) | Pending | |
| M13 | Resolution change with windows parked | Pending | |
| M14 | Retina/non-Retina scaling change with windows parked | Pending | |
| M15 | Display rearrangement with windows parked | Pending | |
| M16 | A topology offering no parking site | Pending | |
| M17 | Failed compensation, then `workspace restore-switch` | Pending | |
| M18 | Accessibility permission revoked mid-session with windows parked | Pending | |

### Task switching, focus, and show states

| # | Row | Result | Evidence |
| --- | --- | --- | --- |
| M19 | Dock entries for applications with parked windows | Pending | |
| M20 | Command-Tab entries for applications with parked windows | Pending | |
| M21 | Mission Control behaviour with windows parked | Pending | |
| M22 | Activating a parked window from the Dock | Pending | |
| M23 | Foreground never changes except to the target's last-focused window | Pending | |
| M24 | Application self-activation while its workspace is hidden | Pending | |
| M25 | Sheets and modal dialogs owned by a parked window | Pending | |
| M26 | Owned and transient windows follow their owner | Pending | |
| M27 | Minimized member stays minimized and unmoved | Pending | |
| M28 | Minimized member restored by its application while hidden | Pending | |
| M29 | Zoomed member round-trips through normal bounds | Pending | |
| M30 | Full-screen member refuses preflight, nothing moves | Pending | |
| M31 | Native Spaces in use alongside Mosaix | Pending | |

### Applications

| # | Application class | Result | Evidence |
| --- | --- | --- | --- |
| M32 | Native AppKit (TextEdit, Finder) | Pending | |
| M33 | Chromium browser | Pending | |
| M34 | Electron application | Pending | |
| M35 | IDE (JetBrains or Xcode) | Pending | |
| M36 | Terminal | Pending | |
| M37 | Office | Pending | |
| M38 | Media player | Pending | |
| M39 | Multi-window application | Pending | |

## How to run a row

1. Declare two workspaces and a profile mapping for the current topology.
2. Start the agent and confirm `mosaix workspace switching` reports
   `experimental`. If it reports anything else, that is the row's result.
3. Record `mosaix status --json` before the row.
4. Perform the row.
5. Record `mosaix status --json` after it, and where every managed window
   actually is.
6. Judge the row against all nine contract clauses, not only the one it
   was aimed at.

Evidence for a row means enough to reproduce it: the two status captures,
the applications and their versions, and the observed window positions. A
row asserted without them does not count.

## What graduation would change

If both matrices pass, switching stops being described as experimental,
and this document records the evidence. Nothing else changes: the
mechanism is already what it will be, and the profile-only activation rule
stays either way.

If either platform cannot pass, switching stays experimental or its
activation surface is removed. Removing it would mean the profile key no
longer activates anything; it would **not** mean removing SQLite
durability, logical workspace identities, or container-tree tiling, all of
which stand on their own (ADR 0023).
