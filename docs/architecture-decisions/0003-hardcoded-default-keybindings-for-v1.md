# Hardcoded default keybindings for v1; config-driven rebinding deferred

**Status:** superseded by [0005](0005-config-delivery-via-queued-event-and-live-hotkey-rebind.md) -- `mosaix-config` is no longer an empty stub; config-driven rebinding, including live rebind on profile switch, is now built. The table this ADR describes moves into `mosaix-config` as an in-memory startup-failure fallback rather than the sole source of default bindings.

The initial hotkey and repeat-cycle work ships with a fixed, hardcoded table of command -> keybinding, not user-configurable bindings. `mosaix-config` (`crates/mosaix-config`) has no schema, loading, or validation code yet -- it's an empty stub -- so building config-driven rebinding now would mean building a config system as a side effect of a hotkeys ticket. Config-driven keybindings are explicitly deferred to whenever `mosaix-config` is actually built.
