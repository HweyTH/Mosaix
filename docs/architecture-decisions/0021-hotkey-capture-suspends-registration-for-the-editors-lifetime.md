# ADR 0021: Hotkey capture suspends registration for the editor's lifetime and arms per dialog

**Status:** Accepted
**Date:** 2026-09-02

## Context

Hotkeys are already user-editable by hand (ADR 0005) and hot-reloaded (ADR 0008); what is missing is editing them without opening a TOML file. Capturing a combination by pressing it is blocked by Mosaix's own architecture: `RegisterHotKey` is OS-arbitrated and delivers `WM_HOTKEY` to the registering thread, so a combination Mosaix owns never reaches the focused settings window. ADR 0002 deliberately rejected the low-level keyboard hook that would let the editor observe it, and that reasoning is unchanged -- a hook has no OS-arbitrated conflict concept.

`docs/research/hotkey-binding-foundations.md` found no tiling window manager ships a GUI hotkey editor: GlazeWM's tray is five items, komorebi's `komorebi-shortcuts` is a 98-line read-only list, AeroSpace has a tray menu. The usable precedent is Microsoft PowerToys Keyboard Manager, which ships key capture and solves the same live-registration problem at two distinct scopes.

## Decision

Capture follows PowerToys' two-scope design, adapted to `RegisterHotKey`.

**Coarse scope -- registration suspension, tied to the editor's lifetime.** While the hotkey editor is open, the agent unregisters every binding and re-registers when it closes. This is `HotkeyRegistrations::stop()` plus a fresh `start_hotkeys()`, which config reload already performs, not a new mechanism. Where PowerToys signals a named event that its hook checks and returns `0`, Mosaix genuinely unregisters, because `RegisterHotKey` offers no way to swallow a keystroke.

The suspension is bounded by the settings app's IPC connection, not by a message. The agent enters suspension on a request over a connection the editor holds open, and re-registers when that connection ends for any reason -- close, crash, or kill -- because Windows closes the pipe handle and the server's read fails. There is no timeout to tune and no "exit capture" message that can be lost.

Suspension is tied to the editor window's **lifetime, not its focus**. Re-registering on every blur would churn `RegisterHotKey` on each alt-tab, and every re-registration is an opportunity for another application to have taken a combination in the interim.

**Fine scope -- capture arming, per modal dialog.** The capture buffer is armed only while a single "press your combination" dialog is frontmost, and is cleared when that dialog loses foreground. A capture armed across a whole settings page is a page on which no key does anything.

**Conflict probing.** Before accepting a combination, probe it: `RegisterHotKey(nullptr, id, mods, vk)`, treat `ERROR_HOTKEY_ALREADY_REGISTERED` as taken, `UnregisterHotKey` on success. Classify the result as owned by another Mosaix binding or by the system/another application, attributing an unexplained refusal to the system. A conflicting binding may still be saved deliberately, matching PowerToys' `IgnoreConflict` escape hatch.

**Reserved combinations.** Hardcode only `Win+L` and `Ctrl+Alt+Del`, which the probe cannot detect because they are never registered hotkeys. Warn but do not block on `F12`, which Microsoft's `RegisterHotKey` documentation reserves for the debugger. Do not enumerate Windows-key chords; rely on the probe.

**Modifier bookkeeping.** Snapshot modifier key state when the dialog opens, so a modifier released after the dialog consumed its key-down is not left stuck.

## Alternatives considered

- **Typing the combination as text:** rejected as the primary path. `KeyCombo::parse` already accepts `"ctrl+alt+left"`, so this stays available by editing config directly, but a text field for a keyboard shortcut requires the user to know Mosaix's key names.
- **A low-level keyboard hook to observe keystrokes during capture:** rejected, re-affirming ADR 0002.
- **A heartbeat or timeout guard instead of connection lifetime:** rejected because both can fail in the direction that leaves hotkeys dead, whereas pipe closure is signalled by the OS even on a hard kill.
- **Arming capture for the whole shortcuts page:** rejected because the dead window would last minutes and every navigation path away would have to disarm correctly.
- **Suspending registration only while the editor holds focus:** rejected because of re-registration churn and the compounding risk of losing a combination to another application on each cycle.

## Consequences

- Re-registration after capture is partial-success, which `hotkeys.rs:44` already models per binding. A combination taken by another application while Mosaix was suspended comes back failed, and the editor must name which bindings did not return rather than leaving the user to discover it.
- While the editor is open, no Mosaix hotkey works anywhere on the system. That is deliberate and bounded by a visible window, but it must be stated in the editor rather than inferred.
- Mosaix would be the first tiling window manager in the surveyed field to ship a GUI hotkey editor, so there is no precedent to inherit beyond PowerToys' interaction design.
- The conflict probe gives the user a reason before they save, rather than a binding that silently fails to register at the next reload.
