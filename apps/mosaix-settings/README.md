# Mosaix Settings

The production Mosaix desktop settings application, built with Tauri 2 and framework-free TypeScript.

## Layout Tab

The initial desktop surface is the visual layout editor. Its locked design decisions are:

- Spatial, monitor-first editor with floating tool and property controls.
- **Night Tide** for dark appearance and **Warm Paper** for light appearance, switched by a two-glyph control in the top actions.
- Product subtitle **Layout Tab**.
- Display context stays next to the canvas; there is no top-centre layout metadata.
- Draft operations include selecting, naming, splitting, duplicating, deleting, undoing, gaps, and overlap policy.
- Preview and Save & apply cross typed Tauri commands; the frontend does not edit configuration files.
- A write's destination is shown before the save, with a redirect to base config beside it.
- Hotkey capture is one dialog layered over the editor; registration stays suspended for the window's lifetime, not per dialog.
- Copy stays short and unsubtitled. A control says what it does; the interface does not caption what the user can already see.

Preview validates the draft locally and draws it against the selected display's real work area. Everything that persists goes through the agent over a connection held for the window's lifetime: saving, renaming, duplicating and deleting a saved layout, and Save & apply, which saves the drawing before applying it. Each is reported only when the agent confirms it.

The destination is named before the write, not only in the receipt: a write lands in the layer that currently supplies the value, so with a topology profile matched it is not necessarily the file a user would guess (ADR 0022). A control beside it redirects the write to base config, which *moves* a profile-local layout out of its profile -- a copy would leave the profile still winning the merge, so the redirect would appear to succeed and change nothing at that desk.

## Hotkeys

The left rail lists every command Mosaix can bind, with its combination and the file supplying it. A command nothing binds is listed too, marked as unbound: a saved layout that is not yet a keystroke away is exactly the one a user wants to reach, and a binding reset out of existence has to leave a row to bind again.

Rebinding is a capture dialog: press the combination you want, and the agent says before you save whether it is free, already another Mosaix binding, or owned by the system or another application. A conflict names its owner and is still savable -- Mosaix does not overrule the user about their own machine. Only `Win+L` and `Ctrl+Alt+Del` are refused outright, because the availability probe cannot see them; `F12` warns rather than blocks. A binding can be reset to its default, and the write follows the same destination and redirect rules layouts do.

While this window is open the agent holds every hotkey unregistered, because `RegisterHotKey` is OS-arbitrated and offers no way to let a combination reach the editor instead (ADR 0021). That is stated in the hotkeys panel rather than left to be inferred. Suspension is bounded by the connection, so hotkeys come back when the window closes -- and when it crashes or is killed. Any binding another application took while Mosaix was suspended is named there too.

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
