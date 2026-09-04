# Windows workspace switching and hidden-workspace lifecycle

Date: 2026-09-04
Issues: #60, #61 (parent #45)
Builds on: #59 and `docs/verification/windows-parking-prototype.md`, which
proved the parking mechanism ADR 0029 records.

## What is verified here

#59 proved a window can leave the screen and come back. These two tickets
build the transaction that decides *when* it should, and the lifecycle
rules that keep the parking site and the displayed assignment in
agreement. The evidence below is automated: deterministic reducer tests
over the real state machine, real SQLite for the durable parts, and the
real IPC and CLI rendering.

The live run over real applications is **not** part of this record. It is
specified at the end, with the exact steps, so it can be done on a
machine whose session can be disturbed.

## Automated verification

| Area | Result | Evidence |
| --- | --- | --- |
| Workspace tests | Pass | `cargo test --workspace` |
| Lint | Pass | `cargo clippy --workspace --all-targets -- -D warnings` |
| Formatting | Pass | `cargo fmt --all --check` |

### The transaction (#60)

| Property | Evidence |
| --- | --- |
| Nothing moves before preflight passes | `a_switch_parks_the_outgoing_windows_before_the_assignment_changes`: after the command, both windows are pending and no park effect exists |
| The way back is on disk first | The same test: park effects appear only after `RecoveryEntryDurable` |
| The assignment changes last | The same test: `workspaces.displayed()` is unchanged until every move has landed |
| Outgoing leaves before the target arrives | `a_switch_back_restores_the_target_workspaces_parked_windows_and_its_focus` compares effect positions |
| The target's point of attention comes back | The same test asserts the `FocusWindow` effect names the target's last-focused window |
| A switch that moves nothing needs no parking site | `a_switch_that_moves_no_window_needs_no_parking_site` |
| Unauthorised switching refuses and moves nothing | `a_switch_that_would_move_a_window_is_refused_without_authorised_switching` |
| A full-screen member refuses preflight | `switch_preflight_refuses_a_full_screen_member_and_moves_nothing` |
| A minimized member is never parked | `a_minimized_member_stays_minimized_and_is_never_parked_for_a_switch` |
| A degraded database refuses | `a_switch_is_refused_while_the_state_database_is_degraded` |
| One switch at a time | `a_second_switch_is_refused_while_one_is_in_flight` |
| A failed park compensates | `a_failed_park_compensates_and_leaves_the_original_assignment` |
| A failed restore re-parks what it restored | `a_failed_restore_parks_again_what_it_restored_and_compensates` |
| A failed compensation degrades and blocks | `a_failed_compensation_enters_workspace_switch_degraded_and_blocks_switching` |
| The explicit restore path unblocks it | `restoring_the_stranded_windows_unblocks_switching` |
| A window stranded on the visible side is parked, not declared fine | `a_window_stranded_in_plain_sight_is_parked_by_the_reconcile_not_declared_fine` |
| A reconcile that cannot move a window leaves it stranded | `a_failed_reconcile_leaves_the_window_stranded_and_switching_blocked` |
| Preflight refuses a window already awaiting an explicit park | `a_switch_refuses_a_window_already_waiting_on_an_explicit_park` |
| A move cannot interleave with a switch | `a_workspace_move_is_refused_while_a_switch_is_in_flight` |
| One undo transaction covers assignment and placements | `a_committed_switch_records_the_prior_assignment_and_placements_as_one_transaction` |
| Undo switches back as a fresh guarded switch | `undoing_a_switch_switches_back_as_a_fresh_guarded_transaction` |
| Undo refuses when the switch back would be | `undo_is_refused_when_the_switch_back_would_be` |
| Prior assignments survive a restart | `a_switch_keeps_the_displayed_assignment_it_changed_across_a_restart` (real SQLite, migration 6) |
| End to end across a real restart | `the_pool_its_displays_and_a_hidden_workspaces_tree_come_back_after_a_restart` (real engine thread, real database) |
| Typed IPC answers, not transport errors | `reconciling_a_switch_that_is_not_degraded_answers_with_a_typed_result_not_an_error` |
| The degraded condition is published | `a_degraded_switch_is_published_with_the_windows_it_left_behind` |
| The CLI names the way out | `a_switch_that_could_not_be_compensated_tells_the_user_what_to_run`, `a_degraded_switch_leads_the_switching_report_and_names_the_way_out` |

### The lifecycle (#61)

| Property | Evidence |
| --- | --- |
| Disconnect hides the workspace and parks its windows against the survivor | `disconnecting_a_monitor_parks_the_windows_of_the_workspace_it_took_with_it`, which also asserts the recovery entry records `DISPLAY1`, the survivor it migrated to |
| Surviving assignments are untouched | The same test |
| Reconnect reveals nothing on its own | `reconnecting_a_monitor_leaves_a_hidden_workspace_hidden_when_nothing_selects_it` |
| A valid topology profile does select one | `reconnecting_a_monitor_reveals_a_hidden_workspace_only_when_the_profile_maps_it` |
| A rule target is recorded before parking, and steals no focus | `a_rule_sending_a_new_window_to_a_hidden_workspace_parks_it_without_switching_or_focus` |
| It never triggers a switch | The same test asserts the displayed assignment is unchanged |
| A minimized window stays minimized while hidden | `a_minimized_member_stays_unparked_while_hidden_and_parks_when_its_application_restores_it` |
| An application-initiated restore writes recovery state first | The same test |
| A maximized window's show state and normal bounds are recorded | `a_maximized_window_records_its_show_state_and_normal_bounds_before_parking` |
| Full-screen is never forced | `a_full_screen_member_of_a_hidden_workspace_is_never_forced_out_of_full_screen` |
| Stored assignments are reapplied through the ledger | `stored_assignments_are_reapplied_by_parking_through_the_ledger` |
| No site means the window stays visible, with the reason published | `a_window_of_a_hidden_workspace_stays_visible_when_no_parking_site_is_verified` |
| The reconciler never fights a switch | `the_reconciler_stands_aside_while_a_switch_is_in_flight` |
| A half-applied stored assignment is never silent | `a_half_applied_stored_assignment_is_never_silent` |
| A site validated for one topology is stale for the next | `a_site_validated_for_one_topology_is_stale_for_the_next` (`mosaix-platform-windows`) |
| The three conditions stay distinct, in one order | `the_three_health_conditions_are_distinct_facts_in_one_fixed_order`, `a_healthy_agent_reports_no_conditions_at_all` |
| Every client leads with the same one | `the_leading_health_condition_is_the_one_published_state_ordered_first`, `degraded_tiling_leads_only_when_nothing_more_serious_holds` |

The profile gate the whole transaction rests on -- that base config can
never request switching -- is covered where it is enforced, by
`switching_in_base_config_is_rejected_with_where_it_belongs` in
`crates/mosaix-config/src/validate.rs`. That crate is unchanged here, so
this diff adds no config test rather than a redundant one.

Startup recovery preceding identity reconciliation is a fact of ordering
in `crates/mosaix-agent/src/main.rs`: the ledger pass runs after the
engine is spawned and before the persistence worker, the window
observation, or any stored identity is matched. #59 exercised that
ordering live (force kill, then restart).

One ordering fix belongs to #61 rather than to the engine: the agent's
topology forwarder now sends `ParkingCapabilityReported` *before*
`DisplayTopologyChanged`. The topology arm is where a vanished display's
windows are parked, and it has to decide that against the new topology's
site rather than the old one's.

## Not exercised live

Everything below needs a Windows session that can be disturbed, because
it moves real windows off every monitor. None of it has been run for
these two tickets.

1. **A switch over real applications.** Declare two workspaces and a
   profile mapping for the current topology, start the agent, confirm
   `mosaix workspace switching` reports `experimental`, then
   `mosaix workspace focus <other>`. Expected: every window of the
   outgoing workspace leaves every monitor before any of the target's
   comes back, the foreground never changes except to the target's
   last-focused window, and `mosaix state --json` shows the new
   assignment only once `recovery.parked_windows` holds the outgoing set.
2. **Maximized and minimized members** through a switch and back, over
   the applications #59 used (Word and Edge maximized, Notepad
   minimized). Expected: the maximized ones come back maximized at their
   recorded normal size, the minimized one never moves.
3. **A failed park.** Hard to provoke deliberately; the closest is
   parking an application that refuses `SetWindowPos`. Expected:
   compensation puts back whatever had already moved and the displayed
   assignment does not change.
4. **Display disconnect while a workspace is displayed on that monitor**,
   then reconnect. Expected: the workspace hides, its windows park
   against the nearest survivor, surviving assignments are untouched, and
   the reconnect does not reveal it unless the profile maps it back.
5. **Sleep/wake and DPI or resolution change** with windows parked.
   Expected: the site is re-validated and parked windows stay off every
   monitor, or the capability is refused and published as such.
6. **Alt-Tab and Task View while a workspace is hidden.** This is the
   known leakage ADR 0029 recorded: a parked window keeps its taskbar
   button and Alt-Tab entry, so selecting one raises it off screen.
   #61 does not fix that, and it should be measured and written down
   here rather than left implicit.
7. **An owned modal dialog of a parked window**, which #59 measured as
   not following its owner. Still outstanding.

Items 6 and 7 are known limitations of the mechanism rather than
regressions; they are listed so the live run records them rather than
rediscovering them.
