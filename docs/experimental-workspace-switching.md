# Experimental workspace switching

Mosaix can group managed windows into named **logical workspaces** and show
one of them per monitor. Switching between them is **experimental**. This
page says exactly what the mechanism is, what it cannot do, and how to get
out of trouble, because the honest description is part of the feature
(spec #45, user story 97).

## What it is, and what it is not

Mosaix does **not** create, name, enumerate, or switch Windows Virtual
Desktops or macOS Spaces. It has no control over them, and using one
alongside Mosaix is outside what Mosaix manages.

What it actually does is **emulated switching through public-API window
parking**: when a workspace stops being displayed, Mosaix moves its
windows to a verified position beyond the edge of the virtual screen and
moves them back when the workspace is shown again (ADR 0029). The windows
are never hidden, never cloaked, and never minimized as a substitute for
hiding. They keep their show state, their styles, their taskbar button,
and their Alt-Tab entry, and they are never activated on the way out or
back.

Mosaix uses only public operating-system APIs for this. It does not use
undocumented interfaces, `IApplicationView::SetCloak`, private
virtual-desktop or Spaces control, process or Dock injection, or a
weakened System Integrity Protection. Where a public API cannot do the
job, Mosaix **refuses and says so** rather than reaching for a less safe
mechanism.

## How it is turned on

Only a **matched topology profile** can request switching. Base
configuration cannot, and no command, tray item, or settings control can
enable it — this is deliberate, so that docking a laptop cannot silently
start moving windows off screen.

A profile's mapping must name a distinct workspace for **every** connected
display. A mapping that covers some displays is rejected whole: Mosaix
never invents a workspace name to fill a gap, and never applies half a
mapping.

`mosaix workspace switching` reports one of four states for the current
topology:

| State | Meaning |
| --- | --- |
| `disabled` | No matched profile requests switching. |
| `requested` | A profile requests it and its mapping is in effect, but switching is waiting on a verified parking site or on persistence recovering. |
| `unavailable` | A profile requests it, but its mapping could not be applied, so the previous displayed assignment stands. |
| `experimental` | The mapping is in effect and parking is authorised. |

`mosaix status` shows the same fact alongside everything else a decision
about the experiment rests on, and the settings window shows it in the
**Workspaces** panel.

## Known limitations

These are properties of the mechanism, not defects to be fixed later.

- **Full-screen windows refuse a switch.** If a managed window in the
  outgoing workspace is full-screen, the switch is refused and nothing
  moves. Mosaix will not force an application out of full-screen while
  you are presenting or playing something.
- **Minimized windows stay minimized and stay put.** They are not parked.
  If the application restores one while its workspace is hidden, Mosaix
  records recovery data and then parks it — and that transition **can
  visibly flash**, because the window is briefly restored before it moves.
- **Maximized windows are un-maximized to park.** A maximized window is
  taken to its normal size first and re-maximized when its workspace is
  shown again. If either transition fails, the whole switch is cancelled
  and compensated.
- **A parked window is still a real window.** It stays in the taskbar and
  in Alt-Tab/Command-Tab. Activating it from there will pull it back to
  the visible desktop, outside Mosaix's switch transaction.
- **Some topologies have no parking site.** Mosaix requires a position it
  can verify it can recover from. Where the display arrangement offers
  none, parking capability is `refused` and switching never activates.
- **Reconnecting a monitor reveals nothing on its own.** A workspace is
  displayed again only when a command or a resolved topology profile
  selects it.
- **Nothing is parked while persistence is degraded.** Recovery data has
  to be durable before a window may leave the screen, so a database
  problem suspends parking rather than risking an unrecoverable window.

## If windows do not come back

Every park writes the way back to a recovery ledger **before** the window
moves, so there is always a path to recover — including after a crash or
a force kill.

- **`mosaix workspace restore`** puts back every window the running agent
  parked.
- **`mosaix workspace restore-switch`** reconciles the windows a failed
  switch left unaccounted for. While that condition stands, switching is
  blocked, and this is the only way out of it.
- **`mosaix restore-windows`** works **without the agent running** — this
  is the one to use after a crash, a force kill, or an uninstall. It reads
  the ledger directly and touches only a handle whose live evidence still
  matches what was recorded: same process instance, same window class. A
  stale, reused, or ambiguous handle is reported and left alone rather
  than guessed at.

`mosaix status` names whichever of these applies, under **recovery**. The
settings window's **Workspaces** panel offers the first two as buttons and
says which is needed.

## Graduation

Switching stays experimental until it passes a live acceptance matrix on
**both** Windows and macOS. See
[`verification/switching-graduation-gate.md`](verification/switching-graduation-gate.md)
for the matrix, the safety contract it is judged against, and the current
decision.

Container-tree tiling does **not** depend on this experiment. If switching
never graduates, the tree, logical workspace identities, and durable state
all remain (ADR 0023).
