# Research: What physically happens to a window when a tiling manager switches away from its workspace?

> **TL;DR**: No shipping tiling window manager achieves real workspace isolation on Windows using only public APIs. The two mature Windows tilers, komorebi and GlazeWM, both default to `IApplicationView::SetCloak` — an **undocumented** shell COM interface obtained from `CLSID_ImmersiveShell` — and both hard-code the same interface IIDs (`372E1D3B-38D3-42E4-A15B-8AB2B178F513` / `1841c6d7-4f9d-42c0-af41-8747538f10e5`) with no per-build table. Neither uses Windows Virtual Desktops, because the public `IVirtualDesktopManager` has exactly three methods and none of them can create, enumerate, or switch a desktop; Microsoft's own maintainer states on Microsoft's own tracker that switching "is not possible with the public API" and that the private interface's "GUID changes build-to-build (as it should)". Microsoft's first-party modules stay inside the public surface and therefore ship **no workspace hiding at all**: FancyZones only repositions, and PowerToys Workspaces only launches and repositions apps. The remaining public-API mechanisms are minimize (komorebi's documented non-default, "has issues with frequent workspace switching") and parking windows in a screen corner with a 1 px sliver visible (GlazeWM's `place_in_corner`, and the only mechanism AeroSpace uses on macOS). Crash safety maps directly onto the mechanism: komorebi persists every known HWND to `komorebi.hwnd.json` and ships `komorebic restore-windows`; GlazeWM ships no equivalent, and its long-running issue #79 documents users permanently losing windows to a hard kill; AeroSpace's corner-parking is self-recovering because the user can drag the sliver back. For Mosaix, mechanisms 1 and 2 are both barred by the existing non-goal on private APIs — mechanism 1 because the public API cannot switch desktops at all, mechanism 2 because in practice everyone implements "hide" as an undocumented cloak.

## Findings

### Scope and source selection

This covers what each manager does to a window's on-screen existence when its workspace stops being displayed, read from the implementation rather than from feature lists. The Windows managers are komorebi and GlazeWM (the two actively maintained automatic tilers already used as reference points in [`docs/research/automatic-tiling-managers.md`](./automatic-tiling-managers.md)) plus Microsoft's own FancyZones and Workspaces modules, which are the strongest available evidence about which APIs Microsoft considers supported. The macOS managers are AeroSpace, yabai, and Amethyst, included because Mosaix's macOS adapter is unbuilt and the workspace model must not assume a mechanism macOS cannot provide.

### The four candidate mechanisms, as the field actually uses them

| Manager | Mechanism on workspace switch | Private / undocumented API? | Crash recovery | Workspace scope |
| --- | --- | --- | --- | --- |
| **komorebi** | `IApplicationView::SetCloak(1, 2)` by default; `SW_MINIMIZE` and `SW_HIDE` selectable via `window_hiding_behaviour` | **Yes.** Defines `IServiceProvider`, `IApplicationView`, `IApplicationViewCollection` by raw IID and obtains them from `CLSID_ImmersiveShell`. Does **not** define or use `IVirtualDesktopManagerInternal`. | Ctrl-C handler calls `restore_all_windows`; every known HWND is mirrored to `komorebi.hwnd.json`; `komorebic restore-windows` replays it. See caveat below — that replay is `SW_RESTORE` only. | Per monitor (`Monitor.workspaces: Ring<Workspace>`) |
| **GlazeWM** | `IApplicationView::SetCloak(1, 2)` by default on Windows; `hide` (`SW_HIDE`) legacy; `place_in_corner` on macOS and optional on Windows | **Yes.** Same two interfaces, same IIDs, source comments literally read "Undocumented COM interface". Does **not** use `IVirtualDesktopManagerInternal`. | Graceful exit calls `ShowWindowAsync(SW_SHOWNA)`, which its own doc comment says does not uncloak; `set_cloaked` appears exactly once in the whole WM package and only in the sync path. Hard kill leaves windows cloaked and a restart skips them. | Global pool of named workspaces, one displayed per monitor |
| **PowerToys FancyZones** | **None.** No workspace concept; only `SetWindowPlacement` repositioning | No. Public `IVirtualDesktopManager` plus registry reads only. | N/A — nothing is ever hidden | Layouts are per monitor, keyed by virtual-desktop GUID read from the registry |
| **PowerToys Workspaces** | **None.** Launches apps and repositions them via `SetWindowPlacement`, preserving captured minimized/maximized state | No. Documented as "publicly available APIs and the FancyZones engine under the hood" | N/A — nothing is ever hidden | Not a workspace switcher; a launch profile |
| **AeroSpace** (macOS) | Parks windows in the monitor's bottom-left/bottom-right corner, offset by one pixel | One private symbol only (`_AXUIElementGetWindow`); explicitly refuses SIP changes and injection | `beforeTermination()` re-centres every window on quit **and** on detected crash (`dieT` → `beforeTermination`); the 1 px sliver is a manual last resort | Global pool of workspaces shared between monitors; each monitor shows one |
| **yabai** (macOS) | Native macOS Spaces — the OS hides the windows | **Yes, extensively.** ~100 private `SLS*` SkyLight symbols even in the base build; space manipulation additionally requires injecting a scripting addition into `Dock.app` with SIP partially disabled | N/A — Spaces state is owned by the OS, so a yabai crash does not strand windows | Native Spaces, per display (requires "Displays have separate Spaces") |
| **Amethyst** (macOS) | Native macOS Spaces; moves a window between Spaces by synthesizing a title-bar mouse drag plus the system Space-switch shortcut | **Yes**, private CGS/`CGSSpace` APIs via Silica, plus `_AXUIElementGetWindow` | N/A — Spaces state is owned by the OS | Native Spaces |

Read across the table, the finding is uniform: **every manager that actually hides windows on switch either uses an undocumented API to do it, or accepts leaving the window visibly on screen.** There is no third option in the field.

### What the public Windows virtual-desktop API can and cannot do

`IVirtualDesktopManager` has exactly three methods, per the Microsoft Learn reference: `GetWindowDesktopId`, `IsWindowOnCurrentVirtualDesktop`, and `MoveWindowToDesktop` ([IVirtualDesktopManager, shobjidl_core.h — Methods table](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ivirtualdesktopmanager)). It can therefore:

- **ask** which desktop GUID hosts a given top-level window;
- **ask** whether a given window is on the active desktop;
- **move** a window to a desktop whose GUID you already possess.

It cannot create a desktop, destroy one, enumerate them, name them, reorder them, or switch the active one. There is no method for any of those. The same page's Remarks go further and tell applications not to try: *"applications should avoid automatically switching the user from one virtual desktop to another. Only the user should instigate that change."* Note also that `MoveWindowToDesktop` needs a GUID, and the only public way to obtain one is `GetWindowDesktopId` on a window that is already on the desired desktop — so even the one write method cannot target a desktop that has no window on it yet.

The first-party confirmation is unambiguous. On Microsoft's own tracker, PowerToys maintainer Dustin Howett (DHowett) opened an issue about letting Window Walker switch desktops and wrote: *"This seems like it's not possible with the public API."* He listed the same three methods verbatim, then: *"There are however private APIs for switching desktops. `IVirtualDesktopManagerInternal` has a `SwitchDesktop` method. Problem is, it's private. And it's GUID changes build-to-build (as it should)."* ([PowerToys #38287, issue body](https://github.com/microsoft/PowerToys/issues/38287)). The issue was closed in July 2026 by zadjii-msft with: *"I originally was trying to push to get that API made public, so we could inbox that extension. Alas, that seems like something we won't get soon, so I'll close this."* ([comment on #38287](https://github.com/microsoft/PowerToys/issues/38287#issuecomment-4972516383)).

**Implication for Mosaix:** mechanism 1 is not merely risky under the non-goal in [`ARCHITECTURE.md` §21](../../ARCHITECTURE.md) ("Private APIs, process injection, or reduced System Integrity Protection"); it is *functionally unavailable*. A workspace feature built on the public interface could not switch workspaces at all. It could only tag windows onto desktops the user already created and then ask the user to press `Win+Ctrl+←`.

### Per-build GUID churn is a measured cost, not a rumour

DHowett's claim is directly verifiable in the reference implementation he cites. `MScholtes/VirtualDesktop` ships five separate source files and five separate binaries because the interface identity is not stable:

> **"With Windows 11 23H2 Release 3085 Microsoft did change the API (COM GUIDs) for accessing the functions for virtual desktops again. I provide five versions of virtualdesktop.cs now: virtualdesktop.cs is for Windows 10, virtualdesktop11.cs is for Windows 11, virtualdesktop11-24h2.cs for Windows 11 24H2, virtualdesktopserver2022.cs is for Windows Server 2022, virtualdesktopserver2016.cs is for Windows Server 2016."**
> — [MScholtes/VirtualDesktop README](https://github.com/MScholtes/VirtualDesktop/blob/master/README.md)

The `IVirtualDesktopManagerInternal` IID in each file confirms it: `F31574D6-B682-4CDC-BD56-1827860ABEC6` in [`VirtualDesktop.cs`](https://github.com/MScholtes/VirtualDesktop/blob/master/VirtualDesktop.cs) (Windows 10), `53F5CA0B-158F-4124-900C-057158060B27` in [`VirtualDesktop11.cs`](https://github.com/MScholtes/VirtualDesktop/blob/master/VirtualDesktop11.cs), and `094afe11-44f2-4ba0-976f-29a97e263ee0` in [`VirtualDesktopServer2022.cs`](https://github.com/MScholtes/VirtualDesktop/blob/master/VirtualDesktopServer2022.cs). That is a per-build compatibility matrix maintained by hand, shipped as separate executables, for one feature.

Neither komorebi nor GlazeWM pays this cost, because neither touches `IVirtualDesktopManagerInternal` at all. Both hard-code exactly one IID for `IApplicationView` and one for `IApplicationViewCollection`, with no version branch (see below). That is a smaller exposure than the virtual-desktop interfaces, but it is still an unversioned dependency on an interface Microsoft does not document.

### komorebi: cloaking by default, with hide and minimize as documented alternatives

komorebi's hiding policy is a single enum, and its doc comments are the maintainer's own assessment of each mechanism:

```rust
pub enum HidingBehaviour {
    /// END OF LIFE FEATURE: Use the `SW_HIDE` flag to hide windows when switching workspaces (has issues with Electron apps)
    #[deprecated(note = "End of life feature")]
    Hide,
    /// Use the `SW_MINIMIZE` flag to hide windows when switching workspaces (has issues with frequent workspace switching)
    Minimize,
    /// Use the undocumented SetCloak Win32 function to hide windows when switching workspaces
    Cloak,
}
```
— [`komorebi/src/core/mod.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/core/mod.rs)

That is a direct verdict on three of the four candidate mechanisms from someone who shipped all three: `SW_HIDE` is end-of-life and breaks Electron apps, `SW_MINIMIZE` breaks under frequent switching, and the survivor is explicitly labelled undocumented. The default is `Cloak`, set both in code (`HIDING_BEHAVIOUR: Arc<Mutex<HidingBehaviour>> = Arc::new(Mutex::new(HidingBehaviour::Cloak))` in [`komorebi/src/lib.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/lib.rs)) and in the published schema, where `window_hiding_behaviour` is documented as "Which Windows signal to use when hiding windows" with default `"Cloak"` ([komorebi Windows configuration schema](https://komorebi.lgug2z.com/reference/komorebi-windows/)).

The dispatch itself is in `Window::hide_with_border` / `Window::restore_with_border` in [`komorebi/src/window.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/window.rs):

```rust
match *hiding_behaviour {
    HidingBehaviour::Hide => WindowsApi::hide_window(self.hwnd),
    HidingBehaviour::Minimize => WindowsApi::minimize_window(self.hwnd),
    HidingBehaviour::Cloak => SetCloak(self.hwnd(), 1, 2),
}
```

`SetCloak` resolves `IServiceProvider` from `CLSID_ImmersiveShell`, queries `IApplicationViewCollection`, calls `get_view_for_hwnd`, and calls `IApplicationView::set_cloak(1, 2)` ([`komorebi/src/com/mod.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/com/mod.rs)). The interface definitions are in [`komorebi/src/com/interfaces.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/com/interfaces.rs), whose first line is *"This code is largely taken verbatim from this repository: https://github.com/Ciantic/AltTabAccessor"* — i.e. a reverse-engineering project. It declares exactly three interfaces, at lines 87, 97, and 200:

- `IServiceProvider` — `6D5140C1-7436-11CE-8034-00AA006009FA`
- `IApplicationView` — `372E1D3B-38D3-42E4-A15B-8AB2B178F513`
- `IApplicationViewCollection` — `1841c6d7-4f9d-42c0-af41-8747538f10e5`

There is no `IVirtualDesktopManagerInternal` and no per-build GUID table anywhere in the file.

komorebi does read the native virtual desktop, but only as an identity check: `current_virtual_desktop()` in [`komorebi/src/lib.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/lib.rs) reads the raw `CurrentVirtualDesktop` value from `HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\SessionInfo\{session}\VirtualDesktops` (Windows 10) or `...\Explorer\VirtualDesktops` (Windows 11), with an explicit comment that the value does not exist until the user has opened task view. komorebi's own workspaces are entirely independent of it — they are per-monitor rings (`pub workspaces: Ring<Workspace>` on the `Monitor` struct, [`komorebi/src/monitor.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/monitor.rs)), and the switch is `Workspace::hide` / `Workspace::restore` in [`komorebi/src/workspace.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/workspace.rs), which walk floating windows, containers, the maximized window, and the monocle container and call `Window::hide()` on each.

### komorebi's crash recovery: an on-disk HWND ledger, with one gap

komorebi treats "the manager died while windows were hidden" as a first-class failure mode, and the design is worth copying in shape even if the mechanism is not.

1. **Graceful exit.** `komorebic stop` is documented as "Stop the komorebi.exe process and restore all hidden windows", with a hidden `--ignore-restore` escape hatch ([`komorebic/src/main.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebic/src/main.rs), `struct Stop` and the `Stop` subcommand doc).
2. **Signal.** The Ctrl-C handler in [`komorebi/src/main.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/main.rs) logs `"received ctrl-c, restoring all hidden windows..."`, dumps state to disk, and calls `wm.lock().restore_all_windows(false)?`. `restore_all_windows` in [`komorebi/src/window_manager.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/window_manager.rs) walks every monitor → workspace → container → window, undoes title-bar removal, transparency and accent changes, and calls `window.restore()`.
3. **Hard kill.** On every event, komorebi rewrites `DATA_DIR/komorebi.hwnd.json` with the full list of known HWNDs ([`komorebi/src/window_manager.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/window_manager.rs), the "Save to file" block). `komorebic restore-windows` — described in the CLI as a "Restore all hidden windows (debugging command)" — reads that file back and calls `restore_window(hwnd)` on each entry. `komorebic kill` does the same after force-stopping the process.

The maintainer described this design himself, in a GlazeWM issue, when a GlazeWM user hit the same problem: *"Every time an event occurs, komorebi updates a local JSON file with the list of all known HWNDs across all workspaces. Then, if there is a crash or the process must be killed forcefully, the user can run the `restore-windows` command… For regular process terminations (ie. ctrl-c), komorebi uses a ctrl-c signal handler to restore all known HWNDs before finally processing the ctrl-c and exiting the process."* — LGUG2Z, [glzr-io/glazewm#79](https://github.com/glzr-io/glazewm/issues/79).

**One gap, read from the code and not empirically tested.** The out-of-process recovery path is only `ShowWindow(SW_RESTORE)`:

```rust
fn restore_window(hwnd: isize) {
    show_window(HWND(hwnd as *mut core::ffi::c_void), SW_RESTORE);
```
— [`komorebic/src/main.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebic/src/main.rs)

The string `SetCloak` does not appear anywhere in `komorebic`. `SW_RESTORE` un-minimizes and un-hides; it does not clear a cloak set through `IApplicationView`. So `komorebic restore-windows` and `komorebic kill` fully recover the `Minimize` and `Hide` behaviours, and — on the reading of the code — do not recover the **default** `Cloak` behaviour after a hard kill. I could not find a maintainer statement confirming or denying this, and I did not run it; the claim here is exactly that the recovery command issues no uncloak call.

### GlazeWM: the same undocumented cloak, plus the only public-API hiding mechanism in the field

GlazeWM exposes the choice in user-facing config, and its own comments rank the options:

```yaml
  # How windows should be hidden when switching workspaces.
  # - 'cloak': (Windows-only) Recommended option for Windows.
  # - 'hide': (Windows-only) Legacy option for Windows. Has stability issues with some apps.
  # - 'place_in_corner': Artifically hides the window by placing it in the corner of the
  #   monitor. On macOS, this is always used instead of cloak/hide.
  hide_method: 'cloak'
```
— [`resources/assets/sample-config.yaml`](https://github.com/glzr-io/glazewm/blob/main/resources/assets/sample-config.yaml)

The defaults are compiled per platform in [`packages/wm-common/src/parsed_config.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-common/src/parsed_config.rs): `HideMethod::PlaceInCorner` under `#[cfg(target_os = "macos")]`, `HideMethod::Cloak` otherwise.

The switch itself lives in `redraw_containers` in [`packages/wm/src/commands/general/platform_sync.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/general/platform_sync.rs), which transitions each window's `DisplayState` from the workspace's `is_displayed()` and then dispatches:

```rust
if config.value.general.hide_method == HideMethod::Cloak {
  window.native().set_cloaked(!is_visible)?;
} else if is_visible {
  window.native().show()?;
} else {
  window.native().hide()?;
}
```

`set_cloaked` in [`packages/wm-platform/src/platform_impl/windows/native_window.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-platform/src/platform_impl/windows/native_window.rs) is the identical sequence to komorebi's: `application_view_collection()` → `get_view_for_hwnd` → `view.set_cloak(1, 2 | 0)`. The interfaces are declared in [`packages/wm-platform/src/platform_impl/windows/com.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-platform/src/platform_impl/windows/com.rs) with the same IIDs komorebi uses, and the source comments say so in plain words: `IApplicationViewCollection` is annotated *"Undocumented COM interface for Windows shell functionality"* and `IApplicationView` *"Undocumented COM interface for managing views in the Windows shell"*. `show`, `hide`, and `minimize` in the same file are plain `ShowWindowAsync` with `SW_SHOWNA`, `SW_HIDE`, and `SW_MINIMIZE`.

**`place_in_corner` is the only mechanism in this entire survey that hides a window on Windows using nothing but public APIs**, and it is worth reading exactly what it does, because it is not "move off-screen":

```rust
if config.value.general.hide_method == HideMethod::PlaceInCorner && !is_visible {
    const VISIBLE_SLIVER: i32 = 1;
    ...
    let position_y = monitor_rect.bottom - VISIBLE_SLIVER;
    let position_x = match hide_corner {
      HideCorner::BottomLeft => monitor_rect.left + VISIBLE_SLIVER - frame.width(),
      HideCorner::BottomRight => monitor_rect.right - VISIBLE_SLIVER,
    };
```
— `reposition_window`, [`platform_sync.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/general/platform_sync.rs)

The window is pushed to a monitor corner such that exactly one pixel remains inside the work area. GlazeWM also picks the corner per monitor (`state.monitors_by_hide_corner()`) so that the parked windows spill toward a screen edge that has no adjacent display.

The undocumented cloak also has a documented fragility beyond the IID: the cached COM pointer goes stale when `explorer.exe` restarts. GlazeWM had to add a retry for it in [PR #1273, "fix: explorer restart fix due to COM interface becoming stale"](https://github.com/glzr-io/glazewm/pull/1273) — the cached interface becomes unusable after an Explorer restart and windows then silently fail to cloak.

### GlazeWM's crash story: no recovery path, and a four-year-old issue to prove it

GlazeWM's exit cleanup is the `Drop for WmState` impl in [`packages/wm/src/wm_state.rs`](https://github.com/glzr-io/glazewm/blob/v3.10.1/packages/wm/src/wm_state.rs) (identical in the shipping `v3.10.1` tag and on `main`). It calls `set_frame`, then, on Windows, `window.native().show()`, `set_taskbar_visibility(true)`, `set_border_color(None)`, and `set_transparency(...)`. It does **not** call `set_cloaked(false)`. I checked every `.rs` file under `packages/wm/src` at `v3.10.1`: `set_cloaked` appears exactly once in the entire package, in `platform_sync.rs`. And GlazeWM's own trait documentation states the consequence:

```rust
  /// Shows the window asynchronously.
  ///
  /// NOTE: Cloaked windows do not get shown until uncloaked.
  fn show(&self) -> crate::Result<()>;
```
— [`packages/wm-platform/src/native_window.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-platform/src/native_window.rs)

Restarting the manager does not repair it either, because a cloaked window is not manageable. `check_is_manageable` in [`packages/wm/src/commands/window/manage_window.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/window/manage_window.rs) begins `if !native_window.is_visible()? { return Ok(None); }`, and `is_visible` in the Windows impl is `IsWindowVisible(hwnd) && !self.is_cloaked()?`, where `is_cloaked` reads `DWMWA_CLOAKED`. A cloaked window is invisible to GlazeWM's own enumeration, so a fresh GlazeWM will not adopt it and will never uncloak it.

The user-facing consequences are documented in the tracker:

- [glzr-io/glazewm#79, "Restore window/workspace state on next startup after a forceful exit"](https://github.com/glzr-io/glazewm/issues/79) — opened after a freeze and Task Manager kill: *"all my windows except one window are hidden… After starting GlazeWM again, all the hidden windows remain hidden and I essentially lost all my unsaved progress."* Maintainer lars-berger replied: *"Killing the WM via Task Manager unfortunately prevents it from performing any cleanup."* He also gave the reason a blanket recovery is unsafe: *"Many background processes actually make use of hidden windows (it's common for 100+ hidden windows to be running at once), so unhiding all windows will likely cause these to freak out."* The issue is marked closed but carries a later report against 3.8.1: *"this still happens to me pretty often in 3.8.1 - can't even get guipropview or winlister to reveal the windows either."*
- [glzr-io/glazewm#1358](https://github.com/glzr-io/glazewm/issues/1358), still open and untriaged: clicking the native minimize button on some apps leaves the window cloaked; the reporter states that `wm-redraw`, `wm-reload-config`, switching every workspace, and **exiting GlazeWM entirely** all fail to restore it, and that *"only manual shell COM uncloaking can recover it"* via `IApplicationView.SetCloak(1, 0)` from PowerShell.

That second report matters for two reasons. It is independent confirmation that GlazeWM's exit path does not uncloak, and it shows why third-party rescue tools do not close the gap: utilities like WinLister and GUIPropView list and un-hide `SW_HIDE` windows, but a cloaked window is `WS_VISIBLE` and simply not composited, so they cannot see or fix it.

**Implication for Mosaix:** the recovery cost of a mechanism is not a separate feature to bolt on later; it is determined by the mechanism at the moment you choose it. Cloaking has no in-band undo an external tool can perform. `SW_HIDE` and `SW_MINIMIZE` do — `ShowWindow(SW_RESTORE)` from any process fixes them, which is exactly why komorebi's out-of-process ledger works for those two behaviours. Corner-parking needs no tool at all; the user drags the sliver.

### PowerToys: what Microsoft ships when it constrains itself to public APIs

Microsoft's two window-arrangement modules are the control group, and the answer is stark: **when Microsoft restricts itself to public APIs, it ships no workspace hiding whatsoever.**

**FancyZones** is virtual-desktop *aware* but only ever reads. Its entire virtual-desktop surface is this class:

```cpp
class VirtualDesktop {
public:
    // IVirtualDesktopManager
    bool IsWindowOnCurrentDesktop(HWND window) const;
    std::vector<HWND> GetWindowsFromCurrentDesktop() const;

    // registry
    GUID GetCurrentVirtualDesktopIdFromRegistry() const;
    std::optional<std::vector<GUID>> GetVirtualDesktopIdsFromRegistry() const;
private:
    IVirtualDesktopManager* m_vdManager{nullptr};
```
— [`src/modules/fancyzones/FancyZonesLib/VirtualDesktop.h`](https://github.com/microsoft/PowerToys/blob/main/src/modules/fancyzones/FancyZonesLib/VirtualDesktop.h)

The header's own section comments split the surface into "IVirtualDesktopManager" and "registry" — and there is nothing else. The implementation creates the object with `CoCreateInstance(CLSID_VirtualDesktopManager, ...)` and otherwise reads `CurrentVirtualDesktop` and `VirtualDesktopIDs` out of `HKCU\...\Explorer\VirtualDesktops`, falling back through a per-session key and finally to "the first element from virtual desktop array, which is primary desktop" ([`VirtualDesktop.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/fancyzones/FancyZonesLib/VirtualDesktop.cpp)). Registry-scraping is the workaround Microsoft's own team uses in place of enumeration, and it is read-only.

FancyZones never hides anything. The only `SW_HIDE` in its window code preserves a window that was *already* invisible while rewriting its placement:

```cpp
    else
    {
        placement.showCmd = SW_HIDE;
    }
```
— [`FancyZonesLib/WindowUtils.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/fancyzones/FancyZonesLib/WindowUtils.cpp), in the `else` branch of `if (IsWindowVisible(window))`

**PowerToys Workspaces**, despite the name, is a launch profile, not a workspace switcher. Launching one starts applications and repositions them; existing windows are moved only if "Move existing windows" is enabled. Its window placement is `SetWindowPlacement` with `SW_MINIMIZE` / `SW_RESTORE` / `SW_SHOWMAXIMIZED` chosen to reproduce the *captured* state of each app — `PlacementHelper::SizeWindowToRect` in [`src/modules/Workspaces/WorkspacesWindowArranger/WindowArranger.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/Workspaces/WorkspacesWindowArranger/WindowArranger.cpp). Nothing is hidden and no desktop is switched.

The documentation states the constraint outright, in the FAQ answer explaining why snapped windows are not restored as snapped:

> **"PowerToys uses publicly available APIs and the FancyZones engine under the hood for positioning apps. Unfortunately, this does not include snapping capabilities."**
> — [PowerToys Workspaces, Frequently Asked Questions](https://learn.microsoft.com/en-us/windows/powertoys/workspaces)

The same page also concedes a related limitation that is directly relevant to any placement engine: *"PowerToys cannot tell an app to launch to a specific position. What we can do is launch an app first, and then give an instruction to move and resize it."*

**Implication for Mosaix:** this is the clearest available signal of where the supported boundary sits. Microsoft has two shipping window-arrangement modules, a strong incentive to offer workspaces, direct access to the Windows shell team, and public knowledge of the private interfaces — and it still ships reposition-only, registry-read-only, hide-nothing. Any Mosaix design that hides windows on Windows is by definition outside what Microsoft itself is willing to do with public APIs.

### macOS: AeroSpace deliberately re-implements Spaces; yabai and Amethyst do not

**AeroSpace** is the closest match to Mosaix's constraints, and its reasoning is stated by the maintainer rather than inferred. From the guide's *Emulation of virtual workspaces* section:

> Native macOS Spaces have a lot of problems
> * The animation for Spaces switching is slow
> * You have a limit of Spaces (up to 16 Spaces with one monitor)
> * You can't create/delete/reorder Space and move windows between Spaces with hotkeys (you can only switch between Spaces with hotkeys)
> * **Apple doesn't provide public API to communicate with Spaces (create/delete/reorder/switch Space and move windows between Spaces)**
>
> Since Spaces are so hard to deal with, AeroSpace reimplements Spaces and calls them "Workspaces". The idea is that **if the workspace isn't active then all of its windows are placed outside the visible area of the screen, in the bottom right or left corner.**
>
> — [AeroSpace guide, "Emulation of virtual workspaces"](https://nikitabobko.github.io/AeroSpace/guide#emulation-of-virtual-workspaces) ([source](https://github.com/nikitabobko/AeroSpace/blob/main/docs/guide.adoc))

Note the shape of the argument: the last bullet is the same complaint as the Windows one. The public-API ceiling on native virtual desktops is not a Windows quirk; both platforms have it, and AeroSpace's answer was to stop using the OS feature entirely.

The mechanism matches GlazeWM's `place_in_corner` almost exactly, including the one-pixel offset. `MacWindow.hideInCorner(_:)` in [`Sources/AppBundle/tree/MacWindow.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/tree/MacWindow.swift) stores the window's proportional position, then sets the frame to `nodeMonitor.visibleRect.bottomRightCorner - CGPoint(x: 1, y: 1)` (or the mirrored bottom-left form), with a per-app carve-out for Zoom. `unhideFromCorner()` restores the saved proportion. The docs are explicit that this is a compromise, not concealment:

> For better or worse, macOS doesn't allow to place windows outside the visible area entirely. You will still be able to see a 1 pixel vertical line of "hidden" windows in the bottom right or left corner of your screen. **That means, that if AeroSpace crashes badly you will still be able to manually "unhide" the windows by dragging these few pixels to the center of the screen.**

The residual sliver *is* the recovery mechanism. On top of it, AeroSpace has a real crash path: `interceptTermination` installs signal handlers that call `terminationHandler?.beforeTermination()`, which iterates `MacWindow.allWindowsMap` and re-centres every window on its monitor ([`Sources/AppBundle/util/appBundleUtil.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/util/appBundleUtil.swift)). The same handler runs on internal assertion failure — `dieT` calls `terminationHandler.beforeTermination()` behind a recursion guard before `fatalError` ([`Sources/Common/util/commonUtil.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/Common/util/commonUtil.swift)). This is the code behind the guide's claim that *"when you quit the AeroSpace or when the AeroSpace detects that it's about to crash, AeroSpace will place all windows back to the visible area of the screen."*

The cost is documented too: monitors must be arranged so every display has a free bottom corner, or hidden windows bleed onto the neighbouring screen; and Mission Control renders badly with many windows stacked in a corner (guide, *Proper monitor arrangement* and *A note on mission control*).

AeroSpace's stated design principle is worth quoting verbatim, because it is nearly the same rule as Mosaix's non-goal:

> - "dark magic" (aka "private APIs", "code injections", etc.) must be avoided as much as possible
>   - Right now, AeroSpace uses only a single private API to get window ID of accessibility object `_AXUIElementGetWindow`. Everything else is macOS public accessibility API.
>   - **AeroSpace will never require you to disable SIP (System Integrity Protection).**
>
> — [AeroSpace README](https://github.com/nikitabobko/AeroSpace/blob/main/README.md)

**yabai** takes the opposite position and pays for it. It uses native Spaces, so the OS hides the windows and a yabai crash cannot strand them — but getting there requires injecting a scripting addition into `Dock.app`. The README's Caveats table states: *"System Integrity Protection can be (partially) disabled for yabai to inject a scripting addition into Dock.app for controlling windows with functions that require elevated privileges. This enables control of the window server…"* ([yabai README, Requirements and Caveats](https://github.com/asmvik/yabai#requirements-and-caveats); note the repository has moved from `koekeishiya/yabai` to `asmvik/yabai`). The SIP wiki page enumerates what is gated behind it: *"move/swap/create/destroy space, remove window shadows, enable window transparency, enable window animations, scratchpad windows, control window layers…, sticky windows…, toggle picture-in-picture"*, requiring `csrutil enable --without fs --without debug --without nvram` on Apple Silicon ([Disabling System Integrity Protection](https://github.com/asmvik/yabai/wiki/Disabling-System-Integrity-Protection)). So without SIP changes yabai can still tile and focus, but **cannot create, destroy, move, or swap a Space** — i.e. it loses workspace management specifically.

That is only the injection layer. Even the base build links roughly a hundred private SkyLight symbols declared in [`src/misc/extern.h`](https://github.com/asmvik/yabai/blob/master/src/misc/extern.h) — `SLSMainConnectionID`, `SLSCopySpacesForWindows`, `SLSManagedDisplayGetCurrentSpace`, `SLSCopyManagedDisplaySpaces`, `SLSMoveWindowsToManagedSpace`, `SLSProcessAssignToSpace`, alongside `_AXUIElementGetWindow` and `_AXUIElementCreateWithRemoteToken`. yabai is not a model Mosaix can borrow from at any level.

**Amethyst**, for contrast, uses native Spaces without injection — but still needs private CGS APIs to *see* them, and resorts to input synthesis to *write* them. `CGSpacesInfo` in [`Amethyst/Model/CGInfo.swift`](https://github.com/ianyh/Amethyst/blob/development/Amethyst/Model/CGInfo.swift) calls `CGSCopySpacesForWindows(CGSMainConnectionID(), kCGSAllSpacesMask, ...)` and parses the private managed-display description dictionaries. Moving a window to a Space bottoms out in Silica's `-[SIWindow moveToSpaceWithEvent:]`, which posts a synthetic `kCGEventLeftMouseDown` + `kCGEventLeftMouseDragged` on the window's title bar, then posts the system Space-switch shortcut, waits 0.4 s for the animation, and releases the mouse ([`Silica/Sources/SIWindow.m`](https://github.com/ianyh/Silica/blob/master/Silica/Sources/SIWindow.m)). That is a manager driving Mission Control by remote-controlling the user's cursor — a vivid demonstration of what "use the native feature through public APIs" degrades into when the native feature has no public write API.

### Are workspaces per-monitor or global?

The three designs are genuinely different, and the choice is independent of the hiding mechanism.

- **komorebi: per monitor.** Workspaces are owned by the monitor — `pub workspaces: Ring<Workspace>` on the `Monitor` struct in [`komorebi/src/monitor.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/monitor.rs) — and the switch is `Monitor::update_focused_workspace`, which updates that monitor's focused ring entry only. Each display carries its own numbered set. Cross-monitor window movement is a separate, configurable policy (`cross_monitor_move_behaviour`, default `"Swap"`, per the [Windows configuration schema](https://komorebi.lgug2z.com/reference/komorebi-windows/)).
- **GlazeWM: a global pool, one displayed per monitor.** Workspaces are declared once in the config as a flat list (`workspaces: - name: '1' ... - name: '9'` in the [sample config](https://github.com/glzr-io/glazewm/blob/main/resources/assets/sample-config.yaml)) and attach to a monitor in the container tree; `Monitor::displayed_workspace()` returns the first in focus order and `Monitor::workspaces()` the rest ([`packages/wm/src/models/monitor.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/models/monitor.rs)). A workspace lives on exactly one monitor at a time and can be moved between them. This is the i3 model.
- **AeroSpace: a global pool, one visible per monitor**, and the maintainer defends the choice explicitly: *"The pool of workspaces is shared between monitors"*, *"By default, all workspaces are assigned to the 'main' monitor"*, and *"The idea of making pool of workspaces shared is based on the observation that most users have a limited set of workspaces on their secondary monitors"* ([AeroSpace guide, workspaces and monitors](https://github.com/nikitabobko/AeroSpace/blob/main/docs/guide.adoc)).

The Windows managers do not agree, so there is no field consensus to inherit. What both agree on is that a workspace is displayed on **at most one monitor at a time**, which is what makes "hide the ones that aren't displayed" a well-defined operation at all.

## What this implies for Mosaix

**No. No manager has found a way to get real workspace isolation on Windows using only public APIs.** That is the direct answer to the crux question, and every branch of the evidence converges on it.

1. **Mechanism 1 (native Windows Virtual Desktops) is not viable — not "risky", but unavailable.** The public `IVirtualDesktopManager` has three methods and cannot create, enumerate, or switch a desktop. Microsoft's own PowerToys maintainer states this on Microsoft's own tracker, names the private `IVirtualDesktopManagerInternal::SwitchDesktop` as the only route, and observes that its GUID "changes build-to-build (as it should)". The reference implementation he cites ships five separate binaries to track that churn. Microsoft closed the request in July 2026 rather than make the API public. Mosaix cannot build a workspace switcher on this, and the non-goal in `ARCHITECTURE.md` §21 forbids the private route regardless.

2. **Mechanism 2 (true hiding) is not what the field actually does, and its best form is undocumented.** Both mature Windows tilers default to `IApplicationView::SetCloak` through `CLSID_ImmersiveShell`, and both label it undocumented in their own source. The genuinely public option, `ShowWindow(SW_HIDE)`, is what they moved *away* from: komorebi marks it `#[deprecated]` / "END OF LIFE FEATURE… has issues with Electron apps"; GlazeWM calls it "Legacy… Has stability issues with some apps". Choosing cloaking would violate the non-goal; choosing `SW_HIDE` means adopting the mechanism two independent maintainers deprecated after shipping it.

3. **Mechanism 3 (minimize) is public, recoverable, and honestly assessed as mediocre.** It is the one mechanism where a hard kill leaves the user in a state any tool — or the taskbar — can repair, which is why komorebi's out-of-process `restore-windows` ledger actually works for it. komorebi's own doc comment is the caveat to plan against: "has issues with frequent workspace switching". It also leaks: minimize is user-visible state, it fires animations, it changes taskbar semantics, and applications observe and react to it.

4. **Mechanism 4 (parking off-screen) is the only mechanism used in production on Windows that is unambiguously public**, and it is the *default* on macOS for both cross-platform managers. In practice it is not truly off-screen: GlazeWM keeps a `VISIBLE_SLIVER: i32 = 1` pixel inside the work area and picks the corner per monitor, and AeroSpace offsets by one pixel and documents that macOS refuses to place a window fully outside the visible area at all. AeroSpace treats the residual sliver as a feature — it is the user's manual recovery path after a bad crash. The costs are equally documented: monitors must be arranged so each has a free bottom corner, and window-overview UI (Mission Control on macOS; by extension Task View and Alt-Tab on Windows) still sees the parked windows.

5. **Crash recovery is chosen when the mechanism is chosen, not afterwards.** Cloaking has no out-of-band undo: a cloaked window is `WS_VISIBLE`, so third-party un-hide tools cannot see it, a restarted GlazeWM skips it in `check_is_manageable`, and GlazeWM's exit path calls `show()` whose own doc comment says it will not uncloak. Four years of glazewm#79 and the still-open #1358 are the user-visible result. Minimize and `SW_HIDE` are repairable by any process with `ShowWindow(SW_RESTORE)`. Corner-parking is repairable by the user with a mouse. If Mosaix wants a recovery story it can honestly promise, it must pick from the second and third groups.

6. **Concretely, the only designs available to Mosaix under its own constraints are:** (a) corner-parking with a deliberate visible sliver, per-monitor corner selection, and a restore-on-crash handler in the mould of AeroSpace's `beforeTermination`; (b) minimize, accepting the animation and taskbar leakage and the "frequent switching" caveat; or (c) no workspaces on Windows in v1. It should not build (a) or (b) without also building komorebi's ledger pattern — a continuously updated on-disk record of every managed window plus an explicit out-of-process restore command — because that is the only crash-recovery design in the survey that was actually validated by users hitting the failure. And whichever is chosen, the workspace model should keep the property all three managers share: a workspace is displayed on at most one monitor at a time, which is what makes "hide everything not displayed" a decidable operation.

## Sources

**Windows — public API surface**

- [IVirtualDesktopManager (shobjidl_core.h)](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ivirtualdesktopmanager) — the complete public interface: three methods (`GetWindowDesktopId`, `IsWindowOnCurrentVirtualDesktop`, `MoveWindowToDesktop`), plus Remarks telling applications not to switch desktops on the user's behalf
- [microsoft/PowerToys#38287](https://github.com/microsoft/PowerToys/issues/38287) — DHowett's statement that desktop switching "is not possible with the public API", the verbatim public interface, `IVirtualDesktopManagerInternal::SwitchDesktop` named as private with a build-to-build GUID, and zadjii-msft's closing comment that the API will not be made public soon
- [MScholtes/VirtualDesktop README](https://github.com/MScholtes/VirtualDesktop/blob/master/README.md) and its per-build sources ([`VirtualDesktop.cs`](https://github.com/MScholtes/VirtualDesktop/blob/master/VirtualDesktop.cs), [`VirtualDesktop11.cs`](https://github.com/MScholtes/VirtualDesktop/blob/master/VirtualDesktop11.cs), [`VirtualDesktopServer2022.cs`](https://github.com/MScholtes/VirtualDesktop/blob/master/VirtualDesktopServer2022.cs)) — five binaries and three different `IVirtualDesktopManagerInternal` IIDs, the measured cost of tracking GUID churn

**Windows — Microsoft's own modules**

- [`FancyZonesLib/VirtualDesktop.h`](https://github.com/microsoft/PowerToys/blob/main/src/modules/fancyzones/FancyZonesLib/VirtualDesktop.h) and [`VirtualDesktop.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/fancyzones/FancyZonesLib/VirtualDesktop.cpp) — FancyZones' entire virtual-desktop surface: public `IVirtualDesktopManager` plus read-only registry scraping of `CurrentVirtualDesktop` / `VirtualDesktopIDs`
- [`FancyZonesLib/WindowUtils.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/fancyzones/FancyZonesLib/WindowUtils.cpp) — the only `SW_HIDE` in FancyZones' placement path, used to preserve an already-invisible window, not to hide one
- [PowerToys Workspaces documentation](https://learn.microsoft.com/en-us/windows/powertoys/workspaces) — what a workspace is (a launch profile), "Move existing windows", and the FAQ statement "PowerToys uses publicly available APIs and the FancyZones engine under the hood for positioning apps"
- [`WorkspacesWindowArranger/WindowArranger.cpp`](https://github.com/microsoft/PowerToys/blob/main/src/modules/Workspaces/WorkspacesWindowArranger/WindowArranger.cpp) — `PlacementHelper::SizeWindowToRect`, the whole of Workspaces' window manipulation: `SetWindowPlacement` with the captured `SW_MINIMIZE`/`SW_RESTORE`/`SW_SHOWMAXIMIZED`

**komorebi**

- [`komorebi/src/core/mod.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/core/mod.rs) — the `HidingBehaviour` enum with the maintainer's verdict on each mechanism (`Hide` deprecated/end-of-life, `Minimize` problematic under frequent switching, `Cloak` undocumented)
- [`komorebi/src/window.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/window.rs) — `hide_with_border` / `restore_with_border`, the three-way dispatch, and the `HIDDEN_HWNDS` bookkeeping list
- [`komorebi/src/com/mod.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/com/mod.rs) and [`komorebi/src/com/interfaces.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/com/interfaces.rs) — the `SetCloak` implementation, the AltTabAccessor provenance comment, and the three hard-coded IIDs; no `IVirtualDesktopManagerInternal`
- [`komorebi/src/lib.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/lib.rs) — `HIDING_BEHAVIOUR` defaulting to `Cloak`; `current_virtual_desktop()` reading the Win10/Win11 registry keys and the comment on when the value does not exist
- [`komorebi/src/workspace.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/workspace.rs) and [`komorebi/src/monitor.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/monitor.rs) — `Workspace::hide` / `Workspace::restore`; `Monitor { workspaces: Ring<Workspace> }` establishing per-monitor workspaces
- [`komorebi/src/main.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/main.rs) and [`komorebi/src/window_manager.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/window_manager.rs) — the Ctrl-C handler calling `restore_all_windows(false)`; the `restore_all_windows` body; the per-event write of `komorebi.hwnd.json`
- [`komorebic/src/main.rs`](https://github.com/LGUG2Z/komorebi/blob/master/komorebic/src/main.rs) — `Stop` ("restore all hidden windows") with `--ignore-restore`; `RestoreWindows` replaying `komorebi.hwnd.json`; `restore_window` implemented as `ShowWindow(SW_RESTORE)` with no uncloak anywhere in the CLI
- [komorebi Windows configuration schema](https://komorebi.lgug2z.com/reference/komorebi-windows/) — `window_hiding_behaviour` default `"Cloak"`; `cross_monitor_move_behaviour` default `"Swap"`

**GlazeWM**

- [`resources/assets/sample-config.yaml`](https://github.com/glzr-io/glazewm/blob/main/resources/assets/sample-config.yaml) — the `hide_method` block ranking `cloak` / `hide` / `place_in_corner`, the macOS note, `show_all_in_taskbar`, and the flat `workspaces:` list
- [`packages/wm-common/src/parsed_config.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-common/src/parsed_config.rs) — per-platform `HideMethod` defaults: `PlaceInCorner` on macOS, `Cloak` elsewhere
- [`packages/wm/src/commands/general/platform_sync.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/general/platform_sync.rs) — `redraw_containers` display-state transition, the cloak/show/hide dispatch, and `reposition_window`'s `VISIBLE_SLIVER: i32 = 1` corner-parking
- [`packages/wm-platform/src/platform_impl/windows/com.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-platform/src/platform_impl/windows/com.rs) — `IApplicationViewCollection` and `IApplicationView` with their IIDs and the "Undocumented COM interface" comments
- [`packages/wm-platform/src/platform_impl/windows/native_window.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-platform/src/platform_impl/windows/native_window.rs) — `set_cloaked`; `show`/`hide`/`minimize` as `ShowWindowAsync(SW_SHOWNA/SW_HIDE/SW_MINIMIZE)`; `is_visible` excluding `DWMWA_CLOAKED` windows
- [`packages/wm-platform/src/native_window.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-platform/src/native_window.rs) — the `show()` doc comment "NOTE: Cloaked windows do not get shown until uncloaked"
- [`packages/wm/src/wm_state.rs` @ v3.10.1](https://github.com/glzr-io/glazewm/blob/v3.10.1/packages/wm/src/wm_state.rs) — the `Drop for WmState` cleanup that calls `show()` but never `set_cloaked(false)`
- [`packages/wm/src/commands/window/manage_window.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/window/manage_window.rs) — `check_is_manageable` rejecting any window that is not `is_visible()`, i.e. any cloaked window
- [`packages/wm/src/models/monitor.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/models/monitor.rs) — `displayed_workspace()` / `workspaces()`, establishing one displayed workspace per monitor from a shared pool
- [glzr-io/glazewm#79](https://github.com/glzr-io/glazewm/issues/79) — the forceful-exit data loss report, lars-berger on why cleanup cannot run after a Task Manager kill and why blanket unhiding is unsafe, LGUG2Z's description of komorebi's ledger design, and the later 3.8.1 recurrence
- [glzr-io/glazewm#1358](https://github.com/glzr-io/glazewm/issues/1358) — windows left cloaked by native minimize, unrecoverable by redraw, reload, workspace switching, or exiting GlazeWM; only manual `IApplicationView.SetCloak(1, 0)` recovers them
- [glzr-io/glazewm#1273](https://github.com/glzr-io/glazewm/pull/1273) — the cached shell COM interface going stale after an `explorer.exe` restart, and the retry added to work around it

**macOS**

- [AeroSpace guide, "Emulation of virtual workspaces"](https://nikitabobko.github.io/AeroSpace/guide#emulation-of-virtual-workspaces) / [`docs/guide.adoc`](https://github.com/nikitabobko/AeroSpace/blob/main/docs/guide.adoc) — the four stated problems with native Spaces including the missing public API, the corner-parking mechanism, the 1 px sliver as manual recovery, the monitor-arrangement requirement, the Mission Control caveat, and the shared-pool workspace model
- [AeroSpace README](https://github.com/nikitabobko/AeroSpace/blob/main/README.md) — the "no dark magic" design principle, `_AXUIElementGetWindow` as the single private API, and the commitment never to require disabling SIP
- [`Sources/AppBundle/tree/MacWindow.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/tree/MacWindow.swift) — `hideInCorner(_:)` / `unhideFromCorner()`, the one-pixel offset, and the saved proportional position
- [`Sources/AppBundle/util/appBundleUtil.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/AppBundle/util/appBundleUtil.swift) and [`Sources/Common/util/commonUtil.swift`](https://github.com/nikitabobko/AeroSpace/blob/main/Sources/Common/util/commonUtil.swift) — `interceptTermination`, `beforeTermination()` re-centring every window, and `dieT` invoking it before `fatalError`
- [yabai README, Requirements and Caveats](https://github.com/asmvik/yabai#requirements-and-caveats) — the scripting-addition/SIP caveat and the "Displays have separate Spaces" requirement (repository moved from `koekeishiya/yabai` to `asmvik/yabai`)
- [yabai wiki, Disabling System Integrity Protection](https://github.com/asmvik/yabai/wiki/Disabling-System-Integrity-Protection) — the exact feature list gated behind SIP changes, including "move/swap/create/destroy space", and the `csrutil` commands per architecture
- [`src/misc/extern.h`](https://github.com/asmvik/yabai/blob/master/src/misc/extern.h) — the private SkyLight/CoreGraphics symbols yabai links even without the scripting addition
- [Amethyst `Model/CGInfo.swift`](https://github.com/ianyh/Amethyst/blob/development/Amethyst/Model/CGInfo.swift) and [`Managers/WindowTransitionCoordinator.swift`](https://github.com/ianyh/Amethyst/blob/development/Amethyst/Managers/WindowTransitionCoordinator.swift) — private `CGSCopySpacesForWindows` / `CGSMainConnectionID` space enumeration and the `moveWindowToSpaceAtIndex` transition
- [Silica `SIWindow.m`](https://github.com/ianyh/Silica/blob/master/Silica/Sources/SIWindow.m) — `moveToSpaceWithEvent:` moving a window between Spaces by synthesizing a title-bar mouse drag plus the system Space-switch shortcut
