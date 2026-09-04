# ADR 0024: Persistent undo is atomic and safety-first

**Status:** Accepted
**Date:** 2026-09-03

## Context

The existing `restore` command remembers only one placement for one live native window. Moving undo history into SQLite and keeping it across agent restarts invalidates that identity shortcut: native window handles cannot be durable identities, one automatic action may place several windows, and the original display topology or window set may no longer exist when undo is requested.

## Decision

Persistent undo records one transaction for each explicit placement-changing user command, including every reversible state change and placement caused by that command's reflow. A workspace switch therefore records both the prior displayed-workspace assignment and every affected placement. Passive reflows caused by window lifecycle, application state, or topology events create no transaction.

Transactions survive agent restarts and are retained while they are among the newest 100 transactions and no more than seven days old. Each records its topology fingerprint and scored window-identity evidence, never a native handle as durable identity.

Undo examines only the newest transaction and preflights every member. It proceeds atomically only when the current topology matches and every target is present and confidently identified. Otherwise it moves nothing, reports the reason, retains the transaction for retry, and does not skip to older history. A successful undo consumes the transaction. Redo is deferred to issue #44.

Refusals expose a typed reason and per-target matching evidence through IPC and the CLI. The first persistent-undo release does not pull forward the graphical evidence and repair inspector from issue #28.

Persisted identity evidence excludes captured raw window titles by default. Matching uses application identity, role/class, safe document identifiers, launch order, and last placement; a title pattern participates only when deliberately authored by the user through the later repair workflow.

Persistent undo and issue #28 share one platform-neutral identity-matching service. It returns `confident`, `ambiguous`, or `no-match` together with scored evidence; only `confident` may authorize a persisted placement.

Undo provides no force flag or best-guess override. A refused transaction remains intact until its targets and topology can be resolved safely or retention prunes it.

## Alternatives considered

- **Keep undo session-only:** rejected because users expect recent recovery to survive an agent restart.
- **Undo only the focused window:** rejected because one command can reflow several windows and expose invalid intermediate layouts.
- **Apply the targets that can be resolved:** rejected because partial placement breaks the command-level atomic guarantee.
- **Remap onto a changed display topology:** rejected because that would be a new placement decision rather than reversal.
- **Skip an unavailable newest transaction:** rejected because silently undoing a different action violates chronological user intent.
- **Include passive reflows:** rejected because Mosaix cannot reverse the external event that caused them.
- **Ship redo in the same milestone:** deferred so the first SQLite and identity-matching increment has one recovery direction; issue #44 preserves the follow-up.

## Consequences

- Durable identity matching and explicit refusal reporting move forward from the later scene-restoration work in issue #28.
- Multi-window commands must group their effects under one transaction boundary before persistence.
- An unavailable newest transaction temporarily blocks older undo history until it becomes applicable, expires, or is otherwise removed through an explicit future history-management design.
- The first release needs no redo stack or branch-invalidation semantics.
