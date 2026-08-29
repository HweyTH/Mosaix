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

The editor keeps only an unsaved local draft. Preview validates that draft locally. Save & apply is intentionally rejected until the versioned `mosaix-ipc` agent transport lands, so this shell never reports a durable apply that the authoritative background agent did not perform.

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
