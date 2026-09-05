# ADR 0026: Directional swap exchanges live leaf occupants

**Status:** Accepted
**Date:** 2026-09-03

## Context

Mosaix already names a display-local `Directional swap` command. Phase 3A adds a
normalized, nested container tree, creating a choice between exchanging windows in
existing slots and making the command reparent containers or reshape the tree.

Research across i3, Sway, GlazeWM, AeroSpace, yabai, and komorebi found no universal
directional-movement algorithm. It did find a stable intent boundary: commands named
`move`, `warp`, or reinsert may traverse and restructure a tree, while commands named
`swap` exchange positions or leaf contents without choosing a new layout shape. See
`docs/research/tree-directional-swap-semantics.md`.

Phase 3A intentionally supports only weighted horizontal/vertical splits and one
window per live leaf. Its persistent undo contract is command-atomic and refuses
unsafe restoration.

## Decision

In container-tree mode, `Directional swap` exchanges only the live window bindings
of the focused tiled leaf and the same-display tiled leaf selected by `Directional
focus`.

- Container identities, parentage, split axes, weights, insertion metadata, and
  dormant leaves do not move.
- Logical focus remains on the same window identity in its new leaf.
- The exchange and resulting placements form one undo transaction.
- Floating, session-floating, dormant, and constraint-overflow windows are not
  endpoints.
- With no eligible directional target, the command returns a structured no-target
  result and makes no mutation.
- The command does not wrap or cross a display. `Display transfer` remains the
  explicit cross-display operation.

Structural directional movement is a separate future command and requires its own
rules for ascent, descent, reparenting, implicit containers, weight redistribution,
normalization, and undo. Stack and monocle swap behavior is deferred with those
layouts.

## Consequences

- The result is visually predictable: two windows exchange rectangles and every
  other tree slot retains its shape.
- Directional focus and swap cannot disagree about which neighbor a direction names.
- Persistence records a small two-binding mutation instead of an open-ended tree
  rewrite, making atomic undo and post-restart explanation simpler.
- Users cannot reshape a nested tree with the first-release swap command. A later
  structural move/reinsert command must be named and designed explicitly rather than
  changing this contract.

## Rejected alternatives

- **Reuse i3-style directional move semantics under `swap`:** rejected because it
  can reparent leaves, create or flatten containers, redistribute weights, and cross
  monitors; that is useful movement behavior but not a literal swap.
- **Swap whole subtrees selected by geometry:** rejected because a leaf-focused
  command could unexpectedly move unrelated descendant windows and because
  ancestor/descendant targets require special cycle prevention.
- **Allow floating or overflow endpoints:** rejected because they do not currently
  occupy active tree geometry and would make a temporary placement state mutate
  persistent structure.
