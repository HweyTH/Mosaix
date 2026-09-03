# Mosaix

![Rust](https://img.shields.io/badge/Rust-000000?style=flat-square&logo=rust&logoColor=white) ![Tauri](https://img.shields.io/badge/Tauri-24C8DB?style=flat-square&logo=tauri&logoColor=white) ![TypeScript](https://img.shields.io/badge/TypeScript-007ACC?style=flat-square&logo=typescript&logoColor=white) ![Windows](https://img.shields.io/badge/Windows-0078D6?style=flat-square&logo=windows&logoColor=white) ![macOS](https://img.shields.io/badge/macOS-000000?style=flat-square&logo=apple&logoColor=white)

A cross-platform window tiling application for Windows 11 and macOS.

Mosaix augments your native desktop window manager with manual snapping,
automatic tiling layouts, per-monitor workspaces, and a visual layout editor.

## Status

**Early development** — the Windows agent and settings application are usable,
while macOS support and several planned tiling policies are still in progress.

## Available now

- **Manual snapping** with configurable global hotkeys for halves, thirds, and
  vertical halves; repeated horizontal snaps cycle through half, third, and
  two-thirds placements.
- **Focused-display controls** including drag-edge snap preview, window throw
  between displays, and configurable inner and outer gaps.
- **Saved layouts** authored in the Tauri settings editor. Save & apply writes
  the layout to the base configuration and applies it to managed windows on
  the focused window's display; Apply-only tries an unsaved draft without
  changing configuration.
- **Configuration profiles** selected by display-topology fingerprint, with
  validation and live reload of config and hotkey changes.
- **Background-agent controls** through the system tray and local IPC-backed
  CLI, including pause/resume and state inspection.

## Roadmap

- Automatic tiling policies: BSP, tall/wide, columns, rows, stack, and monocle.
- Window rules, exclusions, workspace restoration, and layout-hotkey bindings.
- macOS Accessibility/AppKit implementation and broader integrations.

## Architecture

Mosaix is built in Rust with a Tauri + TypeScript settings UI. See
[ARCHITECTURE.md](./ARCHITECTURE.md) for the full design document.

### Workspace Structure

```
mosaix/
|-- crates/
|   |-- mosaix-domain/            # IDs, geometry, state, commands, events
|   |-- mosaix-layout/            # Zones, trees, strategies, normalization
|   |-- mosaix-rules/             # Matching, precedence, explanations
|   |-- mosaix-engine/            # Reducer, reconciliation, transactions
|   |-- mosaix-config/            # Schema, validation, migrations
|   |-- mosaix-ipc/               # Protocol and local transports
|   |-- mosaix-platform-api/      # Adapter traits and capability model
|   |-- mosaix-platform-windows/  # Win32 implementation
|   |-- mosaix-platform-macos/    # Accessibility/AppKit implementation
|   |-- mosaix-agent/             # Background executable
|   +-- mosaix-cli/               # Command-line client
|-- apps/
|   +-- mosaix-settings/          # Tauri/TypeScript settings UI
|-- schemas/                      # Config and IPC schemas
|-- fixtures/                     # Event traces and topology fixtures
|-- tests/
|   |-- contract/                 # Adapter contract tests
|   |-- replay/                   # Recorded event replay tests
|   +-- platform/                 # Platform integration tests
+-- docs/
    |-- architecture-decisions/   # ADRs
    |-- permissions/              # Platform permission docs
    +-- troubleshooting/          # Troubleshooting guides
```

## Building and verification

```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
```

For the settings application, install its JavaScript dependencies and use the
Tauri development command:

```bash
cd apps/mosaix-settings
npm install
npm run tauri dev
```

## License

MIT
