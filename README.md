![Mosaix -- tiling window management for Windows 11 and macOS](./assets/mosaix-banner.svg)

![Rust](https://img.shields.io/badge/Rust-000000?style=flat-square&logo=rust&logoColor=white) ![Tauri](https://img.shields.io/badge/Tauri_2-24C8DB?style=flat-square&logo=tauri&logoColor=white) ![TypeScript](https://img.shields.io/badge/TypeScript-007ACC?style=flat-square&logo=typescript&logoColor=white) ![Windows 11](https://img.shields.io/badge/Windows_11-supported-0078D6?style=flat-square&logo=windows&logoColor=white) ![macOS](https://img.shields.io/badge/macOS-planned-6E6E6E?style=flat-square&logo=apple&logoColor=white) ![License MIT](https://img.shields.io/badge/License-MIT-green?style=flat-square)

A cross-platform window tiling application for Windows 11 and macOS.

## Features

- **Zone snapping with cycling** -- `Ctrl+Alt+Arrow` snaps the focused window to
  a half zone; repeating a horizontal snap cycles half -> third -> two-thirds.
- **Automatic tiling** -- a deterministic, aspect-aware balanced grid, opted into
  per display topology, with runtime toggle, suspension, and `rearrange` recovery.
- **Directional focus and swap** -- `Ctrl+Alt+H/J/K/L` moves focus across the
  grid; add `Shift` to swap two windows' places.
- **Multi-monitor handling** -- windows migrate off a disconnected display to
  the nearest survivor, and topology is re-read across sleep, wake, and hotplug.
- **Per-topology profiles** -- `profiles/*.toml` overlays matched by display
  fingerprint, falling through to base config field by field.
- **TOML config with hot reload** -- debounced, whole-directory atomic
  validation; invalid edits keep the last known-good config instead of applying.
- **Window rules** -- built-in dialog and tool-window floats plus user rules,
  resolved by ordered precedence with a traceable explanation per window.
- **Snap preview overlay** -- a click-through preview that flashes the committed
  placement after a hotkey, and shows the target zone during an edge drag.
- **Focus border** -- a click-through outline around the focused window while
  automatic tiling is live, with configurable color and thickness.
- **Tray and CLI control** -- pause/resume, open config folder, and quit from the
  tray; the `mosaix` CLI drives every command over versioned named-pipe IPC,
  including `mosaix state --json`.
- **Saved layouts** -- named sets of normalized cells declared under
  `[layouts]`, applied to the focused window's display with
  `mosaix layout apply <name>` or a hotkey bound under
  `[hotkeys.apply-layout]`. Gaps apply as they do to the grid; surplus cells
  are left empty and surplus windows are reported rather than dropped.
- **Visual layout editor** -- a Tauri 2 settings app for drafting and previewing
  zone layouts, and for asking the agent to apply one.
- **Failure containment** -- placement rejection detection with a per-window
  circuit breaker, elevated-window skipping, and degraded-tiling diagnostics.
- **Logical workspaces (experimental switching)** -- named groups of managed
  windows, one shown per monitor. Switching between them is **emulated through
  public-API window parking**, not Windows Virtual Desktops or macOS Spaces:
  a hidden workspace's windows are moved beyond the edge of the virtual screen
  and moved back, with the way back written to a recovery ledger first. It is
  activated only by a matched topology profile, and stays experimental until it
  passes its live matrix on both platforms. Read
  [the limitations](docs/experimental-workspace-switching.md) before relying on
  it.
- **Status and recovery** -- `mosaix status` reports tree mode, focused display,
  the workspace pool, constraint overflow, durability, undo availability,
  parking capability, and any repair waiting on you, with the command that
  performs it.

Not yet built: hotkey editing in the settings app, and saving a drafted layout
back to configuration.

## Installation

There are no binary releases yet -- build from source.

**Prerequisites:** Windows 11, a stable Rust toolchain (MSVC), and Node.js with
npm for the settings app.

```powershell
git clone https://github.com/HweyTH/Mosaix.git
cd Mosaix
cargo build --release
```

Start the background agent. On first run it writes a default config to
`%APPDATA%\Mosaix\config\config.toml` and adds a tray icon:

```powershell
.\target\release\mosaix-agent.exe
```

Drive it from the CLI while it runs:

```powershell
.\target\release\mosaix.exe snap left-half
.\target\release\mosaix.exe state --json
```

The settings app runs separately:

```powershell
cd apps\mosaix-settings
npm install
npm run tauri dev
```

Run the test suite with `cargo test --workspace`.

## License

MIT
