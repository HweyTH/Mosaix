# ADR 0018: Saved layouts store zone geometry, not window identity

**Status:** Accepted
**Date:** 2026-09-02

## Context

`ACHITECTURE.md` uses "saved arrangements" (section 1) and "the requested arrangement" (section 9.3) for two different things, and the gap between them is the whole cost question for this feature. Restoring a *shape* is geometry. Restoring an *arrangement of particular windows* requires the scored-evidence matching of section 7.2, the SQLite state database of section 12.2, an evidence inspector so users can repair a bad match, and partial-success reporting -- essentially all of Phase 4.

The settings app already drafts the geometry half. `apps/mosaix-settings/src-tauri/src/editor.rs` defines `ZoneDraft` and `LayoutDraft` as pure rectangles with no window identity, but `BaseConfig` has no layouts field, so a draft has nowhere to persist, and `save_and_apply_layout` is deliberately rejected until the versioned `mosaix-ipc` transport lands.

The first version also needs saved layouts remembered per monitor, and ADR 0004 already provides that shape: profiles are sparse per-topology overlays matched by fingerprint that may override any base-config field.

## Decision

A saved layout stores zone geometry only. Restoring one applies its cells to a display and fills them with the managed windows that exist, in visual window order. Nothing about a saved layout identifies a window, and restoring never fails because a particular application is not running.

Saved layouts live in configuration rather than in a new state store, so they inherit atomic whole-directory validation (ADR 0007), debounced hot reload (ADR 0008), and per-topology overlay (ADR 0004). "Remember the layout per monitor" is therefore a profile overriding the base set, not a separate persistence mechanism.

Restoring a layout to specific windows by identity is deferred to issue #28.

## Alternatives considered

- **Identity-based restoration in the first version:** rejected because it requires section 7.2 scored matching, the unimplemented section 12.2 SQLite database, and a repair inspector, none of which exist, and would deliver a persistence layer and a matching engine as side effects of a saved-layout ticket. Deferred to #28 rather than dropped.
- **Silently recording the last arrangement and restoring it automatically:** rejected in favor of explicit named layouts, because an implicit recall gives the user no way to keep a layout they liked, no way to choose between two, and no way to tell a deliberate save from an accidental one.
- **A separate saved-layout store outside configuration:** rejected because it would duplicate validation, reload, and per-topology overlay that configuration already provides.

## Consequences

- `BaseConfig` gains a layouts section, and `ProfileConfig` can override it, giving per-monitor memory with no new persistence code.
- Restoring is total: any window set can satisfy any saved layout, so there is no partial-success state and no failure mode when an application is absent.
- Restoring cannot put a particular window in a particular cell, which is the capability most users picture under "saved arrangement". #28 records that gap explicitly so the limitation is deliberate rather than forgotten.
- The settings app's existing `LayoutDraft` becomes persistable, but only once the `mosaix-ipc` apply transport exists; that transport is a prerequisite for this feature, not a follow-on.
