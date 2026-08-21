# Config changes reach the engine as a queued Event; hotkey rebind is a polled diff

**Status:** accepted

**Supersedes ADR 0003**, which deferred config-driven keybindings until `mosaix-config` existed.

A validated config (base + the currently-active profile, merged) reaches `EngineState` the same way every other state change does: `mosaix-config`'s file-watcher thread sends `Event::ConfigChanged(ResolvedConfig)` into the existing bounded event queue (`crates/mosaix-engine/src/lib.rs`), and the reducer applies it like `DisplayTopologyChanged` or any other event -- one writer, one queue, revision bump, snapshot publish. `ResolvedConfig` (including which profile is active, selected by matching `topology_fingerprint()`) becomes a new `EngineState` field.

Because per-monitor profiles can override hotkey bindings (Tier 3 #20's scope decision), the *set of registered hotkeys* is no longer fixed for the life of the agent process -- it can change when `DisplayTopologyChanged` flips the active profile, or when the config file is hot-edited. `mosaix-agent`'s hotkey registration (`start_hotkeys`/`RegisterHotKey`, `mosaix-platform-windows`) is OS-level state the reducer never touches (per ARCHITECTURE.md section 14.1: "the reducer deliberately never touches the OS itself"), so nothing in the reducer can re-register a hotkey directly. Instead `mosaix-agent` gains a small poller, structurally identical to the existing placement-executor thread that already diffs `EngineState.windows` against applied OS placements: it diffs the currently-registered binding set against `EngineState`'s resolved active bindings, and calls `start_hotkeys` again (stopping the old registration first) whenever they differ.

## Considered Options

- **Separate `Mutex<ResolvedConfig>` outside the reducer, read live**: avoids growing the `Event` enum, but introduces a second live-mutable state path the reducer doesn't own -- exactly the second-writer problem `mosaix-engine`'s module doc calls out as deliberately avoided ("no lock guards the mutation itself, because nothing else ever touches it").
- **Push-based hotkey rebind from inside the reducer**: rejected outright -- the reducer has no OS access by design (ARCHITECTURE.md section 14.1), and giving it one for this single case would break the "native adapter" boundary every other OS interaction already respects.
- **Defer live rebind, restart-only**: considered and rejected during grilling -- letting profiles override hotkeys in the schema while silently not applying that override until a restart is a worse trap than not offering per-profile hotkeys at all.

## Consequences

- `mosaix-engine` gains a compile-time dependency on `mosaix-config` (for the `ResolvedConfig` type carried in `Event::ConfigChanged`). No cycle: `mosaix-config` has no dependency back on `mosaix-engine`.
- `mosaix-agent/src/keybindings.rs`'s `default_bindings()`/`direction_for_id()` are replaced by reading `EngineState`'s resolved config; the hardcoded Ctrl+Alt+Arrow table itself is not deleted, it moves into `mosaix-config` as an in-memory fallback constant for startup failure (ADR 0007).
- A profile switch and a hot-edit of `config.toml` go through the identical `Event::ConfigChanged` path, so there is exactly one hotkey-rebind code path to test, not two.
