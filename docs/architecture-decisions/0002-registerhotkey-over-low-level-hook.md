# RegisterHotKey over a low-level keyboard hook for global hotkeys

**Status:** accepted

Global hotkeys are registered via Win32 `RegisterHotKey`/`WM_HOTKEY`, not a low-level `WH_KEYBOARD_LL` keyboard hook. This matches ARCHITECTURE.md section 11.1 ("Register supported global hotkeys; isolate any low-level keyboard hook behind an optional input module") and satisfies the requirement that Mosaix's hotkeys must not conflict with existing OS or app shortcuts: `RegisterHotKey` is OS-arbitrated and fails cleanly (a returned error) if the combination is already owned, rather than silently intercepting it.

## Considered Options

- **Low-level keyboard hook** (GlazeWM's and the komorebi/`whkd` ecosystem's approach, per `docs/research/cycle-and-hotkeys.md`): intercepts every keystroke and decides per-event whether to swallow it. Rejected -- a hook has no OS-arbitrated conflict concept at all; it can silently shadow any shortcut, including the OS's own, which is the opposite of "must not conflict."

## Consequences

- If one hotkey among several configured bindings fails to register (already owned by the OS or another app), registration is partial-success: that one binding is skipped and reported via a typed error (extending `WindowError` in `mosaix-platform-windows`), while the rest still register. A single bad binding must not take down every other shortcut, matching how ARCHITECTURE.md section 8.1 already describes command validation elsewhere ("structured success, partial success, or failure details").
- Some OS-reserved combinations (any Win-key chord, Win+L, Ctrl+Alt+Del, F12) can never be registered regardless of this choice -- confirmed via Microsoft's own `RegisterHotKey` docs and the PowerToys Keyboard Manager docs (see `docs/research/cycle-and-hotkeys.md`). No hardcoded reserved-combo list is maintained; `RegisterHotKey`'s own failure is the sole conflict signal, matching PowerToys FancyZones' approach.
