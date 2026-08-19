# Mosaix

![Rust](https://img.shields.io/badge/Rust-000000?style=flat-square&logo=rust&logoColor=white) ![Tauri](https://img.shields.io/badge/Tauri-24C8DB?style=flat-square&logo=tauri&logoColor=white) ![TypeScript](https://img.shields.io/badge/TypeScript-007ACC?style=flat-square&logo=typescript&logoColor=white) ![Windows](https://img.shields.io/badge/Windows-0078D6?style=flat-square&logo=windows&logoColor=white) ![macOS](https://img.shields.io/badge/macOS-000000?style=flat-square&logo=apple&logoColor=white)

A cross-platform window tiling application for Windows 11 and macOS.

Mosaix augments your native desktop window manager with manual snapping,
automatic tiling layouts, per-monitor workspaces, and a visual layout editor.

## Status

**Early development** -- workspace scaffolded, implementation not yet started.

## Features (Planned)

- **Manual snapping** -- keyboard shortcuts, drag zones, and radial selector
- **Automatic tiling** -- BSP, tall/wide, columns, rows, stack, and monocle layouts
- **Per-monitor layouts** and user-defined workspaces
- **Window rules** and exclusions
- **Saved arrangements** and workspace restoration
- **Visual layout editor**, tray/menu-bar controls, CLI, and integrations

## Architecture

Mosaix is built in Rust with a Tauri + TypeScript settings UI. See
[ARCHITECTURE.md](./ACHITECTURE.md) for the full design document.

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

## Building

```bash
cargo build
```

## License

MIT