# Whole-directory atomic validation; startup falls back to in-memory defaults

**Status:** accepted

ARCHITECTURE.md section 12.1 requires atomic config reload: "Reject the entire candidate with line-aware errors, or swap it into the reducer." With config split across `config.toml` plus N files under `profiles/` (ADR 0004), the unit of atomicity is the whole directory: `mosaix-config` reads and validates every file together into one candidate `ResolvedConfig` set, and if *any* file fails (bad TOML, unknown schema version, a duplicate hotkey binding within a resolved config, two profiles sharing a fingerprint), the entire reload is rejected and the last-known-good set stays active. One malformed profile you aren't currently using can still block a base-config fix from taking effect until it's corrected -- accepted as the simpler, single mental model ("the config directory is valid, or it isn't") over per-file isolation.

Three things are validation errors under this rule, not warnings or silent fallbacks:

- A `version` field other than the schema version `mosaix-config` currently understands (no version-1 schema yet supports being lenient about a missing/unrecognized version -- that leniency is exactly what would let a genuinely incompatible future file be silently misread).
- Two commands in the same resolved config (base + active profile, after the field-level merge in ADR 0004) bound to the identical key combo. This is fully deterministic and independent of the OS-level conflict detection ADR 0002 already established for `RegisterHotKey` (which still applies unchanged for conflicts with something outside Mosaix).
- Two profile files claiming the same `fingerprint` (ADR 0004).

Startup is a special case of this: the very first read has no last-known-good to fall back to. If the config directory can't be read or created at all (permissions, corrupt first-run write), `mosaix-config` falls back to a small built-in constant -- the same Ctrl+Alt+Arrow defaults the pre-config hardcoded table used (ADR 0003/0005) -- logs the failure clearly, and keeps running, rather than refusing to start. This matches `mosaix-agent`'s existing failure posture (`main.rs`'s doc comment: only DPI awareness and the shutdown handler are treated as fatal; "every other subsystem failure is logged and degrades the agent instead").

## Consequences

- `mosaix-config` needs one small pure "validate everything, return either a full `ResolvedConfig` set or a list of errors" function, independent of I/O -- straightforward to unit test against fixture directories without touching the filesystem watcher.
- A user hand-editing `config.toml` sees no effect at all from a typo until they fix it (not a partial application), which is the intended atomic behavior but should be surfaced clearly (line-aware error, logged) so a "why didn't my edit take" isn't silent.
