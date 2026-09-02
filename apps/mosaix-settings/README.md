# Mosaix Settings

The production Mosaix desktop settings application, built with Tauri 2 and framework-free TypeScript.

## Layout Tab

The initial desktop surface is the visual layout editor. Its locked design decisions are:

- Spatial, monitor-first editor with floating tool and property controls.
- **Night Tide** for dark appearance and **Warm Paper** for light appearance.
- Product subtitle **Layout Tab**.
- Display context stays next to the canvas; there is no top-centre layout metadata.
- Draft operations include selecting, naming, splitting, duplicating, deleting, undoing, gaps, and overlap policy.
- Preview and Save & apply cross typed Tauri commands; the frontend does not edit configuration files.

Preview validates the draft locally and draws it against the selected display's real work area. Everything that persists goes through the agent over a connection held for the window's lifetime: saving, renaming, duplicating and deleting a saved layout, and Save & apply, which saves the drawing before applying it. Each is reported only when the agent confirms it, along with the configuration file the write landed in -- with a topology profile matched, that is not necessarily the file a user would guess (ADR 0022).

The left rail also lists every hotkey binding in effect and the file supplying it. Read-only for now; editing arrives with the capture dialog.

## Develop

```powershell
npm install
npm run tauri dev
```

## Verify

```powershell
npm test
npm run check
npm run build
cargo test -p mosaix-settings
npm run tauri -- build --no-bundle
```
