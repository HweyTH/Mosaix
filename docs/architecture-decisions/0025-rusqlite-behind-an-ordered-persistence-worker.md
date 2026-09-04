# ADR 0025: Use rusqlite behind an ordered persistence worker

**Status:** Accepted
**Date:** 2026-09-03

## Context

Mosaix needs one embedded state database for persistent undo, placement history, container trees, scene metadata, migrations, and onboarding state. The engine's serialized reducer is the sole authority over live state, and architecture section 14.1 requires persistence to remain asynchronous but ordered by reducer revision.

## Decision

Use `rusqlite` with bundled SQLite behind a dedicated persistence worker. The reducer emits committed persistence records carrying their state revision; it does not execute SQL or own a database connection. The worker applies records in revision order and owns transactions, schema migrations, retention pruning, and database-health reporting.

Manage the `rusqlite` version in the Cargo workspace and expose persistence through a domain-oriented interface so SQL rows and rusqlite types do not leak into the engine, IPC, or platform adapters.

Create schemas only alongside live consumers. The first milestone includes migration metadata, database health, and persistent-undo records; it does not pre-create container-tree, scene, diagnostics, or onboarding tables.

If a write fails after live window effects have committed, Mosaix continues from in-memory state and publishes a visible `persistence-degraded` condition rather than attempting compensating window moves. While degraded, it rejects new workspace parking and makes no persistent-undo promise for new actions. Durability-dependent features resume only after the worker restores ordered writes.

Database corruption, a newer unsupported schema, or a failed startup migration never causes an automatic reset or downgrade. Mosaix preserves the database untouched, starts in `persistence-degraded` mode, and requires an explicit repair or reset action.

The first release relies on user-only OS file access and data minimization instead of application-level encryption. Locally storing an encryption key would not protect against compromise of the signed-in user account and would add cross-platform key-management and recovery obligations; encryption remains a separate future threat-model decision.

## Alternatives considered

- **SQLx:** rejected because Mosaix does not need multiple database backends, an async connection pool, or query macros; those facilities add build and runtime surface without deepening the local state module.
- **Run rusqlite directly in the reducer:** rejected because synchronous disk work would put database latency and locking on the window-event hot path.
- **Use the operating system's SQLite installation:** rejected because availability and version would vary across supported machines; the bundled build gives Mosaix one tested SQLite surface.
- **Store state in additional JSON/TOML files:** rejected because atomic multi-record history, migrations, bounded pruning, and relational identity evidence are database responsibilities, while human-editable configuration remains separate.
- **Encrypt the database with SQLCipher immediately:** deferred because no accepted threat model justifies its key-management, recovery, build, and cross-platform distribution cost, while captured raw titles are already excluded.

## Consequences

- `rusqlite` becomes a workspace dependency and its native bundled build becomes part of release and cross-compilation testing.
- Persistence acknowledgements and failures must carry reducer revisions so clients can distinguish durable state from newer in-memory state.
- The database module can be tested through behavior and migrations without coupling reducer tests to SQL implementation details.
- Tray, CLI, IPC, and settings status must distinguish persistence degradation from placement-related degraded tiling.
