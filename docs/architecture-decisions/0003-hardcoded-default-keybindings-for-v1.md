# Hardcoded default keybindings for v1; config-driven rebinding deferred

**Status:** accepted

The initial hotkey and repeat-cycle work ships with a fixed, hardcoded table of command -> keybinding, not user-configurable bindings. `mosaix-config` (`crates/mosaix-config`) has no schema, loading, or validation code yet -- it's an empty stub -- so building config-driven rebinding now would mean building a config system as a side effect of a hotkeys ticket. Config-driven keybindings are explicitly deferred to whenever `mosaix-config` is actually built.
