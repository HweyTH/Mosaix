# Research: What should directional swap mean in Mosaix's container tree?

> **TL;DR**: Popular tilers do not have one universal directional-movement contract, but the mature designs consistently separate two intents: **swap** exchanges occupants/positions without redesigning the layout, while **move**, **warp**, or reinsert commands may reparent nodes and reshape the tree. For Mosaix Phase 3A, keep `Directional swap` literal and conservative: exchange the focused live tiled window with the same-display live tiled leaf selected by directional focus; preserve every container, split axis, weight, and dormant leaf; keep focus on the moved window; and make the whole exchange one undo transaction. Floating and constraint-overflow windows do not participate, and monitor transfer remains the separate `Display transfer` command. Structural directional move can be designed later under a different command.

## Findings

### There are two distinct operations, not one standard operation

i3's directional `move` is a structural tree operation. It searches ancestors for a split with the requested orientation; depending on what it encounters, it can reorder siblings, descend into a neighboring branch, promote/reparent a container, reorient the workspace, or cross to another output ([i3 movement implementation](https://github.com/i3/i3/blob/next/src/move.c#L257-L391), [i3 movement guide](https://i3wm.org/docs/userguide.html#move_direction)). Sway follows the same family of semantics: adjacent leaf siblings are reordered, but nested cases can insert into a branch or promote the focused container through the tree ([Sway movement implementation](https://github.com/swaywm/sway/blob/master/sway/commands/move.c#L73-L187), [Sway command reference](https://github.com/swaywm/sway/blob/master/sway/sway.5.scd#L318-L365)). GlazeWM also implements `move --direction` as a tree edit: a neighboring leaf is reordered, a neighboring split receives the window at its near edge, and an outer move can reparent the window or invert the workspace split ([GlazeWM source](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/window/move_window_in_direction.rs#L46-L116), [nested-target cases](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/window/move_window_in_direction.rs#L133-L189), [ancestor insertion](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/window/move_window_in_direction.rs#L314-L348)).

By contrast, these projects use **swap** for a position exchange. i3 documents `swap container with ...` as making two containers assume each other's position and geometry; it rejects an ancestor/descendant pair because that exchange is not structurally valid ([i3 swap guide](https://i3wm.org/docs/userguide.html#swapping_containers), [i3 swap implementation](https://github.com/i3/i3/blob/next/src/con.c#L2393-L2501)). Sway exposes the same separately named operation and the same ancestor/descendant restriction ([Sway swap implementation](https://github.com/swaywm/sway/blob/master/sway/commands/swap.c#L59-L133)).

AeroSpace makes the distinction especially clear. `move DIR` can reorder, insert into or extract from a nested container, or create an implicit parent container ([AeroSpace `move`](https://nikitabobko.github.io/AeroSpace/commands#move)). Its separate `swap DIR` exchanges the focused window with the nearest directional window; focus stays with the same window by default, and wrapping is opt-in ([AeroSpace `swap`](https://nikitabobko.github.io/AeroSpace/commands#swap)). AeroSpace's tree permits arbitrary nesting and normalizes redundant containers, so this separation is deliberate rather than an artifact of a flat layout ([AeroSpace tree](https://nikitabobko.github.io/AeroSpace/guide#tree), [AeroSpace normalization](https://nikitabobko.github.io/AeroSpace/guide#normalization)).

yabai likewise separates `--swap`, which swaps positions, from `--warp`, which reinserts the source by splitting the target ([yabai command reference](https://github.com/asmvik/yabai/blob/master/doc/yabai.asciidoc#window)). Its directional selector searches managed BSP leaves on the current Space, requires perpendicular-axis overlap, minimizes edge distance, and uses window-list order to break ties ([yabai directional lookup](https://github.com/asmvik/yabai/blob/master/src/view.c#L499-L560)). The swap implementation exchanges leaf window lists while retaining the BSP nodes and their split topology ([yabai swap implementation](https://github.com/asmvik/yabai/blob/master/src/window_manager.c#L1761-L1856)).

komorebi is not a nested free-form container tree: a workspace owns an ordered ring of containers and a selected layout computes their rectangles ([komorebi data model](https://komorebi.lgug2z.com/about/overview/#data-model)). Its directional `move` resolves another layout index and swaps the two containers within the workspace ([komorebi workspace indexing and swap](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/workspace.rs#L1082-L1092), [container swap](https://github.com/LGUG2Z/komorebi/blob/master/komorebi/src/workspace.rs#L1756-L1766)). This supports occupant/slot exchange for policy-driven layouts, but it is not evidence that a nested tree's `move` should avoid restructuring.

The evidence therefore supports a naming rule rather than one universal implementation: **swap preserves the layout shape; move/reinsert is allowed to change it**.

### Comparison

| Manager | Directional rearrangement | Separate swap | Nested-group effect | Boundary behavior |
|---|---|---|---|---|
| i3 | `move DIR` traverses and may reparent/reorient | Targeted `swap container with ...` | Move may reshape; swap exchanges valid non-ancestor containers | Directional move can cross outputs at the workspace edge |
| Sway | `move DIR` follows i3-style tree traversal | Targeted `swap container with ...` | Move may reshape; swap rejects ancestor/descendant pairs | Directional and explicit output moves are defined separately |
| GlazeWM | `move --direction DIR` traverses/reparents | No established directional swap contract in the current command path | Move may insert into a sibling split or ancestor | At the workspace edge it can move to a directional monitor |
| AeroSpace | `move DIR` structurally edits the tree | `swap DIR` exchanges nearest windows | Move reshapes; swap preserves layout structure | Swap is workspace-local; monitor movement is separate |
| yabai | `--warp DIR` reinserts/splits | `--swap DIR` exchanges BSP leaf contents | Warp reshapes; swap preserves BSP topology | Cardinal lookup is Space-local; display transfer is separate |
| komorebi | `move DIR` swaps layout container indices | Swap is the movement behavior for in-workspace slots | Flat/policy layout rather than an arbitrary nested tree | Cross-monitor swap/insert/no-op is configurable |

### Floating windows are a separate command domain

i3 and Sway interpret directional move on floating windows as pixel translation, not as a tree neighbor operation ([i3 floating move](https://github.com/i3/i3/blob/next/src/commands.c#L1441-L1472), [Sway floating move](https://github.com/swaywm/sway/blob/master/sway/commands/move.c#L3361-L3433)). yabai's directional swap lookup selects only managed BSP windows and rejects unmanaged swap endpoints ([yabai selector](https://github.com/asmvik/yabai/blob/master/src/message.c#L826-L883), [yabai validation](https://github.com/asmvik/yabai/blob/master/src/message.c#L2089-L2105)). AeroSpace describes floating windows as outside the tiling tree; its special accommodation is for directional **focus**, not structural move or swap ([AeroSpace floating windows](https://nikitabobko.github.io/AeroSpace/guide#floating-windows), [AeroSpace focus](https://nikitabobko.github.io/AeroSpace/commands#focus)).

For Mosaix, `Directional swap` should consequently apply only when the focused window and target are actively arranged tree leaves. A session-floating or constraint-overflow window should produce no tree mutation. This avoids overloading a layout command with pixel movement and prevents an overflow fallback from silently becoming persistent structure.

### Monitor crossing should remain explicit

The products vary substantially at monitor boundaries. i3, Sway, and GlazeWM may let directional **move** cross an output when tree traversal is exhausted. komorebi makes cross-monitor move behavior configurable as swap, insert, or no-op ([komorebi monitor boundaries](https://komorebi.lgug2z.com/usage/monitors/#boundary-behaviour)). AeroSpace instead keeps `swap DIR` workspace-local and provides `move-node-to-monitor DIR` explicitly ([AeroSpace monitor move](https://nikitabobko.github.io/AeroSpace/commands#move-node-to-monitor)). yabai's cardinal swap lookup stays inside the current managed view, while `window --display DIR` is the explicit display operation ([yabai command reference](https://github.com/asmvik/yabai/blob/master/doc/yabai.asciidoc#window), [yabai directional lookup](https://github.com/asmvik/yabai/blob/master/src/window_manager.c#L891-L902)).

Mosaix already has `Display transfer` and defines `Directional swap` as display-local. Keeping that boundary is the more predictable contract and prevents one key binding from changing meaning at a screen edge.

### Persistence and undo favor a leaf-occupant exchange

A structural move needs rules for ascent, descent, implicit container creation, weight redistribution, normalization, dormant placeholders, and potentially monitor transfer. i3's own maintainer documentation calls movement code delicate and documents multiple structural cases ([i3 hacking guide](https://i3wm.org/docs/hacking-howto.html#_moving_containers)). GlazeWM's implementation likewise contains distinct sibling-window, sibling-split, ancestor-insertion, workspace-reorientation, and monitor paths ([GlazeWM source](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/window/move_window_in_direction.rs#L46-L348)).

Exchanging two live leaf occupants is a much smaller persistent mutation: the two window bindings change places, while split identities, axes, weights, parentage, and dormant leaves remain unchanged. It can be stored and reversed as one command-level undo transaction, matching Mosaix's atomic undo rule. It is also visually explainable: the two windows assume each other's rectangles and every other rectangle stays stable.

## Recommendation for Mosaix Phase 3A

Adopt the following contract:

1. `Directional swap` starts only from a focused, live, actively arranged tiled leaf.
2. Resolve the target with the same display-local spatial neighbor function used by `Directional focus`. This keeps “the window I would focus” identical to “the window I would swap with” and avoids a second directional geometry definition.
3. Exchange only the two leaves' live window bindings and window-associated identity metadata. Preserve the tree nodes, parentage, split axes, weights, insertion metadata, and dormant leaves.
4. Keep logical focus on the same window identity after it takes the target slot, following AeroSpace's default and the ordinary expectation of moving the thing under the user's hand.
5. Recompute placement, but only the two exchanged windows should change rectangles if the stored tree was already normalized.
6. Record the command and all resulting placements as one atomic undo transaction.
7. If there is no directional live arranged leaf, do nothing and report a structured no-target result. Do not wrap and do not cross a display.
8. Floating, session-floating, dormant, and constraint-overflow windows are not swap endpoints. Use their dedicated commands instead.
9. Defer structural directional movement. If added later, name it `Directional move` (or reinsert) and specify its tree-shaping rules independently; do not silently change `Directional swap`.
10. Defer stack/monocle swap semantics with those layouts. Phase 3A has one live window per leaf, so it need not prematurely decide whether a later stack moves as a unit or only swaps its active member.

This is not the only behavior in the market. It is the behavior most consistent with the word **swap**, Mosaix's existing glossary, its narrow first tree slice, and its safety-first persistent undo architecture.

## Sources

- [i3 User's Guide: moving and swapping containers](https://i3wm.org/docs/userguide.html#move_direction) — public command contracts.
- [i3 movement implementation](https://github.com/i3/i3/blob/next/src/move.c#L257-L391) — nested-tree traversal and reparenting.
- [i3 swap implementation](https://github.com/i3/i3/blob/next/src/con.c#L2393-L2501) — positional swap and structural restrictions.
- [Sway movement implementation](https://github.com/swaywm/sway/blob/master/sway/commands/move.c#L73-L187) — i3-compatible nested-tree cases.
- [Sway swap implementation](https://github.com/swaywm/sway/blob/master/sway/commands/swap.c#L59-L133) — explicit positional swap validation.
- [GlazeWM directional movement](https://github.com/glzr-io/glazewm/blob/main/packages/wm/src/commands/window/move_window_in_direction.rs) — sibling exchange, branch insertion, reparenting, and monitor behavior.
- [AeroSpace command reference](https://nikitabobko.github.io/AeroSpace/commands#move) — distinct structural move, swap, focus, and monitor commands.
- [AeroSpace guide](https://nikitabobko.github.io/AeroSpace/guide#tree) — tree and floating-window model.
- [yabai command reference](https://github.com/asmvik/yabai/blob/master/doc/yabai.asciidoc#window) — separate swap, warp, pixel move, and display transfer commands.
- [yabai directional selection](https://github.com/asmvik/yabai/blob/master/src/view.c#L499-L560) — current-view geometric nearest-neighbor algorithm.
- [yabai swap implementation](https://github.com/asmvik/yabai/blob/master/src/window_manager.c#L1761-L1856) — BSP leaf-content exchange.
- [komorebi window movement](https://komorebi.lgug2z.com/usage/windows/#movement-on-a-workspace) — directional and cyclic movement commands.
- [komorebi monitor boundary behavior](https://komorebi.lgug2z.com/usage/monitors/#boundary-behaviour) — configurable cross-monitor policies.
