export type Appearance = "dark" | "light";

export interface ZoneDraft {
  id: number;
  name: string;
  x: number;
  y: number;
  width: number;
  height: number;
}

export interface LayoutDraft {
  name: string;
  gap: number;
  allowOverlap: boolean;
  zones: ZoneDraft[];
}

export interface DisplaySummary {
  name: string;
  resolution: string;
  scalePercent: number;
  /// The work area's pixel dimensions. The canvas is drawn at these
  /// proportions, so a preview shows the shape the layout will really take.
  workAreaWidth: number;
  workAreaHeight: number;
}

export interface EditorSnapshot {
  appearance: Appearance;
  /// Every display a layout can be previewed against, primary first.
  displays: DisplaySummary[];
  draft: LayoutDraft;
}

export interface SavedLayout {
  name: string;
  cells: ZoneDraft[];
}

export interface LayoutWriteReceipt {
  file: string;
}

export interface CommandReceipt {
  revision: number;
  status: "previewing" | "applied";
}

export interface AutomaticTilingSettings {
  topologyFingerprint: string;
  matchedProfile: boolean;
  enabled: boolean;
  outerGap: number;
  innerGap: number;
  focusBorderEnabled: boolean;
  focusBorderColor: string;
  focusBorderThickness: number;
}

/// One hotkey binding as the interface shows it. `file` is the
/// configuration file that currently supplies it, and so the file an edit
/// of it would be written to (ADR 0022).
export interface HotkeyBinding {
  command: string;
  layout: string | null;
  combo: string;
  source: "base" | "profile";
  file: string;
}

export interface HotkeyList {
  topologyFingerprint: string;
  bindings: HotkeyBinding[];
}

export interface DesktopBridge {
  loadEditorSnapshot(): Promise<EditorSnapshot>;
  loadHotkeyBindings(): Promise<HotkeyList>;
  loadSavedLayouts(): Promise<SavedLayout[]>;
  saveLayout(draft: LayoutDraft): Promise<LayoutWriteReceipt>;
  renameLayout(from: string, to: string): Promise<LayoutWriteReceipt>;
  duplicateLayout(from: string, to: string): Promise<LayoutWriteReceipt>;
  deleteLayout(name: string): Promise<LayoutWriteReceipt>;
  previewLayout(draft: LayoutDraft): Promise<CommandReceipt>;
  saveAndApplyLayout(draft: LayoutDraft): Promise<CommandReceipt>;
  setAppearance(appearance: Appearance): Promise<void>;
  loadAutomaticTilingSettings(): Promise<AutomaticTilingSettings>;
  saveAutomaticTilingSettings(settings: AutomaticTilingSettings): Promise<AutomaticTilingSettings>;
}

/// The label for a command's TOML path: `snap-left` becomes "Snap left",
/// and `apply-layout.writing` becomes "Apply layout · writing".
///
/// Reads the path rather than carrying a table of pretty names, so a verb
/// added to the schema shows up here without a second edit -- at the cost
/// of a label that is only as good as the verb's spelling, which is the
/// right trade for a list a user scans rather than reads.
export function bindingLabel(command: string): string {
  const [verb, ...rest] = command.split(".");
  const words = (verb ?? "").split("-").join(" ");
  const title = words.charAt(0).toUpperCase() + words.slice(1);
  return rest.length > 0 ? `${title} · ${rest.join(".")}` : title;
}

export interface WatchHandlers<T> {
  onChange: (value: T) => void;
  onError: (error: unknown) => void;
}

/// Calls `read` every `intervalMs`, reporting only when the answer has
/// actually changed. Returns a function that stops it.
///
/// Polling rather than a push from the agent: the IPC protocol answers
/// requests and never initiates, so a settings window that wants to notice
/// a change made elsewhere -- docking a laptop, hand-editing a config file
/// -- has to ask.
///
/// Reporting only changes is what keeps this from re-rendering every tick,
/// and it applies to failures too, so an agent that is not running is
/// reported once rather than twice a second.
export function watchChanges<T>(
  read: () => Promise<T>,
  handlers: WatchHandlers<T>,
  intervalMs: number,
): () => void {
  let reported: string | undefined;
  const report = (key: string, emit: () => void): void => {
    if (key === reported) return;
    reported = key;
    emit();
  };
  const poll = async (): Promise<void> => {
    try {
      const value = await read();
      report(`ok:${JSON.stringify(value)}`, () => handlers.onChange(value));
    } catch (error: unknown) {
      report(`error:${String(error)}`, () => handlers.onError(error));
    }
  };
  const timer = setInterval(() => void poll(), intervalMs);
  void poll();
  return () => clearInterval(timer);
}

/// Watches the hotkey bindings. A topology change swaps the matched
/// profile, and with it both the combinations on screen and the files
/// behind them.
export function watchHotkeyBindings(
  bridge: Pick<DesktopBridge, "loadHotkeyBindings">,
  handlers: WatchHandlers<HotkeyList>,
  intervalMs = 2000,
): () => void {
  return watchChanges(() => bridge.loadHotkeyBindings(), handlers, intervalMs);
}

/// Watches the saved-layout set, so a layout added by hand in a
/// configuration file appears without reopening the window.
export function watchSavedLayouts(
  bridge: Pick<DesktopBridge, "loadSavedLayouts">,
  handlers: WatchHandlers<SavedLayout[]>,
  intervalMs = 2000,
): () => void {
  return watchChanges(() => bridge.loadSavedLayouts(), handlers, intervalMs);
}

function escapeHtml(value: string): string {
  return value
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#039;");
}

function renderZones(snapshot: EditorSnapshot, selectedZoneId: number): string {
  return snapshot.draft.zones
    .map(
      (zone, index) => `
        <button class="zone${zone.id === selectedZoneId ? " active" : ""}" data-zone="${zone.id}" style="left:calc(${zone.x}% + ${snapshot.draft.gap / 2}px);top:calc(${zone.y}% + ${snapshot.draft.gap / 2}px);width:calc(${zone.width}% - ${snapshot.draft.gap}px);height:calc(${zone.height}% - ${snapshot.draft.gap}px)">
          <strong>${escapeHtml(zone.name)}</strong>
          <span class="zone-index">${index + 1}</span>
          <small>${zone.width}% × ${zone.height}%</small>
          <span class="resize-handle" aria-hidden="true"></span>
        </button>`,
    )
    .join("");
}

function renderLayouts(
  layouts: SavedLayout[] | undefined,
  selected: string | undefined,
  error: string | undefined,
): string {
  if (error !== undefined) return `<p data-layout-error>${escapeHtml(error)}</p>`;
  if (layouts === undefined) return `<p>Reading saved layouts…</p>`;
  if (layouts.length === 0) return `<p>No saved layouts yet.</p>`;
  return `<ul class="library-list">${layouts
    .map(
      (layout) => `
        <li class="library-item${layout.name === selected ? " active" : ""}" data-layout="${escapeHtml(layout.name)}">
          <button class="layout-open" data-open-layout="${escapeHtml(layout.name)}">${escapeHtml(layout.name)}</button>
          <small class="library-detail">${layout.cells.length} zone${layout.cells.length === 1 ? "" : "s"}</small>
          <span class="layout-actions">
            <button data-duplicate-layout="${escapeHtml(layout.name)}" title="Duplicate">⧉</button>
            <button data-delete-layout="${escapeHtml(layout.name)}" title="Delete">⌫</button>
          </span>
        </li>`,
    )
    .join("")}</ul>`;
}

function renderBindings(hotkeys: HotkeyList | undefined, hotkeyError: string | undefined): string {
  if (hotkeyError !== undefined) {
    return `<p data-hotkey-error>${escapeHtml(hotkeyError)}</p>`;
  }
  if (hotkeys === undefined) return `<p>Reading bindings…</p>`;
  if (hotkeys.bindings.length === 0) return `<p>No hotkeys are bound.</p>`;
  return `<ul class="binding-list">${hotkeys.bindings
    .map(
      (binding) => `
        <li class="binding" data-binding="${escapeHtml(binding.command)}" data-source="${escapeHtml(binding.source)}">
          <span class="binding-command">${escapeHtml(bindingLabel(binding.command))}</span>
          <kbd>${escapeHtml(binding.combo)}</kbd>
          <small class="binding-file">${binding.source === "profile" ? "profile · " : ""}${escapeHtml(binding.file)}</small>
        </li>`,
    )
    .join("")}</ul>`;
}

export async function mountLayoutEditor(root: HTMLElement, bridge: DesktopBridge): Promise<void> {
  const [snapshot, initialTilingSettings] = await Promise.all([
    bridge.loadEditorSnapshot(),
    bridge.loadAutomaticTilingSettings(),
  ]);
  let tilingSettings = initialTilingSettings;
  let hotkeys: HotkeyList | undefined;
  let hotkeyError: string | undefined;
  let layouts: SavedLayout[] | undefined;
  let layoutError: string | undefined;
  let selectedLayout: string | undefined;
  let selectedDisplayIndex = 0;
  let selectedZoneId = snapshot.draft.zones[0]?.id ?? 0;
  let commandStatus = "Ready";
  const history: LayoutDraft[] = [];

  const rememberDraft = (): void => {
    history.push(structuredClone(snapshot.draft));
  };

  const nextZoneId = (): number => Math.max(0, ...snapshot.draft.zones.map((zone) => zone.id)) + 1;

  const render = (): void => {
    const selectedZone = snapshot.draft.zones.find((zone) => zone.id === selectedZoneId);
    // Never undefined: the session always offers at least a nominal
    // display, so the canvas has proportions to draw at.
    const display = snapshot.displays[selectedDisplayIndex] ?? snapshot.displays[0]!;
    document.body.className = snapshot.appearance === "dark" ? "night-tide" : "warm-paper";
    root.innerHTML = `
      <main class="spatial-editor">
        <div class="ambient ambient-one"></div><div class="ambient ambient-two"></div>
        <header class="brand-block">
          <span class="brand-mark" aria-hidden="true"><i></i><i></i><i></i><i></i></span>
          <span class="brand-name">Mosaix</span>
          <small data-product-subtitle>Layout Tab</small>
        </header>
        <div class="top-actions">
          <button class="soft-button" data-preview><i></i>Live preview</button>
          <button class="primary-button" data-save-apply>Save &amp; apply</button>
        </div>
        <aside class="tool-dock" aria-label="Canvas tools">
          <button class="icon-button active" aria-label="Select zone">↖</button>
          <button class="icon-button" data-command="split" aria-label="Split zone">⑂</button>
          <button class="icon-button" data-add-zone aria-label="Add zone">＋</button>
        </aside>
        <section class="world" aria-label="Layout canvas">
          <div class="display-meta">
            <span><i></i><select data-display>${snapshot.displays
              .map(
                (display, index) =>
                  `<option value="${index}"${index === selectedDisplayIndex ? " selected" : ""}>${escapeHtml(display.name)}</option>`,
              )
              .join("")}</select></span>
            <span>${escapeHtml(display.resolution)} · ${display.scalePercent}%</span>
          </div>
          <div class="monitor-shell" data-monitor style="aspect-ratio:${display.workAreaWidth} / ${display.workAreaHeight}">
            <div class="work-area">${renderZones(snapshot, selectedZoneId)}<span class="work-label">WORK AREA · ${escapeHtml(display.resolution)}</span></div>
          </div>
          <div class="monitor-foot"><span></span><i></i><span></span></div>
        </section>
        <div class="left-rail">
        <aside class="panel tiling-settings" aria-label="Automatic tiling settings">
          <div class="panel-title">AUTOMATIC TILING</div>
          <p><small>Current topology</small><br><code data-topology-fingerprint>${escapeHtml(tilingSettings.topologyFingerprint)}</code></p>
          <p data-profile-status>${tilingSettings.matchedProfile ? "Matched topology profile" : "No profile yet — saving creates one"}</p>
          <label class="toggle-line"><span>Balanced grid<small>Enable for this whole topology</small></span><input data-auto-tiling type="checkbox" ${tilingSettings.enabled ? "checked" : ""} /></label>
          <label class="field"><span>Outer gap</span><input data-outer-gap type="number" min="0" max="256" value="${tilingSettings.outerGap}" /></label>
          <label class="field"><span>Inner gap</span><input data-inner-gap type="number" min="0" max="256" value="${tilingSettings.innerGap}" /></label>
          <label class="toggle-line"><span>Focus border</span><input data-border-enabled type="checkbox" ${tilingSettings.focusBorderEnabled ? "checked" : ""} /></label>
          <label class="field"><span>RGBA color</span><input data-border-color value="${escapeHtml(tilingSettings.focusBorderColor)}" pattern="#[0-9A-Fa-f]{8}" /></label>
          <label class="field"><span>Thickness</span><input data-border-thickness type="number" min="1" max="16" value="${tilingSettings.focusBorderThickness}" /></label>
          <button class="primary-button" data-save-tiling>Save tiling settings</button>
        </aside>
        <aside class="panel layout-library" aria-label="Saved layouts">
          <div class="panel-title">SAVED LAYOUTS</div>
          <label class="field"><span>Name</span><input data-layout-name value="${escapeHtml(snapshot.draft.name)}" /></label>
          <div class="library-actions">
            <button class="primary-button" data-save-layout>Save layout</button>
            <button class="soft-button" data-rename-layout ${selectedLayout === undefined ? "disabled" : ""}>Rename</button>
          </div>
          ${renderLayouts(layouts, selectedLayout, layoutError)}
        </aside>
        <aside class="panel hotkey-list" aria-label="Hotkey bindings">
          <div class="panel-title">HOTKEYS</div>
          ${renderBindings(hotkeys, hotkeyError)}
        </aside>
        </div>
        ${selectedZone ? `
          <aside class="properties" aria-label="Zone properties">
            <div class="panel-title">ZONE ${String(selectedZone.id).padStart(2, "0")}</div>
            <label class="field"><span>Label</span><input data-zone-name value="${escapeHtml(selectedZone.name)}" /></label>
            <div class="geometry-grid">
              <div><span>X</span><b>${selectedZone.x}.00%</b></div><div><span>Y</span><b>${selectedZone.y}.00%</b></div>
              <div><span>Width</span><b>${selectedZone.width}.00%</b></div><div><span>Height</span><b>${selectedZone.height}.00%</b></div>
            </div>
            <div class="panel-rule"></div>
            <label class="field"><span>Gap</span><div class="range-line"><input data-gap type="range" min="0" max="32" value="${snapshot.draft.gap}" /><output>${snapshot.draft.gap}px</output></div></label>
            <label class="toggle-line"><span>Allow zone overlap<small>Zones can share the same space</small></span><input data-allow-overlap type="checkbox" ${snapshot.draft.allowOverlap ? "checked" : ""} /></label>
          </aside>` : ""}
        <nav class="command-dock" aria-label="Zone commands">
          <button data-command="undo" ${history.length === 0 ? "disabled" : ""}><kbd>⌘ Z</kbd> Undo</button><button data-command="split"><kbd>S</kbd> Split</button><button data-command="duplicate"><kbd>D</kbd> Duplicate</button><button data-command="delete"><kbd>⌫</kbd> Delete</button>
          <button class="new-zone" data-add-zone>＋ New zone</button>
        </nav>
        <nav class="appearance-toggle" aria-label="Appearance">
          <button data-appearance="dark" aria-pressed="${snapshot.appearance === "dark"}"><i>☾</i><span>Dark<small>Night Tide</small></span></button>
          <button data-appearance="light" aria-pressed="${snapshot.appearance === "light"}"><i>☀</i><span>Light<small>Warm Paper</small></span></button>
        </nav>
        <div class="command-status" role="status">${commandStatus}</div>
      </main>`;

    root.querySelector<HTMLSelectElement>("[data-display]")?.addEventListener("change", (event) => {
      selectedDisplayIndex = Number((event.currentTarget as HTMLSelectElement).value);
      render();
    });
    root.querySelector<HTMLInputElement>("[data-layout-name]")?.addEventListener("change", (event) => {
      snapshot.draft.name = (event.currentTarget as HTMLInputElement).value;
    });
    /// Runs one configuration write and reports what the agent said. The
    /// saved-layout list is re-read afterwards so the panel reflects the
    /// write the agent actually made, rather than the one asked for.
    const write = (
      pending: string,
      done: (receipt: LayoutWriteReceipt) => string,
      request: () => Promise<LayoutWriteReceipt>,
    ): void => {
      commandStatus = pending;
      render();
      void request()
        .then(async (receipt) => {
          commandStatus = done(receipt);
          layouts = await bridge.loadSavedLayouts();
          layoutError = undefined;
          render();
        })
        .catch((error: unknown) => {
          commandStatus = `${pending.replace("…", "")} failed · ${String(error)}`;
          render();
        });
    };
    root.querySelector<HTMLElement>("[data-save-layout]")?.addEventListener("click", () => {
      const draft = structuredClone(snapshot.draft);
      write(
        "Saving layout…",
        (receipt) => `Saved to ${receipt.file}`,
        () => bridge.saveLayout(draft).then((receipt) => {
          selectedLayout = draft.name;
          return receipt;
        }),
      );
    });
    root.querySelector<HTMLElement>("[data-rename-layout]")?.addEventListener("click", () => {
      const from = selectedLayout;
      const to = snapshot.draft.name;
      if (from === undefined) return;
      write(
        "Renaming layout…",
        (receipt) => `Renamed in ${receipt.file}`,
        () => bridge.renameLayout(from, to).then((receipt) => {
          selectedLayout = to;
          return receipt;
        }),
      );
    });
    root.querySelectorAll<HTMLElement>("[data-duplicate-layout]").forEach((button) => {
      button.addEventListener("click", () => {
        const from = button.dataset.duplicateLayout!;
        write(
          "Duplicating layout…",
          (receipt) => `Duplicated in ${receipt.file}`,
          () => bridge.duplicateLayout(from, `${from} copy`),
        );
      });
    });
    root.querySelectorAll<HTMLElement>("[data-delete-layout]").forEach((button) => {
      button.addEventListener("click", () => {
        const name = button.dataset.deleteLayout!;
        write(
          "Deleting layout…",
          (receipt) => `Deleted from ${receipt.file}`,
          () => bridge.deleteLayout(name).then((receipt) => {
            if (selectedLayout === name) selectedLayout = undefined;
            return receipt;
          }),
        );
      });
    });
    root.querySelectorAll<HTMLElement>("[data-open-layout]").forEach((button) => {
      button.addEventListener("click", () => {
        const name = button.dataset.openLayout!;
        const layout = layouts?.find((candidate) => candidate.name === name);
        if (layout === undefined) return;
        // Opening a saved layout replaces the draft deliberately -- it is
        // the one action that is meant to discard what is on the canvas.
        rememberDraft();
        selectedLayout = name;
        snapshot.draft = { ...snapshot.draft, name, zones: structuredClone(layout.cells) };
        selectedZoneId = snapshot.draft.zones[0]?.id ?? 0;
        commandStatus = `Editing ${name}`;
        render();
      });
    });
    root.querySelectorAll<HTMLElement>("[data-zone]").forEach((zone) => {
      zone.addEventListener("click", () => {
        selectedZoneId = Number(zone.dataset.zone);
        render();
      });
    });
    root.querySelector<HTMLInputElement>("[data-auto-tiling]")?.addEventListener("change", (event) => {
      tilingSettings.enabled = (event.currentTarget as HTMLInputElement).checked;
    });
    root.querySelector<HTMLInputElement>("[data-outer-gap]")?.addEventListener("change", (event) => {
      tilingSettings.outerGap = Number((event.currentTarget as HTMLInputElement).value);
    });
    root.querySelector<HTMLInputElement>("[data-inner-gap]")?.addEventListener("change", (event) => {
      tilingSettings.innerGap = Number((event.currentTarget as HTMLInputElement).value);
    });
    root.querySelector<HTMLInputElement>("[data-border-enabled]")?.addEventListener("change", (event) => {
      tilingSettings.focusBorderEnabled = (event.currentTarget as HTMLInputElement).checked;
    });
    root.querySelector<HTMLInputElement>("[data-border-color]")?.addEventListener("change", (event) => {
      tilingSettings.focusBorderColor = (event.currentTarget as HTMLInputElement).value;
    });
    root.querySelector<HTMLInputElement>("[data-border-thickness]")?.addEventListener("change", (event) => {
      tilingSettings.focusBorderThickness = Number((event.currentTarget as HTMLInputElement).value);
    });
    root.querySelector<HTMLElement>("[data-save-tiling]")?.addEventListener("click", () => {
      commandStatus = "Saving tiling settings…";
      void bridge.saveAutomaticTilingSettings(structuredClone(tilingSettings)).then((saved) => {
        tilingSettings = saved;
        commandStatus = "Automatic tiling settings saved";
        render();
      }).catch((error: unknown) => {
        commandStatus = `Tiling settings failed · ${String(error)}`;
        render();
      });
    });
    root.querySelector<HTMLInputElement>("[data-zone-name]")?.addEventListener("change", (event) => {
      const selected = snapshot.draft.zones.find((zone) => zone.id === selectedZoneId);
      if (selected) {
        rememberDraft();
        selected.name = (event.currentTarget as HTMLInputElement).value || "Untitled zone";
      }
      render();
    });
    root.querySelector<HTMLInputElement>("[data-gap]")?.addEventListener("change", (event) => {
      rememberDraft();
      snapshot.draft.gap = Number((event.currentTarget as HTMLInputElement).value);
      render();
    });
    root.querySelector<HTMLInputElement>("[data-allow-overlap]")?.addEventListener("change", (event) => {
      rememberDraft();
      snapshot.draft.allowOverlap = (event.currentTarget as HTMLInputElement).checked;
      render();
    });
    root.querySelectorAll<HTMLElement>("[data-add-zone]").forEach((button) => {
      button.addEventListener("click", () => {
        rememberDraft();
        const id = nextZoneId();
        snapshot.draft.zones.push({ id, name: `Zone ${id}`, x: 69, y: 69, width: 29, height: 29 });
        selectedZoneId = id;
        render();
      });
    });
    root.querySelectorAll<HTMLElement>("[data-command]").forEach((button) => {
      button.addEventListener("click", () => {
        const command = button.dataset.command;
        const selectedIndex = snapshot.draft.zones.findIndex((zone) => zone.id === selectedZoneId);
        const selected = snapshot.draft.zones[selectedIndex];

        if (command === "undo") {
          const previous = history.pop();
          if (previous) snapshot.draft = previous;
          if (!snapshot.draft.zones.some((zone) => zone.id === selectedZoneId)) {
            selectedZoneId = snapshot.draft.zones[0]?.id ?? 0;
          }
        } else if (command === "split" && selected) {
          rememberDraft();
          const sibling = structuredClone(selected);
          sibling.id = nextZoneId();
          sibling.name = `${selected.name} 2`;
          if (selected.width >= selected.height) {
            const firstWidth = selected.width / 2;
            selected.width = firstWidth;
            sibling.x = selected.x + firstWidth;
            sibling.width -= firstWidth;
          } else {
            const firstHeight = selected.height / 2;
            selected.height = firstHeight;
            sibling.y = selected.y + firstHeight;
            sibling.height -= firstHeight;
          }
          snapshot.draft.zones.splice(selectedIndex + 1, 0, sibling);
          selectedZoneId = sibling.id;
        } else if (command === "duplicate" && selected) {
          rememberDraft();
          const duplicate = structuredClone(selected);
          duplicate.id = nextZoneId();
          duplicate.name = `${selected.name} copy`;
          snapshot.draft.zones.push(duplicate);
          selectedZoneId = duplicate.id;
        } else if (command === "delete" && selected && snapshot.draft.zones.length > 1) {
          rememberDraft();
          snapshot.draft.zones.splice(selectedIndex, 1);
          selectedZoneId = snapshot.draft.zones[Math.min(selectedIndex, snapshot.draft.zones.length - 1)]!.id;
        }
        render();
      });
    });
    root.querySelector<HTMLElement>("[data-preview]")?.addEventListener("click", () => {
      commandStatus = "Previewing…";
      void bridge.previewLayout(structuredClone(snapshot.draft)).then((receipt) => {
        commandStatus = `Preview active · revision ${receipt.revision}`;
        render();
      }).catch((error: unknown) => {
        commandStatus = `Preview failed · ${String(error)}`;
        render();
      });
    });
    root.querySelector<HTMLElement>("[data-save-apply]")?.addEventListener("click", () => {
      commandStatus = "Applying…";
      void bridge.saveAndApplyLayout(structuredClone(snapshot.draft)).then((receipt) => {
        commandStatus = `Applied · revision ${receipt.revision}`;
        render();
      }).catch((error: unknown) => {
        commandStatus = `Apply failed · ${String(error)}`;
        render();
      });
    });
    root.querySelectorAll<HTMLElement>("[data-appearance]").forEach((button) => {
      button.addEventListener("click", () => {
        const appearance = button.dataset.appearance as Appearance;
        const previousAppearance = snapshot.appearance;
        commandStatus = "Changing appearance…";
        void bridge.setAppearance(appearance).then(() => {
          snapshot.appearance = appearance;
          commandStatus = "Appearance updated";
          render();
        }).catch((error: unknown) => {
          snapshot.appearance = previousAppearance;
          commandStatus = `Appearance failed · ${String(error)}`;
          render();
        });
      });
    });
  };

  render();

  // The saved-layout list is re-read on the same cadence as the bindings,
  // so a layout added by hand in the file shows up here. Only the *list*
  // is replaced: the draft on the canvas is the user's in-progress work,
  // and the echo of the editor's own write must not discard it.
  watchSavedLayouts(bridge, {
    onChange: (list) => {
      layouts = list;
      layoutError = undefined;
      render();
    },
    onError: (error) => {
      layoutError = `Could not read saved layouts · ${String(error)}`;
      render();
    },
  });
  watchHotkeyBindings(bridge, {
    onChange: (list) => {
      hotkeys = list;
      hotkeyError = undefined;
      render();
    },
    onError: (error) => {
      hotkeyError = `Could not read hotkeys · ${String(error)}`;
      render();
    },
  });
}
