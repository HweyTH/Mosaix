![Mosaix -- tiling window management for Windows 11](./assets/mosaix-banner.svg)

# Mosaix

A keyboard-driven tiling window manager for Windows 11.

[Installation](#installation) · [Keybindings](#default-keybindings) · [Configuration](#configuration) · [CLI](#cli) · [Documentation](#documentation)

## Key features

- **Zone snapping with cycling** -- snap the focused window to a half zone;
  repeating a horizontal snap cycles half → third → two-thirds.
- **Automatic tiling** -- a deterministic, aspect-aware balanced grid, opted
  into per display topology, with a runtime toggle and `rearrange` recovery.
- **Directional focus, swap, and resize** -- move focus across the grid and
  swap two windows' places from the home row, or move the nearest divider
  with the arrow keys.
- **Per-topology profiles** -- sparse overlays matched by display fingerprint,
  falling through to base config field by field, re-read across sleep, wake,
  and hotplug.
- **Logical workspaces** -- named groups of managed windows, one shown per
  monitor, switched by hotkey or CLI. Experimental; see [Status](#status).
- **Saved layouts** -- named sets of normalized cells, applied by hotkey or
  CLI, drawn and edited in a visual editor.
- **Hot-reloaded TOML config** -- whole-directory atomic validation; an invalid
  edit keeps the last known-good configuration rather than applying.
- **Settings app** -- a Tauri 2 desktop app for drafting layouts, editing
  hotkeys by pressing them, and tuning tiling, each write naming the file it
  lands in.
- **Tray and CLI control** -- pause, resume, and quit from the tray; the
  `mosaix` CLI drives every command over versioned named-pipe IPC.
- **Status and recovery** -- `mosaix status` reports tree mode, focused display,
  the workspace pool, constraint overflow, durability, undo availability,
  parking capability, and any repair waiting on you, with the command that
  performs it.

## Status

Pre-release. Windows 11 only, and there are no binary releases yet -- see
[Installation](#installation).

- The **macOS adapter is not implemented**; `mosaix-platform-macos` is a stub.
  The name "cross-platform" describes the architecture, not today's build.
- **Workspace switching is experimental** and off unless a matched profile
  declares a mapping; `mosaix workspace switching` reports its state. It is
  emulated through public-API window parking, not Windows Virtual Desktops --
  read [the limitations](docs/experimental-workspace-switching.md) before
  relying on it.
- **Restoring a layout to particular windows** is not supported: a saved
  layout stores zone geometry only, and restoring fills its cells with
  whatever managed windows exist, in visual order.

## Installation

There are no binary releases yet. Build from source -- see
[Building from source](#building-from-source).

On first run the agent writes a default configuration to
`%APPDATA%\Mosaix\config\config.toml` and adds a tray icon.

## Default keybindings

Every binding below is a default that configuration can override, either by
editing `config.toml` or by pressing a new combination in the settings app.

| Keys | Command |
| --- | --- |
| `Win+Alt+←` / `→` / `↑` / `↓` | Snap the focused window to a half zone; repeat to cycle |
| `Win+Alt+H` / `J` / `K` / `L` | Move focus left / down / up / right |
| `Win+Alt+Shift+H` / `J` / `K` / `L` | Swap the focused window with its neighbour |
| `Win+Alt+Shift+←` / `→` / `↑` / `↓` | Resize by moving the nearest divider |
| `Win+Alt+A` | Toggle automatic tiling for this topology |
| `Win+Alt+Space` | Float or unfloat the focused window |
| `Win+Alt+E` | Rearrange -- recover the grid after manual moves |
| `Win+Alt+P` | Pause and resume window management |
| `Win+Alt+N` / `Shift+N` | Move the focused window to the next / previous display |
| `Win+Alt+Z` | Return the focused window to where it was before that |

`Win+Alt` rather than `Ctrl+Alt`, because `Ctrl+Alt` is `AltGr`: on a European
layout every binding here would fire while you were typing an ordinary
character. It also collides with JetBrains IDEs, where `Ctrl+Alt+L` reformats
code. `A` and `E` rather than the more obvious `T` and `R` because Xbox Game
Bar holds `Win+Alt+R` and `Win+Alt+T` system-wide.

## Configuration

Configuration lives in `%APPDATA%\Mosaix\config\`: a `config.toml` base, plus
sparse `profiles/*.toml` overlays matched by display topology. Edits are
hot-reloaded, and the whole directory is validated as a unit -- an invalid
edit is rejected and the last known-good configuration stays in effect.

```toml
version = 1
workspaces = ["main"]

[hotkeys]
snap-left = "win+alt+left"
snap-right = "win+alt+right"

[hotkeys.apply-layout]
writing = "win+alt+1"

[gaps]
outer = 8
inner = 4

[focus_border]
enabled = true
thickness = 2

[[layouts.writing.cells]]
x = 0.0
y = 0.0
width = 0.62
height = 1.0

[[rules]]
id = "float-calculator"
priority = 10
match.application_id = "Microsoft.WindowsCalculator"
actions.manage = "float"
```

### Window rules

Each `[[rules]]` entry decides how one kind of window is managed. `match`
takes any of `application_id`, `application_regex`, `title_regex`,
`native_class`, `class_regex`, `exe_path`, `exe_path_regex` and `role`, and
every field given must match. `actions.manage` is `tile`, `float` or
`exclude`, and `actions.workspace` names a workspace the window joins --
one that already exists, since a rule never creates one.

Higher `priority` wins; Mosaix's own built-in rules sit below every rule you
write. Rules live in `config.toml` only, never in a profile: a window's
management decision must not change under it when a monitor is unplugged.
An unparseable regex or a repeated `id` is rejected with the rest of the
directory, so a rule either works or says why.

## CLI

The `mosaix` CLI drives a running agent over a versioned named pipe.

```powershell
mosaix snap left-half          # snap the focused window
mosaix throw next              # move the focused window to the next display
mosaix restore-placement       # put it back where it was before that
mosaix layout apply writing    # apply a saved layout
mosaix arrangement             # report the tiling arrangement and container trees
mosaix undo                    # reverse the newest placement command
mosaix state --json            # the full agent state, for scripts and bug reports
```

Run `mosaix --help` for the complete command set, including workspaces,
persistence, and recovery.

## Documentation

| Document | What it covers |
| --- | --- |
| [`ARCHITECTURE.md`](./ARCHITECTURE.md) | Logical architecture, repository structure, testing strategy, explicit non-goals |
| [`CONTEXT.md`](./CONTEXT.md) | The domain glossary -- zone, profile, resolved config, balanced grid, and the rest |
| [`docs/architecture-decisions/`](./docs/architecture-decisions/) | Architecture decision records: what was decided, what was rejected, and why |
| [`docs/research/`](./docs/research/) | Cited research behind the decisions |

## Building from source

**Prerequisites:** Windows 11, a stable Rust toolchain (MSVC), and Node.js
with npm for the settings app.

```powershell
git clone https://github.com/HweyTH/Mosaix.git
cd Mosaix
cargo build --release
```

Start the background agent:

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

Run the test suite with `cargo test --workspace`, and the settings app's own
with `npm test` in `apps/mosaix-settings`.

## License

[MIT](./LICENSE)
