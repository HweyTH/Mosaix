# Research: Should Mosaix keep workspace switching, narrow it, defer it, or drop it?

> **TL;DR**: Keep the container tree and the logical workspace model, but do not promise seamless workspace switching as a supported cross-platform Phase 3 feature. Ship the tree first over the currently visible windows, after introducing the SQLite state database it needs for durable tree/undo state. Then prototype one explicitly experimental, public-API-only switcher based on reversible window parking—not Windows Virtual Desktops, `SW_HIDE`, minimizing, or private cloaking—and graduate it only if it passes crash recovery, multi-monitor, taskbar/Alt-Tab, and application-compatibility tests on both platforms. Otherwise leave switching experimental and ship the stronger tiling product. Dropping all workspace concepts would discard useful grouping and future restoration seams; shipping private APIs would contradict Mosaix's security/distribution boundary and create an open-ended OS-version compatibility obligation.

## Findings

### “Workspace” is two features, not one

Mosaix currently bundles two separable capabilities under one word:

1. A **logical model**: each monitor has a root container, nested layout nodes, ordered windows, weights, gaps, and workspace identity. This powers BSP/stack/monocle layout, directional commands, serialization, and later scene restoration.
2. A **visibility mechanism**: focusing workspace B makes workspace A's windows cease to occupy the usable screen, then restores them exactly when A returns.

The first capability needs no private OS behavior. Mosaix already moves and resizes visible windows, and Windows publicly exposes position and Z-order changes through `SetWindowPos` ([Microsoft, `SetWindowPos`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos)). macOS tilers likewise build trees over windows using Accessibility positioning; AeroSpace documents its tiling tree separately from its later workspace-emulation section ([AeroSpace guide, “Tree” and “Emulation of virtual workspaces”](https://nikitabobko.github.io/AeroSpace/guide#tree)).

The second capability is where the platform boundary appears. Treating both as one deliverable makes an undocumented hiding primitive a prerequisite for ordinary tree tiling when it is not.

### Native Windows Virtual Desktops cannot provide supported switching

The complete public `IVirtualDesktopManager` surface has three methods: determine a window's desktop ID, test whether it is on the current desktop, and move it to a specified desktop. It has no method to enumerate, create, delete, name, reorder, or activate desktops. Microsoft also tells applications that only the user should initiate switching ([Microsoft, `IVirtualDesktopManager`](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ivirtualdesktopmanager)).

This is not merely an omission in the documentation. A PowerToys maintainer recorded that switching is unavailable through the public API, identified private `IVirtualDesktopManagerInternal::SwitchDesktop` as the alternative, and noted that its GUID changes between Windows builds ([PowerToys #38287](https://github.com/microsoft/PowerToys/issues/38287)). Therefore a Mosaix-owned native desktop switcher would require exactly the private, version-sensitive integration that `ARCHITECTURE.md` excludes.

The narrower public `MoveWindowToDesktop` method is not a substitute. It requires the target desktop GUID, while the public interface cannot enumerate desktops or activate the target. It can support limited cooperation with desktops the user already owns, but not Mosaix's named, per-monitor workspace contract.

### Undocumented cloaking is effective, but creates a permanent support obligation

The mature Windows tilers converge on shell cloaking because the public substitutes are worse. GlazeWM's default configuration recommends `cloak`, calls `hide` a legacy option with application stability problems, and describes `place_in_corner` as an artificial fallback ([GlazeWM sample configuration](https://github.com/glzr-io/glazewm/blob/main/resources/assets/sample-config.yaml)). Its Windows implementation declares raw `IApplicationViewCollection` and `IApplicationView` vtables as “Undocumented COM interface[s]” and calls `set_cloak` through them ([GlazeWM `com.rs`](https://github.com/glzr-io/glazewm/blob/main/packages/wm-platform/src/platform_impl/windows/com.rs)).

komorebi independently exposes the same trade-off in code: `SW_HIDE` is end-of-life because of Electron problems, `SW_MINIMIZE` has problems under frequent switching, and `Cloak` calls an undocumented function ([komorebi `HidingBehaviour`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/core/mod.rs#L2414-L2431)).

Using cloaking would consequently require Mosaix to:

- test every supported Windows build and react to shell-interface changes;
- maintain guarded version detection and a safe fallback;
- persist enough information for out-of-process recovery when the manager is killed;
- ship a repair command capable of reversing the exact cloaking operation; and
- accept that a Windows update can strand windows or disable the feature before Mosaix ships a fix.

Those are continuing product obligations, not a one-time implementation cost. They also conflict with the existing non-goal on private APIs rather than merely stretching it.

### Public Windows approximations are supported primitives with unsupported product semantics

Windows publicly offers three relevant operations:

- `SW_HIDE` removes a window from display and activates another;
- `SW_MINIMIZE` minimizes it and activates the next top-level window; and
- `SetWindowPos` can park it at a chosen position while controlling activation and Z-order.

The first two behaviors are documented by Microsoft ([Microsoft, `ShowWindow`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-showwindow)); the positioning behavior is documented by `SetWindowPos` ([Microsoft, `SetWindowPos`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos)). Public API status does not make the resulting workspace experience sound:

- **Hide** changes another application's show state. Real applications may treat hidden windows specially, and komorebi/GlazeWM both record compatibility failures.
- **Minimize** exposes every workspace change through taskbar state and animations, changes focus/Z-order, and can trigger application-specific minimize behavior. komorebi explicitly warns about frequent switching.
- **Corner/off-screen parking** preserves show state and is the easiest mechanism to reverse after a crash, but inactive windows can remain visible in task switchers, focus can escape to them, popups/owned windows may not follow, and display topology changes can turn an old parking coordinate into a visible location.

Only parking fails “open”: if the manager dies, the windows still exist in an ordinary movable state. That makes it the only acceptable public-API candidate for an experiment, provided Mosaix records every pre-park placement before moving the first window and offers an out-of-process restore command.

### macOS has the same boundary, with parking as the demonstrated public approach

AeroSpace states that Apple exposes no public API to create, delete, reorder, switch, or move windows between Spaces. It therefore implements its own logical workspaces by moving inactive windows to a bottom corner and restoring them later ([AeroSpace guide, “Emulation of virtual workspaces”](https://nikitabobko.github.io/AeroSpace/guide#emulation-of-virtual-workspaces)). This is the closest maintained implementation to Mosaix's public-API constraint, and its documentation makes the UX costs concrete:

- macOS leaves a one-pixel strip visible, which doubles as manual crash recovery;
- some monitor arrangements expose parked windows on another display;
- Mission Control presents parked windows poorly; and
- native Spaces plus separate per-display Spaces introduce focus and stability limitations for an Accessibility-based manager.

AeroSpace restores parked windows on normal termination and when it detects an impending crash, but a hard kill still relies on the visible sliver or subsequent repair ([same guide](https://nikitabobko.github.io/AeroSpace/guide#emulation-of-virtual-workspaces)).

The native-Spaces alternative violates Mosaix's boundary more severely. yabai documents that moving, swapping, creating, or destroying Spaces requires injecting a scripting addition into `Dock.app` and partially disabling System Integrity Protection ([yabai, “Disabling System Integrity Protection”](https://github.com/asmvik/yabai/wiki/Disabling-System-Integrity-Protection)). This is incompatible with Mosaix's stated no-injection/no-weakened-SIP position.

### Cross-platform parity exists only at the logical and parking layers

Native desktop integration would produce different products: Windows public APIs cannot switch desktops, while macOS public APIs cannot control Spaces. Private integrations would also differ in failure and distribution characteristics. A shared command such as `workspace focus dev` could not truthfully promise the same behavior.

The portable layers are:

- a platform-neutral workspace/container tree;
- a single visible tree per monitor;
- durable tree identity and pre-switch placement state; and
- a best-effort parking transaction with platform-specific coordinates and recovery.

That is enough to keep one domain model and adapter contract. It is not enough to call switching seamless. The product should label the mechanism, limitations, and recovery action rather than hiding them behind a generic “native workspace” setting.

### SQLite should precede any switching experiment

`ARCHITECTURE.md` §12.2 assigns workspace trees, placement/undo history, restoration metadata, migrations, and onboarding state to SQLite. None of those needs a native window handle to survive across sessions; the architecture explicitly forbids persisting handles. This is the right separation: persist logical identities and geometry in SQLite, while keeping the current session's native handles in a short-lived recovery ledger.

Switching raises the consequence of lost state from “layout resets” to “the user's windows appear missing.” Therefore the database is not merely Phase 4 infrastructure once switching is considered. Before an experimental switcher, Mosaix needs:

1. transactional tree persistence and schema migrations;
2. a write-ahead recovery record containing each currently parked window and its pre-park rectangle/show state;
3. startup reconciliation that never assumes a stale native handle is the same window; and
4. a separate repair command that can restore session handles after the UI/agent crashes.

The durable database can support tree history and later identity matching. The volatile recovery ledger protects the current session. Conflating them would either persist invalid handles or make hard-kill recovery impossible.

## Recommendation

### Decision

**Retain workspaces as a long-term logical feature; narrow Phase 3 to the container tree; defer supported switching; build and evaluate public parking behind an experimental capability flag after SQLite and recovery exist. Do not use private APIs.**

This is a deliberate narrowing, not dropping the feature. It preserves the valuable structure while refusing to make the least reliable mechanism the foundation of automatic tiling.

### Revised phase order

1. **State foundation (move forward from Phase 4):** introduce SQLite migrations, tree/undo persistence, onboarding capability state, and the separate current-session recovery ledger.
2. **Phase 3A — container-tree tiling:** implement nested containers, weights, normalization, layouts, focus/move/resize, and one visible root per monitor. Do not expose multiple switchable workspaces yet. Internally, avoid assuming there can only ever be one workspace.
3. **Phase 3B — experimental logical switching:** implement named workspaces with parking only. Make it opt-in, clearly label it experimental, show a persistent “restore all windows” action, and restore before disabling/uninstalling/upgrading.
4. **Graduation gate:** call switching supported only after it passes the matrix below on both Windows and macOS. If it fails, leave it experimental or remove the switcher without removing the tree model.
5. **Phase 4:** build identity-based scene restoration on the now-existing database. Treat optional native-desktop cooperation as a separate adapter capability, never as the cross-platform workspace implementation.

### Graduation matrix for parking

The experiment should not graduate unless all of these are demonstrated in live tests:

- graceful exit, crash, force-kill, upgrade, disable, and uninstall restore every managed window;
- sleep/wake, display disconnect/reconnect, DPI/resolution changes, and monitor rearrangement cannot strand windows;
- taskbar, Alt-Tab/Command-Tab, Mission Control, app activation, dialogs, transient/owned windows, full-screen windows, and minimized windows have documented behavior;
- focus cannot silently land on a parked window;
- Electron, browsers, IDEs, terminals, Office apps, and multi-window native apps survive rapid repeated switching;
- a repair command works without the main agent running and only touches windows in Mosaix's verified session ledger; and
- the UI never describes emulated parking as native virtual desktops or Spaces.

### Documentation correction now

`CONTEXT.md` currently opens by promising “per-monitor workspaces,” while the implemented product and current ADRs defer them. Until switching graduates, change that sentence to promise manual snapping and automatic tiling, and describe workspaces as a researched, deferred capability. `ARCHITECTURE.md` should likewise split §7.3 into “container tree” and “workspace visibility mechanism,” because they no longer share a delivery or platform-risk profile.

## Sources

- [Microsoft: `IVirtualDesktopManager`](https://learn.microsoft.com/en-us/windows/win32/api/shobjidl_core/nn-shobjidl_core-ivirtualdesktopmanager) — the complete supported Windows virtual-desktop surface and guidance that switching should be user-instigated
- [Microsoft: `ShowWindow`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-showwindow) — public hide, minimize, show, and restore semantics
- [Microsoft: `SetWindowPos`](https://learn.microsoft.com/en-us/windows/win32/api/winuser/nf-winuser-setwindowpos) — public positioning, Z-order, activation, hide, and show semantics
- [Microsoft PowerToys #38287](https://github.com/microsoft/PowerToys/issues/38287) — first-party confirmation that switching is private and its interface identity changes across Windows builds
- [GlazeWM sample configuration](https://github.com/glzr-io/glazewm/blob/main/resources/assets/sample-config.yaml) — maintained product ranking of cloak, hide, and corner-parking behavior
- [GlazeWM Windows COM implementation](https://github.com/glzr-io/glazewm/blob/main/packages/wm-platform/src/platform_impl/windows/com.rs) — raw undocumented shell interfaces used for cloaking
- [komorebi `HidingBehaviour`](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/core/mod.rs#L2414-L2431) — maintained implementation's compatibility assessment of hide, minimize, and cloak
- [AeroSpace guide: workspace emulation](https://nikitabobko.github.io/AeroSpace/guide#emulation-of-virtual-workspaces) — public-API macOS approach, recovery behavior, visible sliver, monitor geometry, Mission Control, and native-Spaces limitations
- [yabai: Disabling System Integrity Protection](https://github.com/asmvik/yabai/wiki/Disabling-System-Integrity-Protection) — injection and SIP requirements for native Space manipulation
