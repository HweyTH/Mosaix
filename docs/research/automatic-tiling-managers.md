# Research: What should Mosaix ship first for automatic tiling?

> **TL;DR**: The representative Windows tools split into two camps. GlazeWM and komorebi make a continuously maintained window model the centre of the product, with tiling/floating state, rules, pause controls, workspaces, directional focus/movement, and configurable gaps. FancyZones is deliberately different: it presents user-authored zones and moves a window only after a drag or keyboard action; it does not continuously reflow all windows. Mosaix should preserve that distinction. Its first automatic-tiling release should be an opt-in, per-topology **Balanced grid** over each display's **Active tiling set**, with stable ordering, deterministic reflow, existing rules/gaps/config reload, pause, toggle-floating, directional focus, directional swap, and an explicit manual-drag contract. It should not introduce workspace/container trees, multiple automatic layouts, adjustable weights, or persistence yet. While automatic tiling is enabled, an edge drop should float that window for the session and honor the existing drag-to-snap placement; otherwise automatic reflow would immediately undo the user's drop.

## Findings

### Scope and source selection

This is a comparison of representative, actively documented Windows approaches, not a popularity ranking. GlazeWM and komorebi are automatic tilers that extend the Windows desktop rather than replace DWM; FancyZones is the first-party Microsoft contrast for manual zone snapping. The GlazeWM repository describes the project as a Windows/macOS tiling manager and publishes current releases, while the komorebi repository describes itself as a tiling extension to DWM with a command-line control surface ([GlazeWM repository](https://github.com/glzr-io/glazewm), [komorebi repository](https://github.com/LGUG2Z/komorebi)).

### Automatic tiling and manual snapping are different promises

GlazeWM and komorebi own a live arrangement: opening, closing, moving, floating, or minimizing a managed window changes the arrangement of the other managed windows. Their public controls operate on window-manager state: focus, move, resize, float, pause, workspace, and layout.

FancyZones owns a zone map, not an automatic inventory. It reveals zones during a drag or responds to a keyboard command, and it supports putting multiple windows in one zone without reflowing unrelated windows. Its settings can keep already-zoned windows associated with zones across resolution or layout changes, and can remember an application's last zone, but those are placement conveniences rather than automatic tiling ([FancyZones usage and settings](https://learn.microsoft.com/windows/powertoys/fancyzones#settings)).

This distinction matters for Mosaix because its existing drag-to-snap path commits a half-zone placement. If that same window remains in an automatic **Active tiling set**, the next automatic plan will overwrite the half-zone. The two features need a visible state transition, not competing placement writers.

### Layout policies and sensible defaults

GlazeWM uses nested horizontal/vertical tiling direction rather than a catalogue of automatic layouts. The direction on the target container determines where a new or moved window is inserted, and the official FAQ explicitly says native automatic layout selection is not supported ([GlazeWM FAQ: custom layouts](https://github.com/glzr-io/glazewm#faq)). This gives the user direct structural control, but makes a tree and its editing commands part of the minimum mental model.

komorebi offers BSP, vertical/horizontal stacks, columns, rows, ultrawide variants, and Grid. It can also switch layout automatically at configured container-count thresholds; the official example shows BSP for 1–3 containers, an ultrawide layout for 4–6, and Grid from 7 onward ([komorebi layouts](https://komorebi.lgug2z.com/usage/layouts/), [official example configurations](https://github.com/LGUG2Z/komorebi/blob/master/docs/example-configurations.md#layouts)). This breadth is powerful, but each policy introduces policy-specific behavior and options. The same official examples note that Grid does not support resizing tile dimensions.

FancyZones ships editable Grid and Canvas zone models, plus orientation-specific default layouts. Grid is relative and split/merge based; Canvas permits independently sized, overlapping zones ([FancyZones layout editor](https://learn.microsoft.com/windows/powertoys/fancyzones#create-a-custom-layout)). Those are good precedents for Mosaix's later manual layout editor, but they do not justify adding multiple automatic planners to the first release.

**Implication for Mosaix:** ship only **Balanced grid** first. It already has a pinned meaning in `CONTEXT.md`, is stateless, and can cover the work area without introducing a persistent container tree. Use one deterministic algorithm and do not expose `layout = "balanced-grid"` as a meaningless one-choice setting. A layout selector becomes useful only when a second automatic policy exists.

### Window eligibility, floating, and rules

GlazeWM defaults new windows to either `tiling` or `floating`, supports per-window commands to toggle those states, and matches window rules by process, title, and class. Its sample configuration also exposes a global pause binding ([GlazeWM window behavior and rules](https://github.com/glzr-io/glazewm#config-window-behavior), [GlazeWM sample configuration](https://github.com/glzr-io/glazewm/blob/main/resources/assets/sample-config.yaml)).

komorebi distinguishes ignored windows from floating windows: ignored windows are outside command targeting, while floating windows remain in a managed floating layer. It supports matching by executable, title, class, and composite conditions, plus force-manage and application-specific handling for difficult Windows apps ([komorebi rules](https://komorebi.lgug2z.com/usage/rules/)). It also exposes global/workspace float overrides and a focused-window `toggle-float` command ([komorebi layers](https://komorebi.lgug2z.com/usage/layers/), [komorebic command reference](https://komorebi.lgug2z.com/reference/komorebic-windows/)).

FancyZones uses a simpler application-name exclusion list and has explicit caveats for elevated, popup, child, non-DPI-aware, and application-specific windows ([FancyZones exclusions and compatibility](https://learn.microsoft.com/windows/powertoys/fancyzones#application-compatibility)). The compatibility list is evidence that eligibility cannot be inferred once and assumed forever.

**Implication for Mosaix:** reuse the existing `ManageAction::{Tile, Float, Exclude}` model rather than inventing automatic-layout-specific filters. A **Managed window** enters the **Active tiling set** only when it is movable, resizable, active (not minimized), non-elevated, and has no open placement circuit. `Float` keeps the window observed and command-addressable but outside the grid; `Exclude` keeps it outside management. These semantics should be inspectable in logs/state using a reason such as `rule-float`, `minimized`, `elevated`, or `placement-circuit-open`.

### Insertion, removal, and ordering

The automatic tilers make insertion semantics visible because they influence every subsequent layout. GlazeWM exposes tiling direction specifically because it determines where new windows are inserted ([GlazeWM sample configuration: `toggle-tiling-direction`](https://github.com/glzr-io/glazewm/blob/main/resources/assets/sample-config.yaml)). komorebi exposes preselection, move, promote, stack, and layout-specific controls through its CLI, again making ordering a first-class state concern ([komorebic command reference](https://komorebi.lgug2z.com/reference/komorebic-windows/), [komorebi windows](https://komorebi.lgug2z.com/usage/windows/)).

Mosaix does not need that full insertion model for a stateless grid. It should:

- seed startup order with **Visual window order**;
- preserve that order while the same windows remain observed;
- append a newly eligible window after the existing ordered members on its display;
- remove an ineligible/closed window without reordering survivors;
- restore a temporarily ineligible observed window to its prior order slot when it becomes eligible again;
- let directional swap be the only first-release way to alter order deliberately.

This prevents native enumeration order or transient focus changes from shuffling every window. It also makes opening and closing a window predictable without exposing an insertion-policy setting.

### Focus, swap, and resize controls

Both automatic tilers treat directional keyboard control as core. GlazeWM's default configuration binds directional focus/move and percentage resize, with a dedicated resize binding mode ([GlazeWM default keybindings and binding modes](https://github.com/glzr-io/glazewm#default-keybindings)). komorebi exposes directional focus and move, cyclic alternatives, promotion to the largest tile, and resize by edge or axis with a configurable delta ([komorebi windows](https://komorebi.lgug2z.com/usage/windows/), [komorebic command reference](https://komorebi.lgug2z.com/reference/komorebic-windows/)).

**Implication for Mosaix:** directional focus and directional swap belong in the first usable automatic-tiling release. Without them the user gets automatic geometry but must fall back to Alt-Tab and pointer dragging to navigate or change order. Arbitrary resize does **not** belong in the Balanced-grid release: equal, computed cells have no persistent weights to resize. Adding resize now would either be fake (lost on the next reflow) or silently introduce the tree/weight model that the MVP is trying to avoid.

### Multiple displays and workspaces

GlazeWM predefines named workspaces, assigns them to monitors at startup, and can bind a workspace to a monitor; its commands focus/move workspaces and windows across displays ([GlazeWM workspace configuration](https://github.com/glzr-io/glazewm#config-workspaces)). komorebi models workspaces per monitor, supports named and indexed navigation, can move workspaces between monitors, and makes cross-monitor window behavior configurable as swap, insert, or no-op ([komorebi workspaces](https://komorebi.lgug2z.com/usage/workspaces/), [komorebi monitors](https://komorebi.lgug2z.com/usage/monitors/)). FancyZones assigns layouts per monitor and optionally treats equal-DPI monitors as one spanned surface ([FancyZones settings](https://learn.microsoft.com/windows/powertoys/fancyzones#settings)).

**Implication for Mosaix:** tile each physical display independently and keep the existing **Display migration** contract. Disconnecting a display migrates its windows to the nearest survivor before grids are recomputed; connecting a display does not redistribute existing windows. Per-monitor workspaces, cross-monitor swap/insert policies, and spanning one grid across displays should remain deferred. The existing per-topology `Profile` mechanism is sufficient for turning automatic tiling on or off for a particular monitor setup.

### Configuration, reload, gaps, and visual feedback

GlazeWM uses a human-editable YAML file for behavior, rules, workspaces, keybindings, inner/outer gaps, borders, and commands; its sample config includes an explicit `wm-reload-config` binding and post-reload commands ([GlazeWM configuration](https://github.com/glzr-io/glazewm#config-documentation), [GlazeWM sample configuration](https://github.com/glzr-io/glazewm/blob/main/resources/assets/sample-config.yaml)). komorebi uses schema-described JSON, supports replacing a running configuration, and exposes per-workspace/default container and workspace padding plus optional borders/themes ([komorebi configuration schema](https://komorebi.lgug2z.com/reference/komorebi-windows/), [komorebi themes](https://komorebi.lgug2z.com/usage/themes/)). FancyZones exposes margins, zone colors, opacity, orientation defaults, and a GUI editor ([FancyZones editor and settings](https://learn.microsoft.com/windows/powertoys/fancyzones#get-started-with-the-editor)).

Mosaix already has stronger relevant primitives than a new subsystem needs: `Base config` plus topology `Profile`, whole-directory atomic validation, debounced hot reload, live hotkey rebinding, inner/outer `Gap`, tray pause state, and the snap preview overlay. The required new configuration should therefore stay small and profile-only:

```toml
[automatic_tiling]
enabled = true
```

`enabled` is valid only in a topology profile; base config cannot enable automatic tiling, so a docked topology can tile automatically while an unmatched laptop-only topology remains manual. Existing gaps, hotkeys, and rules remain the customization surfaces. Add commands/bindings for `toggle-automatic-tiling`, `toggle-floating`, `focus-{left,right,up,down}`, `swap-{left,right,up,down}`, and `rearrange` (force reconciliation plus a fresh plan). Do not add knobs for row-count strategy, insertion policy, settling time, edge threshold, circuit-breaker threshold, or per-edge automatic-grid gaps until evidence shows the defaults are wrong.

### Failure, reconciliation, and recovery UX

komorebi exposes a `retile` command, state/query output, logs, pause, and configuration replacement through its CLI ([komorebic command reference](https://komorebi.lgug2z.com/reference/komorebic-windows/)). GlazeWM's default config similarly includes pause, redraw, reload, and exit controls ([GlazeWM sample configuration](https://github.com/glzr-io/glazewm/blob/main/resources/assets/sample-config.yaml)). FancyZones documents concrete incompatibilities, including elevated apps, missing move/size events, and mixed-DPI edge differences rather than pretending all windows obey placements ([FancyZones application compatibility](https://learn.microsoft.com/windows/powertoys/fancyzones#application-compatibility)).

Mosaix should build recovery into the automatic policy:

- A **Placement rejection** immediately removes that window from the next **Active tiling set**, so it cannot reserve a blank cell or cause an endless reflow loop.
- Other windows reflow once; retry/backoff remains per-window, using the existing circuit breaker and 500 ms/2 px correlation rules.
- A failed or empty display observation is not authoritative; retain the last **Usable topology snapshot** and retry reconciliation.
- The tray distinguishes running, paused, and degraded (one or more excluded-by-failure windows). State/log output names each window's exclusion reason.
- `rearrange` provides an explicit recovery action. It re-enumerates, closes eligible temporary circuits, and computes one plan; it must not erase user rules or floating state.

These behaviors are more important to a trustworthy first release than animations or configurable borders.

### Recommended Mosaix MVP

#### Ship in the first automatic-tiling release

1. **One opt-in policy:** profile-only `automatic_tiling.enabled = true`; unmatched topologies are always manual, and the policy is **Balanced grid** only.
2. **Per-display planning:** one independent grid over each display work area, using deterministic integer edge allocation and existing inner/outer gaps.
3. **Stable membership and order:** use the **Managed window**, **Active tiling set**, and **Visual window order** semantics above; preserve order across reflows and temporary ineligibility.
4. **Complete lifecycle reflow:** recompute when a window becomes eligible/ineligible, closes, changes display, or when a display work area/topology changes. Coalesce an event burst into one stable plan.
5. **Safety controls:** existing global pause plus toggle automatic tiling, toggle focused window floating, force `rearrange`, and clear degraded status/retry through that explicit action.
6. **Keyboard usability:** directional focus and directional swap. Swapping changes the stable visual order, then recomputes the two affected placements.
7. **Rules and observability:** reuse `Tile`/`Float`/`Exclude`; publish/log the resolved action and temporary exclusion reason. No second rule language.
8. **Manual drag coexistence:** when automatic tiling is enabled, dropping a managed window into an existing edge drag zone changes it to session-floating and commits the half-zone placement. It leaves the **Active tiling set**, and the remaining windows reflow. A drag ending outside a snap zone does not adopt arbitrary geometry; the window returns to its grid cell. `toggle-floating` is the explicit way back into tiling. This preserves drag-to-snap as a real promise instead of letting automatic reflow undo it.
9. **Topology recovery:** retain **Display migration**, ignore non-usable topology observations, and exclude windows with an open placement circuit without leaving holes.

#### Required customization

- Automatic tiling enabled in a sparse topology profile; base config has no global enable switch.
- Existing inner and outer gaps.
- Existing ordered rules with `Tile`, `Float`, and `Exclude` outcomes.
- Bindings for mode toggle, float toggle, focus, swap, rearrange, and pause.

Everything else should have a documented default rather than a setting. The first release needs confidence and predictability more than a large configuration schema.

#### Deferred features and explicit non-goals

- BSP/container trees; tall, wide, rows, columns, stack, monocle, scrolling, and count-based layout switching.
- Per-monitor workspaces, workspace persistence/restoration, and workspace-to-monitor movement.
- Weighted tile resizing, resize modes, saved resize dimensions, and insertion/preselection policies.
- Custom automatic layouts, layout plugins, and the visual automatic-layout editor.
- Cross-display redistribution when a display is added, grids spanning displays, or configurable cross-monitor swap/insert behavior.
- Focus-follows-pointer, mouse-follows-focus, animations, title-bar changes, transparency, and configurable focus borders.
- Persisting session-floating state across restart. Persistent behavior belongs in an explicit rule; session floating resets on restart.

### Why this cut fits Mosaix

The recommendation takes the usability floor from automatic tilers—stable management, float/pause escape hatches, directional focus/swap, rules, and repair—without importing their most expensive state model. It takes configuration/profile, gaps, overlay, reconciliation, **Display migration**, and **Placement rejection** behavior from mechanisms Mosaix already owns. Balanced grid remains a deep, deterministic planner with a small interface. A later tree-based policy can implement the architecture's broader Phase 3 without forcing the first automatic release to solve tree normalization, weight persistence, multiple layouts, and workspace restoration simultaneously.

## Sources

- [GlazeWM official repository and documentation](https://github.com/glzr-io/glazewm) — project scope, configuration, workspaces, rules, default commands, and the explicit absence of native automatic layout selection
- [GlazeWM official sample configuration](https://github.com/glzr-io/glazewm/blob/main/resources/assets/sample-config.yaml) — defaults for gaps, window states, pause, focus/move/resize, floating, redraw, and config reload
- [komorebi official repository](https://github.com/LGUG2Z/komorebi) — DWM-extension model and CLI-oriented architecture
- [komorebi layouts](https://komorebi.lgug2z.com/usage/layouts/) — layouts, runtime selection, count-based layout rules, and ratios
- [komorebi official example configurations](https://github.com/LGUG2Z/komorebi/blob/master/docs/example-configurations.md) — representative defaults and layout behavior, including Grid's resize limitation
- [komorebi rules](https://komorebi.lgug2z.com/usage/rules/) — matching, ignore, float, force-manage, workspace, and Windows-specific application handling rules
- [komorebi layers](https://komorebi.lgug2z.com/usage/layers/) — tiling/floating layers and new-window behavior
- [komorebi windows](https://komorebi.lgug2z.com/usage/windows/) — directional focus, movement, promotion, and cross-workspace/monitor operations
- [komorebi workspaces](https://komorebi.lgug2z.com/usage/workspaces/) — named/indexed focus and workspace movement
- [komorebi monitors](https://komorebi.lgug2z.com/usage/monitors/) — monitor focus and cross-monitor boundary/move policies
- [komorebi containers](https://komorebi.lgug2z.com/usage/containers/) — stacking and monocle behavior
- [komorebi Windows configuration schema](https://komorebi.lgug2z.com/reference/komorebi-windows/) — defaults and configuration fields for padding, floating, resize, rules, and monitor/workspace settings
- [komorebic Windows command reference](https://komorebi.lgug2z.com/reference/komorebic-windows/) — focus, move, resize, float, pause, retile, configuration replacement, and state/query commands
- [FancyZones official documentation](https://learn.microsoft.com/windows/powertoys/fancyzones) — drag/keyboard zone snapping, editor, per-monitor settings, exclusions, persistence options, and compatibility limitations
- [PowerToys DSC settings reference](https://learn.microsoft.com/windows/powertoys/dsc-configure/psdsc) — machine-configurable FancyZones behavior for moving windows after work-area/layout changes, last-zone restore, and multi-monitor options
