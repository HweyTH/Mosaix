# Research: Cycle-on-repeat detection and Windows hotkey conflict handling

> **TL;DR**: Among tools that actually implement "press-again-to-cycle-size," Rectangle uses a **hybrid** strategy — it primarily tracks state (last-applied action + a repeat `count`, invalidated the instant the window's live rect stops matching what Rectangle itself last set, with exact `CGRect` equality and no epsilon), but also does a **bounds-based reconciliation pass** first, scanning the cycle's candidate rects for one that equals the window's current rect (again exact equality, except a documented 4pt-minimum tolerance used only in its corner-cycle-expansion path). Loop is purely **state-based**: it stores an ordered list of past `WindowAction`s per `CGWindowID` and finds "where we are" in the cycle array by struct equality on the recorded action, invalidating that state the moment the user manually drags/resizes the window with the mouse. PowerToys FancyZones, GlazeWM, komorebi, yabai, and AeroSpace do **not** implement size-cycling at all — their "cycle" verbs (where they exist) refer to focus/workspace/monitor/zone-index cycling, not repeated-press half→third→two-thirds resizing. For hotkeys: PowerToys FancyZones uses `RegisterHotKey`/`UnregisterHotKey` for its runtime shortcuts, logs (does not toast) a `RegisterHotKey` failure at the point of registration, but *also* runs a proactive, live conflict probe in its Settings UI (temporarily calling `RegisterHotKey`/`UnregisterHotKey` against a null window and checking `GetLastError() == ERROR_HOTKEY_ALREADY_REGISTERED`) that surfaces conflicts as a tooltip/colored key-visual/dialog with an explicit "ignore and save anyway" override — with **no hardcoded reserved-shortcut list**, it relies entirely on the OS's own rejection. GlazeWM instead uses a single global `WH_KEYBOARD_LL` low-level keyboard hook for *all* its keybindings (no `RegisterHotKey` calls at all), which means it has no registration-conflict concept whatsoever — it simply intercepts and can shadow any OS shortcut. komorebi doesn't register hotkeys itself; it delegates to the external `whkd` daemon, which also uses a `WH_KEYBOARD_LL` hook (via the `win-hotkeys` crate) — and komorebi's own docs explicitly warn that `whkd` "does not include workarounds for Microsoft's restrictions on hotkey combinations that can use the Windows key," recommending AutoHotkey instead when Win-key bindings matter.

## Findings

### Question 1: Cycle-on-repeat detection — Loop

Loop's cycle is a first-class `WindowAction` variant: `direction == .cycle` carries an ordered array `cycle: [WindowAction]` of sub-actions ([`WindowAction.swift` init](https://github.com/mrkai77/Loop/blob/821a174e8e9027e62497888edd154cbeeae365e9/Loop/Window%20Management/Window%20Action/WindowAction.swift#L77-L94)). The engine itself treats `.cycle` as a no-op ([`WindowActionEngine.performApply`](https://github.com/mrkai77/Loop/blob/398b06b19e6b03f9ff40200acab1a4db60466a52/Loop/Window%20Management/Window%20Manipulation/WindowActionEngine.swift#L99-L107)) — the actual step resolution happens one layer up, in `LoopManager.getNextCycleAction`.

This is **purely state-based**, not bounds-based:

- [`LoopManager.getNextCycleAction`](https://github.com/mrkai77/Loop/blob/2467291f3095a571e80fdb0024845d4dedf111c9/Loop/Core/LoopManager.swift#L476-L525) finds "where we currently are" in the cycle array via `currentCycle.firstIndex(of: resizeContext.action)` — an **object/struct-equality lookup** on the *last applied `WindowAction` value* (its `Equatable`/`Hashable` conformance covers direction, size/anchor params, and the nested cycle itself — see [`WindowAction.SemanticKey`](https://github.com/mrkai77/Loop/blob/821a174e8e9027e62497888edd154cbeeae365e9/Loop/Window%20Management/Window%20Action/WindowAction.swift#L125-L157)), never a comparison of on-screen pixel bounds.
- When the loop session's live state (`resizeContext.action`) is `.noSelection` (e.g. a fresh keypress after the window lost the loop's live context), it falls back to a **per-window persisted record**: `WindowRecords.shared.getCurrentAction(for: window)`, which reads `record.actions[0]` — the most recently recorded `WindowAction` for that `CGWindowID` — from an in-memory dictionary (`recordsByWindowID: [CGWindowID: Record]`) ([`WindowRecords.swift`](https://github.com/mrkai77/Loop/blob/e21d282caf96c50409efdd11f7701fbaf81413bc/Loop/Window%20Management/Window%20Manipulation/WindowRecords.swift#L138-L160)).
- If no match is found in the cycle array (index is `nil`), it restarts at `currentCycle[0]` — step 1.
- A user setting, `Defaults[.cycleModeRestartEnabled]`, can force *every* repeated press to always restart at step 1 rather than resume ([`getNextCycleAction`, lines 493-496](https://github.com/mrkai77/Loop/blob/2467291f3095a571e80fdb0024845d4dedf111c9/Loop/Core/LoopManager.swift#L493-L496)).

**What invalidates the stored state**: `WindowRecords.eraseRecords(for:)` is called from `WindowDragManager` on **mouse-up after the user manually drags or resizes a window** ([`WindowDragManager.swift`, mouse-up handler](https://github.com/mrkai77/Loop/blob/2f9d20725b13b784c20abadc90a5f987e064200f/Loop/Core/WindowDragManager.swift#L126-L132) and [line 242](https://github.com/mrkai77/Loop/blob/2f9d20725b13b784c20abadc90a5f987e064200f/Loop/Core/WindowDragManager.swift#L242)) — that erases the window's entire action-history record, so the next Loop keypress on that window has no stored "current action" to match against and starts the cycle fresh at step 1. There is no timeout-based or focus-change-based invalidation found; the record persists across focus changes and is keyed purely by `CGWindowID`, so a window that keeps its `CGWindowID` (i.e. isn't closed/recreated) retains its cycle position even if the user switches away and back — only a manual mouse-driven move/resize, or the window closing (which naturally drops the dictionary entry when a fresh window with a new id appears), resets it.

### Question 1: Cycle-on-repeat detection — Rectangle

Confirmed active/not archived — `rxhanson/Rectangle` was pushed to as recently as 2026-08-19, so this is the canonical repo, not a fork.

Rectangle's repeated-press cycling ("Almost Maximize"-style half→third→two-thirds behavior, configurable via `SubsequentExecutionMode` — [`SubsequentExecutionMode.swift`](https://github.com/rxhanson/Rectangle/blob/2c1b5aa24424143d0c371ac92415c3c069c0f6bd/Rectangle/SubsequentExecutionMode.swift)) is a **hybrid of bounds-based and state-based detection**, and the two layers do different jobs:

1. **Top-level reconciliation (state, exact-equality invalidation).** Every action execution, `WindowManager.execute` compares the window's live rect to what Rectangle itself last set it to:
   ```
   let windowMovedExternally = currentWindowRect != lastRectangleAction?.rect
   ```
   ([`WindowManager.swift`, lines 87-98](https://github.com/rxhanson/Rectangle/blob/dead394b9d6de2f6a11a50dcfa3becaf4169701c/Rectangle/WindowManager.swift#L87-L98)). This is an **exact `CGRect` equality check, no epsilon at all**. If the window doesn't match exactly — user manually resized it, another app moved it, or even a sub-pixel DPI/rounding drift — the stored `lastRectangleActions[windowId]` entry is deleted outright, so the very next repeated press has no history to resume from and restarts at step 1.
2. **Step-index bookkeeping.** `WindowManager.recordAction` increments a per-window `count` only when the same `action` repeats consecutively; a different action resets `count` to 1 ([`WindowManager.swift`, lines 24-44](https://github.com/rxhanson/Rectangle/blob/dead394b9d6de2f6a11a50dcfa3becaf4169701c/Rectangle/WindowManager.swift#L24-L44)), stored in `WindowHistory.lastRectangleActions: [CGWindowID: RectangleAction]` ([`WindowHistory.swift`](https://github.com/rxhanson/Rectangle/blob/2c1b5aa24424143d0c371ac92415c3c069c0f6bd/Rectangle/WindowHistory.swift)).
3. **Bounds-based reconciliation inside the calculation itself.** `RepeatedExecutionsInThirdsCalculation.calculateRepeatedSideRect` (used by e.g. left/right half cycling) first checks the action-type is compatible with the last one, then walks the cycle's candidate sizes **comparing each candidate's computed rect against the window's actual current rect via `CGRect.equalTo` (exact, no tolerance)**, advancing from whichever candidate matches; only if none match does it fall back to the stored `count`-derived index ([`RepeatedExecutionsInThirdsCalculation.swift`, lines 22-72](https://github.com/rxhanson/Rectangle/blob/2c662c134b2560636e63eed5e5c3def7c886cddf/Rectangle/WindowCalculation/RepeatedExecutionsInThirdsCalculation.swift#L22-L72)). The base protocol's simpler variant, `RepeatedExecutionsCalculation.calculateRepeatedRect`, skips the bounds scan and goes straight from the stored `count` ([`RepeatedExecutionsCalculation.swift`, lines 23-39](https://github.com/rxhanson/Rectangle/blob/2c662c134b2560636e63eed5e5c3def7c886cddf/Rectangle/WindowCalculation/RepeatedExecutionsCalculation.swift#L23-L39)).
4. **The one place an explicit epsilon appears**: the corner-quadrant cycle path (`CornerCycleExpansionCalculation`, used for gap-aware corner-cycling) compares frames with `tolerance = max(4, gapSize*2 + 4)` points, to absorb the configured window-gap size when matching a candidate frame to the live window rect ([`RepeatedExecutionsCalculation.swift`, lines 137-165](https://github.com/rxhanson/Rectangle/blob/2c662c134b2560636e63eed5e5c3def7c886cddf/Rectangle/WindowCalculation/RepeatedExecutionsCalculation.swift#L137-L165)). Everywhere else (plain half/third cycling), Rectangle uses exact equality with no DPI/rounding slack.
5. There's also a base-class helper, `isRepeatedCommand`, used by the cross-monitor cycling path (`LeftRightHalfCalculation.calculateLeftAcrossDisplays`/`calculateRightAcrossDisplays`), which likewise does exact-rect comparison of the last-applied rect to the current window rect (no tolerance) — [`WindowCalculation.swift`, lines 39-45](https://github.com/rxhanson/Rectangle/blob/5c95a544d65849e5fecf8934f20123c7028738b6/Rectangle/WindowCalculation/WindowCalculation.swift#L39-L45), used at [`LeftRightHalfCalculation.swift`, lines 59-92](https://github.com/rxhanson/Rectangle/blob/2c662c134b2560636e63eed5e5c3def7c886cddf/Rectangle/WindowCalculation/LeftRightHalfCalculation.swift#L59-L92).

**Net effect for Mosaix's decision**: Rectangle's design shows that a *pure* bounds-comparison scheme with zero tolerance is workable on macOS's point-based coordinate system (not raw pixels), but the authors still felt the need for a persisted step-counter as a fallback and, in the one place gaps/gutters are involved, added an explicit multi-point tolerance rather than trusting exact equality.

### Question 1: Cycle-on-repeat detection — PowerToys FancyZones

FancyZones does **not** have a "half → third → two-thirds" size-cycle feature for an unzoned window. What it *does* have is keyboard-driven **zone-index cycling**: pressing Win+Ctrl+Arrow repeatedly moves a window through the zones of the active custom-layout grid, one zone at a time, wrapping at the ends when a `cycle` flag is set ([`WindowKeyboardSnap.h`](https://github.com/microsoft/PowerToys/blob/bf16e10baf0abbb38a5848cb25f3e146ad6e72c1/src/modules/fancyzones/FancyZonesLib/WindowKeyboardSnap.h#L39-L53)).

This *is* state-based, but the "state" is **the window's currently-assigned zone index**, not a size-cycle step counter:

```cpp
auto zoneIndexes = layoutWindows.GetZoneIndexSetFromWindow(window);
...
const ZoneIndex oldId = zoneIndexes[0];
// advance oldId +/-1, wrapping via the `cycle` bool at the grid edges
```
([`WindowKeyboardSnap.cpp`, `MoveByDirectionAndIndex`, lines 256-316](https://github.com/microsoft/PowerToys/blob/bf16e10baf0abbb38a5848cb25f3e146ad6e72c1/src/modules/fancyzones/FancyZonesLib/WindowKeyboardSnap.cpp#L256-L316)). `GetZoneIndexSetFromWindow` reads a persisted window→zone-index registry maintained per work-area (`LayoutAssignedWindows`), not a comparison of the window's on-screen bounds. There is no size-division concept (no "half/third/two-thirds") anywhere in this code path — it's purely which grid cell(s) the window currently occupies.

### Question 1: Cycle-on-repeat detection — GlazeWM

**Not implemented.** GlazeWM's only uses of "cycle" in its command layer are `cycle_focus.rs` (window focus cycling) — see [file listing search](https://github.com/glzr-io/glazewm/blob/71464306bcbb9ede6a7abea0f70ca1488d670f95/packages/wm/src/commands/general/cycle_focus.rs) — and the corresponding CLI verb in `app_command.rs`. Its window-resize command, `resize_window`, adjusts a tiling-tree node's split weight by a caller-supplied delta (`width_delta`/`height_delta`) with no stored step index and no candidate-size list at all ([`resize_window.rs`, lines 1-50](https://github.com/glzr-io/glazewm/blob/42436baf9655a9094d9be28b0f9ad40965955dd6/packages/wm/src/commands/window/resize_window.rs#L1-L50)). This is architecturally expected: GlazeWM is a BSP/i3-style tiling WM where "resize" is a continuous tree-weight adjustment, not a discrete-preset snap — there is no half/third/two-thirds concept to cycle through.

### Question 1: Cycle-on-repeat detection — komorebi

**Not implemented** in the WM core. A code search for "cycle" across `LGUG2Z/komorebi` turns up only focus/workspace/monitor/stack-index cycling commands (`cycle-focus`, `cycle-workspace`, `cycle-monitor`, `cycle-stack`, `cycle-move*`, etc. — see the [`docs/cli/cycle-*.md`](https://github.com/LGUG2Z/komorebi/tree/master/docs/cli) pages and their handlers in `komorebi/src/process_command.rs` / `komorebi/src/workspace.rs`), never a window-size cycle. Like GlazeWM, komorebi is a tiling WM where resizing (`resize-edge`, `resize-axis`) is a continuous delta adjustment to the tiling tree, not a preset-size cycle.

### Question 1: Cycle-on-repeat detection — yabai

**Not implemented.** `src/message.c`, which defines every CLI command yabai recognizes, contains no "cycle" token at all, and its one discrete-placement feature, `--grid` (`COMMAND_WINDOW_GRID`), is a **one-shot absolute placement** — the caller supplies explicit `rows:cols:start-x:start-y:width:height` arguments on every invocation ([`message.c`, line 141](https://github.com/koekeishiya/yabai/blob/ccbe8bda1f15aa5e8379791996514a2e441a34e3/src/message.c#L141)); there is no "repeat the same shortcut to advance" behavior. `src/window.c`, which holds yabai's window-manipulation logic, likewise contains no "grid", "cycle", "half", or "third" tokens ([searched, no matches](https://github.com/koekeishiya/yabai/blob/5bde933ec85a4a601a186163b7db04aa3bf6c3b1/src/window.c)) — yabai is a pure BSP-tree tiling WM; its `--resize` command takes an explicit delta, not a preset step.

### Question 1: Cycle-on-repeat detection — AeroSpace

**Not implemented.** AeroSpace's command set (`Sources/AppBundle/command/impl/`) has no cycle/step-size command; its `ResizeCommand` mutates a tiling-tree node's `weight` by an add/subtract/set numeric amount with no persisted repeat state at all:

```swift
let diff: CGFloat = switch args.units.val {
    case .set(let unit): CGFloat(unit) - node.getWeight(orientation)
    case .add(let unit): CGFloat(unit)
    case .subtract(let unit): -CGFloat(unit)
}
```
([`ResizeCommand.swift`, lines 1-55](https://github.com/nikitabobko/AeroSpace/blob/d07cfee4a04c48b6d3c2b14e0bfa1ae4603b69f9/Sources/AppBundle/command/impl/ResizeCommand.swift#L1-L55)). Each invocation is independent — pressing the same "resize" binding repeatedly with `add` just keeps adding the same delta, it does not advance through a fixed list of target sizes. Like the other tiling WMs surveyed, AeroSpace has no half/third/two-thirds snap-cycle concept.

### Question 2: Hotkey conflict handling — PowerToys FancyZones

**Mechanism**: `RegisterHotKey`/`UnregisterHotKey`, not a low-level keyboard hook, for the actual runtime global shortcuts. `FancyZones::UpdateHotkey` unregisters the previous binding, then registers the new one and only **logs** a failure — no toast, no dialog, at this call site:

```cpp
void FancyZones::UpdateHotkey(int hotkeyId, const PowerToysSettings::HotkeyObject& hotkeyObject, bool enable) noexcept
{
    ...
    UnregisterHotKey(m_window, hotkeyId);
    if (!enable) { return; }
    auto modifiers = hotkeyObject.get_modifiers();
    auto code = hotkeyObject.get_code();
    auto result = RegisterHotKey(m_window, hotkeyId, modifiers, code);
    if (!result)
    {
        Logger::error(L"Failed to register hotkey: {}", get_last_error_or_default(GetLastError()));
    }
}
```
([`FancyZones.cpp`, lines 1511-1533](https://github.com/microsoft/PowerToys/blob/29471231dd4efdb4433163be135c839177287cdc/src/modules/fancyzones/FancyZonesLib/FancyZones.cpp#L1511-L1533)).

**But PowerToys additionally runs a proactive, cross-module conflict detector** that surfaces conflicts to the user *before* a shortcut is even saved, independent of the above runtime path:

- The Settings UI's `ShortcutControl` sends an IPC request (`check_hotkey_conflict`) as the user edits a shortcut ([`HotkeyConflictHelper.cs`](https://github.com/microsoft/PowerToys/blob/75526b9580d4965a92c66fcd46a86d2c0b22895d/src/settings-ui/Settings.UI/Helpers/HotkeyConflictHelper.cs)).
- The background runner process handles it in [`settings_window.cpp`, lines 285-334](https://github.com/microsoft/PowerToys/blob/558e633c59aa8515fe7533b5b2a964c3b6bf4a85/src/runner/settings_window.cpp#L285-L334), delegating to `HotkeyConflictDetector::HotkeyConflictManager`.
- `HotkeyConflictManager` distinguishes `InAppConflict` (another PowerToys module already owns that combo — checked against its own in-memory `hotkeyMap`) from `SystemConflict` ([`hotkey_conflict_detector.h`, lines 33-38](https://github.com/microsoft/PowerToys/blob/75526b9580d4965a92c66fcd46a86d2c0b22895d/src/runner/hotkey_conflict_detector.h#L33-L38)).
- **`SystemConflict` detection is a live OS probe, not a hardcoded list**: `HasConflictWithSystemHotkey` actually calls `RegisterHotKey(nullptr, 0x0FFF, modifiers, key)` with a throwaway id/window, checks `GetLastError() == ERROR_HOTKEY_ALREADY_REGISTERED`, and immediately `UnregisterHotKey`s if the probe succeeded:
  ```cpp
  if (!RegisterHotKey(nullptr, hotkeyId, modifiers, hotkey.key))
  {
      if (GetLastError() == ERROR_HOTKEY_ALREADY_REGISTERED) { return true; }
  }
  else { UnregisterHotKey(nullptr, hotkeyId); }
  return false;
  ```
  ([`hotkey_conflict_detector.cpp`, lines 334-381](https://github.com/microsoft/PowerToys/blob/75526b9580d4965a92c66fcd46a86d2c0b22895d/src/runner/hotkey_conflict_detector.cpp#L334-L381)). **There is no hardcoded reserved-combo list anywhere in this file** — PowerToys relies entirely on `RegisterHotKey`'s own OS-level arbitration for system-conflict detection, it just does the probe *proactively* (while editing) in addition to the real registration later.
- The result reaches the UI as `HasConflict`/`ConflictDescription` on the shortcut's `HotkeySettings`, driving a colored key-visual, a tooltip (`SysHotkeyConflictTooltipText` vs `InAppHotkeyConflictTooltipText`), and an optional `ShortcutConflictWindow` dialog; the user can flip `IgnoreConflict` to save the binding anyway ([`ShortcutControl.xaml.cs`](https://github.com/microsoft/PowerToys/blob/e62a41c53a7e4a38e21fd0edfcd469398dbca4ba/src/settings-ui/Settings.UI/SettingsXAML/Controls/ShortcutControl/ShortcutControl.xaml.cs#L565-L610)).

**A separate, unrelated use of `SetWindowsHookEx`** exists in the Settings app only: `HotkeySettingsControlHook` wraps a `WH_KEYBOARD_LL`-based `KeyboardHook` purely to **capture** the keys the user is physically pressing while recording a new shortcut in the editor UI ([`HotkeySettingsControlHook.cs`](https://github.com/microsoft/PowerToys/blob/195c6f588a45a471406670a1b11542fc50638f74/src/settings-ui/Settings.UI.Library/HotkeySettingsControlHook.cs)) — this is not the mechanism used to fire the actual runtime hotkey actions, which remains `RegisterHotKey` as shown above.

### Question 2: Hotkey conflict handling — komorebi/GlazeWM

These two projects take opposite approaches, and **neither** uses PowerToys' `RegisterHotKey`-plus-proactive-probe pattern:

**GlazeWM** never calls `RegisterHotKey` (zero hits searching the repo). Instead it installs one process-wide `WH_KEYBOARD_LL` hook and routes every keystroke through an app-level callback that returns whether to intercept it:

```rust
SetWindowsHookExW(WH_KEYBOARD_LL, Some(Self::hook_proc), HINSTANCE::default(), 0)
...
if should_intercept { return LRESULT(1); }
unsafe { CallNextHookEx(None, code, wparam, lparam) }
```
([`keyboard_hook.rs`, lines 82-195](https://github.com/glzr-io/glazewm/blob/42436baf9655a9094d9be28b0f9ad40965955dd6/packages/wm-platform/src/platform_impl/windows/keyboard_hook.rs#L82-L195)). Because this is a low-level hook rather than an OS-arbitrated registration, **there is no "conflict" concept at all** for GlazeWM to detect — it does not ask the OS for permission to own a combo, it simply decides per-keystroke whether to swallow it, which lets it shadow Win+Arrow/Win+Tab/etc. if the user binds them, but also means the user gets no OS-level or app-level warning if their chosen combo collides with something else; a repo search for "reserved"/"conflict"/"duplicate binding" in the keybinding-config code turned up nothing, confirming there is no reserved-shortcut list or conflict UI in GlazeWM either.

**komorebi** does not register hotkeys itself at all — the WM core only exposes an IPC/CLI surface (`komorebic`) and expects a separate hotkey daemon to translate keypresses into `komorebic` invocations. Its own getting-started docs recommend `whkd`:

> "However, `whkd` is a very simple hotkey daemon, and notably, does not include workarounds for Microsoft's restrictions on hotkey combinations that can use the `Windows` key. If using hotkey combinations with the `Windows` key is important to you, I suggest that once you are familiar with the main `komorebic.exe` commands ... you use AutoHotKey to handle your key bindings."
([`docs/installation.md`, lines 12-24](https://github.com/LGUG2Z/komorebi/blob/00384ce3339b1533e367a586563300eb04392b21/docs/installation.md#L12-L24))

`whkd` itself (`LGUG2Z/whkd`) depends on the third-party `win-hotkeys` crate (`Cargo.toml`, [pinned](https://github.com/LGUG2Z/whkd/blob/5d6a767f2ab7ac309686fe44ce59689546cf55fe/Cargo.toml)), which — like GlazeWM — is a `WH_KEYBOARD_LL` low-level-hook wrapper, not a `RegisterHotKey` wrapper:

```rust
let hhook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook_proc), None, 0).unwrap();
```
([`win-hotkeys` `hook.rs`, lines 117-119](https://github.com/iholston/win-hotkeys/blob/1a55e2c079f0ff609cbf5489195d99e6227e7a1c/src/hook.rs#L117-L119)), with an explicit `KeyAction::{Allow, Block, Replace}` per-event decision and a documented `SILENT_KEY` workaround constant used to suppress the Windows-key's own shell-level side effects (line 25 of the same file) — precisely the kind of "workaround for Microsoft's restrictions" komorebi's docs say `whkd` **doesn't** implement, which is why Win-key chords are called out as unreliable with the default `whkd` setup. Net effect: the komorebi ecosystem's default hotkey path is a low-level hook with no OS conflict arbitration (same category as GlazeWM), and its own maintainers steer users toward AutoHotkey (a third, separate hooking implementation) specifically to work around Windows-key shortcut quirks that a bare low-level hook doesn't handle.

### Question 2: Official Microsoft documentation on reserved shortcuts

**`RegisterHotKey` API reference** (`learn.microsoft.com/windows/win32/api/winuser/nf-winuser-registerhotkey`):

- On the `MOD_WIN` modifier flag: *"Either WINDOWS key must be held down. These keys are labeled with the Windows logo. **Keyboard shortcuts that involve the WINDOWS key are reserved for use by the operating system.**"*
- On failure semantics: *"If the function fails, the return value is zero. To get extended error information, call `GetLastError`. This function fails if you try to associate a hot key with a window created by another thread. **Typically, `RegisterHotKey` also fails if the keystrokes specified for the hot key have already been registered for another hot key. However, some pre-existing, default hotkeys registered by the OS (such as PrintScreen, which launches the Snipping tool) may be overridden by another hot key registration** when one of the app's windows is in the foreground."*
- On F12: *"The F12 key is reserved for use by the debugger at all times, so it should not be registered as a hot key. Even when you are not debugging an application, F12 is reserved in case a kernel-mode debugger or a just-in-time debugger is resident."*

(https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey)

**PowerToys Keyboard Manager** (`learn.microsoft.com/windows/powertoys/keyboard-manager`) — an official first-party doc, separately confirms specific reserved combos in practice:

> "There are some shortcut keys that are reserved by the operating system or cannot be replaced. Keys that cannot be remapped include: **⊞ Win+L and Ctrl+Alt+Del cannot be remapped as they are reserved by the Windows OS.** ... **⊞ Win+G often opens the Xbox Game Bar, even when reassigned.**"

(https://learn.microsoft.com/en-us/windows/powertoys/keyboard-manager)

**Windows keyboard design guidance** (`learn.microsoft.com/windows/win32/uxguide/inter-keyboard`, a Win32 UX guideline page):

> "**Don't use the Windows logo modifier key for program shortcut keys.** Windows logo key is reserved for Windows use. Even if a Windows logo key combination isn't being used by Windows now, it may be in the future." and "**Don't use shortcut keys used by Windows for program shortcut keys.** Doing so will conflict with the Windows system shortcut keys when your program has input focus."

(https://learn.microsoft.com/en-us/windows/win32/uxguide/inter-keyboard)

None of these three official pages publish a complete enumerated list of every OS-reserved combo (Win+Arrow, Win+Tab, etc. are not itemized on the `RegisterHotKey` page itself) — the concrete confirmed reserved/unregisterable items from primary Microsoft sources are: any Win-key combo (by design, MOD_WIN remark), Win+L, Ctrl+Alt+Del, F12, and (as a practical/shell-level exception, not a `RegisterHotKey` failure) Win+G. This matches and reinforces what the PowerToys conflict detector already does at runtime: it doesn't try to hardcode this list either — it just asks the OS via a live `RegisterHotKey` probe.

## Sources

- [Loop `WindowAction.swift`](https://github.com/mrkai77/Loop/blob/821a174e8e9027e62497888edd154cbeeae365e9/Loop/Window%20Management/Window%20Action/WindowAction.swift) — cycle action model, `canRepeat`
- [Loop `LoopManager.swift`](https://github.com/mrkai77/Loop/blob/2467291f3095a571e80fdb0024845d4dedf111c9/Loop/Core/LoopManager.swift) — `getNextCycleAction`, state-based index lookup
- [Loop `WindowRecords.swift`](https://github.com/mrkai77/Loop/blob/e21d282caf96c50409efdd11f7701fbaf81413bc/Loop/Window%20Management/Window%20Manipulation/WindowRecords.swift) — per-window action history store
- [Loop `WindowDragManager.swift`](https://github.com/mrkai77/Loop/blob/2f9d20725b13b784c20abadc90a5f987e064200f/Loop/Core/WindowDragManager.swift) — manual-drag invalidation of cycle state
- [Loop `WindowActionEngine.swift`](https://github.com/mrkai77/Loop/blob/398b06b19e6b03f9ff40200acab1a4db60466a52/Loop/Window%20Management/Window%20Manipulation/WindowActionEngine.swift) — confirms `.cycle` is resolved before reaching the engine
- [Rectangle `WindowManager.swift`](https://github.com/rxhanson/Rectangle/blob/dead394b9d6de2f6a11a50dcfa3becaf4169701c/Rectangle/WindowManager.swift) — `windowMovedExternally` invalidation, `count` bookkeeping
- [Rectangle `WindowHistory.swift`](https://github.com/rxhanson/Rectangle/blob/2c1b5aa24424143d0c371ac92415c3c069c0f6bd/Rectangle/WindowHistory.swift) — per-window last-action store
- [Rectangle `WindowCalculation.swift`](https://github.com/rxhanson/Rectangle/blob/5c95a544d65849e5fecf8934f20123c7028738b6/Rectangle/WindowCalculation/WindowCalculation.swift) — `isRepeatedCommand` exact-rect check
- [Rectangle `RepeatedExecutionsCalculation.swift`](https://github.com/rxhanson/Rectangle/blob/2c662c134b2560636e63eed5e5c3def7c886cddf/Rectangle/WindowCalculation/RepeatedExecutionsCalculation.swift) — `cycleIndex`, corner-cycle tolerance
- [Rectangle `RepeatedExecutionsInThirdsCalculation.swift`](https://github.com/rxhanson/Rectangle/blob/2c662c134b2560636e63eed5e5c3def7c886cddf/Rectangle/WindowCalculation/RepeatedExecutionsInThirdsCalculation.swift) — bounds-scan-then-count-fallback hybrid
- [Rectangle `LeftRightHalfCalculation.swift`](https://github.com/rxhanson/Rectangle/blob/2c662c134b2560636e63eed5e5c3def7c886cddf/Rectangle/WindowCalculation/LeftRightHalfCalculation.swift) — half/third/two-thirds cycle entry point
- [Rectangle `CycleSize.swift`](https://github.com/rxhanson/Rectangle/blob/db8574e8a94b5df68eb70f7ea6adc96d1718fcb1/Rectangle/CycleSize.swift) — configurable cycle-size list
- [PowerToys `WindowKeyboardSnap.h`/`.cpp`](https://github.com/microsoft/PowerToys/blob/bf16e10baf0abbb38a5848cb25f3e146ad6e72c1/src/modules/fancyzones/FancyZonesLib/WindowKeyboardSnap.cpp) — FancyZones zone-index cycling (not size-cycling)
- [PowerToys `FancyZones.cpp`](https://github.com/microsoft/PowerToys/blob/29471231dd4efdb4433163be135c839177287cdc/src/modules/fancyzones/FancyZonesLib/FancyZones.cpp) — `RegisterHotKey`/`UnregisterHotKey`, log-only failure
- [PowerToys `hotkey_conflict_detector.h`/`.cpp`](https://github.com/microsoft/PowerToys/blob/75526b9580d4965a92c66fcd46a86d2c0b22895d/src/runner/hotkey_conflict_detector.cpp) — live `RegisterHotKey` conflict probe, no hardcoded reserved list
- [PowerToys `settings_window.cpp`](https://github.com/microsoft/PowerToys/blob/558e633c59aa8515fe7533b5b2a964c3b6bf4a85/src/runner/settings_window.cpp) — `check_hotkey_conflict` IPC handler
- [PowerToys `ShortcutControl.xaml.cs`](https://github.com/microsoft/PowerToys/blob/e62a41c53a7e4a38e21fd0edfcd469398dbca4ba/src/settings-ui/Settings.UI/SettingsXAML/Controls/ShortcutControl/ShortcutControl.xaml.cs) — conflict UI (tooltip, dialog, ignore-and-save)
- [PowerToys `HotkeySettingsControlHook.cs`](https://github.com/microsoft/PowerToys/blob/195c6f588a45a471406670a1b11542fc50638f74/src/settings-ui/Settings.UI.Library/HotkeySettingsControlHook.cs) — settings-UI-only `WH_KEYBOARD_LL` capture, distinct from the runtime mechanism
- [GlazeWM `keyboard_hook.rs`](https://github.com/glzr-io/glazewm/blob/42436baf9655a9094d9be28b0f9ad40965955dd6/packages/wm-platform/src/platform_impl/windows/keyboard_hook.rs) — sole `WH_KEYBOARD_LL` hook for all keybindings, no `RegisterHotKey`
- [GlazeWM `resize_window.rs`](https://github.com/glzr-io/glazewm/blob/42436baf9655a9094d9be28b0f9ad40965955dd6/packages/wm/src/commands/window/resize_window.rs) — delta-based resize, confirms no size-cycling
- [komorebi `docs/installation.md`](https://github.com/LGUG2Z/komorebi/blob/00384ce3339b1533e367a586563300eb04392b21/docs/installation.md) — official note on `whkd`'s Windows-key limitations, AutoHotkey recommendation
- [whkd `Cargo.toml`](https://github.com/LGUG2Z/whkd/blob/5d6a767f2ab7ac309686fe44ce59689546cf55fe/Cargo.toml) — depends on `win-hotkeys`
- [`win-hotkeys` `hook.rs`](https://github.com/iholston/win-hotkeys/blob/1a55e2c079f0ff609cbf5489195d99e6227e7a1c/src/hook.rs) — `SetWindowsHookExW(WH_KEYBOARD_LL, ...)` implementation used by `whkd`
- [yabai `message.c`](https://github.com/koekeishiya/yabai/blob/ccbe8bda1f15aa5e8379791996514a2e441a34e3/src/message.c) — CLI command table, confirms no cycle command, `--grid` is one-shot
- [yabai `window.c`](https://github.com/koekeishiya/yabai/blob/5bde933ec85a4a601a186163b7db04aa3bf6c3b1/src/window.c) — window logic, no grid/cycle/half/third tokens
- [AeroSpace `ResizeCommand.swift`](https://github.com/nikitabobko/AeroSpace/blob/d07cfee4a04c48b6d3c2b14e0bfa1ae4603b69f9/Sources/AppBundle/command/impl/ResizeCommand.swift) — tree-weight delta resize, no cycle state
- [`RegisterHotKey` function (winuser.h) — Win32 apps | Microsoft Learn](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-registerhotkey) — MOD_WIN reservation, failure/GetLastError semantics, F12 reservation
- [Remap Keys and Shortcuts with PowerToys Keyboard Manager | Microsoft Learn](https://learn.microsoft.com/en-us/windows/powertoys/keyboard-manager) — Win+L, Ctrl+Alt+Del, Win+G reserved/unreliable combos
- [Keyboard - Win32 apps | Microsoft Learn](https://learn.microsoft.com/en-us/windows/win32/uxguide/inter-keyboard) — design guidance against using the Windows-logo key or OS shortcuts for app shortcuts
