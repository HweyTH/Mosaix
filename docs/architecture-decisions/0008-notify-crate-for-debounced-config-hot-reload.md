# `notify` crate for config-directory watching, debounced ~300ms

**Status:** accepted

`mosaix-config` watches its config directory (`config.toml` plus `profiles/`) for changes using the `notify` crate rather than polling or hand-rolling `ReadDirectoryChangesW`. This is a new workspace dependency, called out explicitly per this project's "no new libraries without approval" convention.

The existing precedent for OS-level watching in this codebase, `watch_display_topology` (`mosaix-platform-windows`), is hand-written native Win32 message-loop code -- but display topology watching is legitimately platform-specific (it belongs behind the per-platform adapter boundary, ARCHITECTURE.md section 9.1's `PlatformAdapter` trait). `mosaix-config` is a shared, non-platform-specific crate by design (it needs to behave identically once `mosaix-platform-macos` is a real implementation, not a one-line stub), so hand-rolling a Windows-only file watcher inside it would put platform-specific code somewhere the architecture doesn't otherwise allow it.

Raw filesystem events are debounced: the watcher waits for ~300ms of quiet after the last event before reading and validating, coalescing both "several `Write` events for one save" and "temp-file write + rename" save patterns into a single read. 300ms is comfortably above how long a small TOML write takes and comfortably below a delay a human editing the file would notice.

## Considered Options

- **Poll on a timer** (matching the existing placement-executor thread's pattern in `mosaix-agent`'s `main.rs`): zero new dependencies, but reload latency is bounded by the poll interval, and mtime-comparison/debounce logic would be hand-built and hand-tested instead of using a maintained crate's.
- **Read on every raw event, retry-once on parse failure**: rejected during grilling -- no debounce delay sounds faster, but a slow multi-write save could still race a single retry, and a genuinely broken config would need to fail, wait, then fail again before being reported, which is a worse error experience than one debounced read.

## Consequences

- `notify` is added to `[workspace.dependencies]`.
- The debounce window is a constant in `mosaix-config`, alongside the crate's other tunables (comparable to `mosaix-engine`'s `DEFAULT_QUEUE_CAPACITY`).
