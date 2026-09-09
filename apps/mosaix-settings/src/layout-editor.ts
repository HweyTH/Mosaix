import {
  blurCapture,
  capturedCombo,
  capturedLabel,
  focusCapture,
  isBareKey,
  openCapture,
  pressKey,
  releaseKey,
  type CaptureSession,
} from "./hotkey-capture";

import {
  bindWorkspaceRepairs,
  renderWorkspaceStatus,
  type RepairReceipt,
  type WorkspaceStatus,
} from "./workspace-status";

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
  /**
   * The work area's pixel dimensions. The canvas is drawn at these
   * proportions, so a preview shows the shape the layout will really take.
   */
  workAreaWidth: number;
  workAreaHeight: number;
}

export interface EditorSnapshot {
  appearance: Appearance;
  /**
   * Every display a layout can be previewed against, primary first.
   */
  displays: DisplaySummary[];
  draft: LayoutDraft;
}

export interface SavedLayout {
  name: string;
  cells: ZoneDraft[];
  /** Which layer supplies it: `base`, `profile`, or `unknown`. */
  source: "base" | "profile" | "unknown";
  /**
   * The configuration file supplying it, and so the file a save of it
   * would be written to. Absent when the agent could not name it.
   */
  file: string | null;
}

/**
 * The saved layouts on offer, plus the file a layout that does not exist
 * yet would be created in -- which the interface needs to name a
 * destination for a new drawing too.
 */
export interface SavedLayoutList {
  baseFile: string;
  layouts: SavedLayout[];
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

/**
 * One bindable command as the interface shows it. `file` is the
 * configuration file that currently supplies it, and so the file an edit
 * of it would be written to.
 *
 * Every command appears, bound or not: a saved layout that is not yet a
 * keystroke away is exactly the one a user wants to reach, and a binding
 * reset out of existence has to leave a row to bind again.
 */
export interface HotkeyBinding {
  command: string;
  layout: string | null;
  /** `null` for a command nothing is bound to. */
  combo: string | null;
  source: "base" | "profile" | "unbound" | "unknown";
  /** Absent when nothing supplies this binding, or the agent could not name it. */
  file: string | null;
}

/** The agent's verdict on a combination the user just pressed. */
export interface HotkeyProbeResult {
  availability:
    | "available"
    | "mosaix_binding"
    | "system_or_other_application"
    | "reserved"
    | "unsupported";
  /** The Mosaix binding already using it, for `mosaix_binding`. */
  command: string | null;
  /** An advisory that does not block the binding, such as F12. */
  warning: string | null;
  /** Why Mosaix cannot express the combination, for `unsupported`. */
  reason: string | null;
}

export interface BindingWriteReceipt {
  file: string;
  /** Absent when the edit left the command unbound. */
  combo: string | null;
}

export interface HotkeyList {
  topologyFingerprint: string;
  bindings: HotkeyBinding[];
  /**
   * The file a binding neither layer carries yet would be created in.
   */
  baseFile: string;
  /**
   * Whether the agent currently has every binding unregistered for a
   * hotkey editor. While it is true no Mosaix hotkey works anywhere on the
   * system, so the interface says so rather than leaving the user to read
   * it as Mosaix having stopped working.
   */
  captureSuspended: boolean;
  /**
   * Commands whose bindings did not come back from the last registration
   * pass -- a combination another application took while capture held
   * registration suspended.
   */
  unregisteredCommands: string[];
}

export interface DesktopBridge {
  loadEditorSnapshot(): Promise<EditorSnapshot>;
  loadHotkeyBindings(): Promise<HotkeyList>;
  startHotkeyCapture(): Promise<void>;
  endHotkeyCapture(): Promise<void>;
  probeHotkey(combo: string, forCommand: string): Promise<HotkeyProbeResult>;
  setBinding(command: string, combo: string, toBase: boolean): Promise<BindingWriteReceipt>;
  resetBinding(command: string): Promise<BindingWriteReceipt>;
  loadSavedLayouts(): Promise<SavedLayoutList>;
  saveLayout(draft: LayoutDraft, toBase: boolean): Promise<LayoutWriteReceipt>;
  renameLayout(from: string, to: string): Promise<LayoutWriteReceipt>;
  duplicateLayout(from: string, to: string): Promise<LayoutWriteReceipt>;
  deleteLayout(name: string): Promise<LayoutWriteReceipt>;
  previewLayout(draft: LayoutDraft): Promise<CommandReceipt>;
  saveAndApplyLayout(draft: LayoutDraft): Promise<CommandReceipt>;
  setAppearance(appearance: Appearance): Promise<void>;
  loadAutomaticTilingSettings(): Promise<AutomaticTilingSettings>;
  saveAutomaticTilingSettings(settings: AutomaticTilingSettings): Promise<AutomaticTilingSettings>;
  loadWorkspaceStatus(): Promise<WorkspaceStatus>;
  restoreParkedWindows(): Promise<RepairReceipt>;
  restoreWorkspaceSwitch(): Promise<RepairReceipt>;
}

/**
 * The label for a command's TOML path: `snap-left` becomes "Snap left",
 * and `apply-layout.writing` becomes "Apply layout · writing".
 *
 * Reads the path rather than carrying a table of pretty names, so a verb
 * added to the schema shows up here without a second edit -- at the cost
 * of a label that is only as good as the verb's spelling, which is the
 * right trade for a list a user scans rather than reads.
 */
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

/**
 * Calls `read` every `intervalMs`, reporting only when the answer has
 * actually changed. Returns a function that stops it.
 *
 * Polling rather than a push from the agent: the IPC protocol answers
 * requests and never initiates, so a settings window that wants to notice
 * a change made elsewhere -- docking a laptop, hand-editing a config file
 * -- has to ask.
 *
 * Reporting only changes is what keeps this from re-rendering every tick,
 * and it applies to failures too, so an agent that is not running is
 * reported once rather than twice a second.
 */
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

/**
 * Watches the hotkey bindings. A topology change swaps the matched
 * profile, and with it both the combinations on screen and the files
 * behind them.
 */
export function watchHotkeyBindings(
  bridge: Pick<DesktopBridge, "loadHotkeyBindings">,
  handlers: WatchHandlers<HotkeyList>,
  intervalMs = 2000,
): () => void {
  return watchChanges(() => bridge.loadHotkeyBindings(), handlers, intervalMs);
}

/**
 * Watches the saved-layout set, so a layout added by hand in a
 * configuration file appears without reopening the window.
 */
export function watchSavedLayouts(
  bridge: Pick<DesktopBridge, "loadSavedLayouts">,
  handlers: WatchHandlers<SavedLayoutList>,
  intervalMs = 2000,
): () => void {
  return watchChanges(() => bridge.loadSavedLayouts(), handlers, intervalMs);
}

/**
 * Watches the experimental switching surface, so a profile match, a
 * parking-site verification, or a failed switch appears without
 * reopening the window (issue #63).
 */
export function watchWorkspaceStatus(
  bridge: Pick<DesktopBridge, "loadWorkspaceStatus">,
  handlers: WatchHandlers<WorkspaceStatus>,
  intervalMs = 2000,
): () => void {
  return watchChanges(() => bridge.loadWorkspaceStatus(), handlers, intervalMs);
}

/**
 * Where a configuration write would land, and whether the redirect
 * control can change it.
 *
 * The same question for a saved layout and for a binding, so it has one
 * answer shape and one rendering ([`renderWriteDestination`]).
 */
export interface WriteDestination {
  file: string;
  redirectable: boolean;
}

/**
 * The configuration file a rebind of `binding` would land in, and whether
 * the redirect control can change it.
 *
 * The same rule saved layouts follow: the write goes to the layer that
 * currently supplies the value, and only a profile-supplied one has
 * anywhere to be redirected from.
 */
export function bindingDestination(
  binding: HotkeyBinding | undefined,
  list: HotkeyList | undefined,
  toBase: boolean,
): WriteDestination {
  const baseFile = list?.baseFile ?? "your base configuration";
  if (binding === undefined || binding.source !== "profile") {
    return { file: binding?.file ?? baseFile, redirectable: false };
  }
  return { file: toBase ? baseFile : (binding.file ?? baseFile), redirectable: true };
}

/**
 * The configuration file a save of `name` would land in, and whether the
 * redirect control can change it.
 *
 * The destination is the layer that currently supplies the layout. Two
 * cases leave nothing for a redirect to do: a layout base config already
 * supplies, and a name no layer declares yet -- a new layout goes to base
 * config regardless, so it is available at every desk.
 *
 * Names are matched the way configuration matches them, ignoring case, so
 * what the interface promises and where the agent writes cannot disagree
 * over `Writing` and `writing`.
 */
export function saveDestination(
  name: string,
  list: SavedLayoutList | undefined,
  toBase: boolean,
): WriteDestination {
  const baseFile = list?.baseFile ?? "your base configuration";
  const existing = list?.layouts.find(
    (layout) => layout.name.trim().toLowerCase() === name.trim().toLowerCase(),
  );
  if (existing === undefined || existing.source !== "profile") {
    return { file: existing?.file ?? baseFile, redirectable: false };
  }
  return { file: toBase ? baseFile : (existing.file ?? baseFile), redirectable: true };
}

/**
 * The attribute name behind a `dataset` key: `layoutName` is written
 * `data-layout-name`.
 */
function camelToDataAttribute(key: string): string {
  return key.replace(/[A-Z]/g, (letter) => `-${letter.toLowerCase()}`);
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
  list: SavedLayoutList | undefined,
  selected: string | undefined,
  error: string | undefined,
): string {
  if (error !== undefined) return `<p data-layout-error>${escapeHtml(error)}</p>`;
  if (list === undefined) return `<p>Reading saved layouts…</p>`;
  if (list.layouts.length === 0) return `<p>No saved layouts yet.</p>`;
  // `data-source` carries the same distinction the binding list draws, so
  // a desk-specific layout looks desk-specific at a glance rather than
  // only once its destination is read.
  return `<ul class="library-list">${list.layouts
    .map(
      (layout) => `
        <li class="library-item${layout.name === selected ? " active" : ""}" data-layout="${escapeHtml(layout.name)}" data-source="${escapeHtml(layout.source)}">
          <button class="layout-open" data-open-layout="${escapeHtml(layout.name)}">${escapeHtml(layout.name)}</button>
          <small class="library-detail">${layout.cells.length} zone${layout.cells.length === 1 ? "" : "s"} · ${escapeHtml(layout.file ?? "")}</small>
          <span class="layout-actions">
            <button data-duplicate-layout="${escapeHtml(layout.name)}" title="Duplicate">⧉</button>
            <button data-delete-layout="${escapeHtml(layout.name)}" title="Delete">⌫</button>
          </span>
        </li>`,
    )
    .join("")}</ul>`;
}

/**
 * Where the next write will go, said before it happens rather than in the
 * receipt afterwards.
 *
 * Whenever a profile is matched, the value on screen and the file that
 * would receive the write are different objects, so naming the destination
 * is load-bearing rather than decorative -- and the redirect beside it is
 * the only way to move a desk-specific layout or binding out of its
 * profile without editing TOML.
 *
 * `marker` is the data attribute a test reaches for, which is the only
 * thing that differs between the layout panel's copy and the capture
 * dialog's.
 */
function renderWriteDestination(
  destination: WriteDestination,
  toBase: boolean,
  marker: string,
): string {
  const redirect = destination.redirectable
    ? `<label class="toggle-line redirect"><span>Save to base config</span><input data-${marker}-redirect type="checkbox" ${toBase ? "checked" : ""} /></label>`
    : "";
  return `<p class="save-destination" data-${marker}-destination>Saves to <code>${escapeHtml(destination.file)}</code></p>${redirect}`;
}

/**
 * What the user needs told about the state of registration, above the list
 * itself: that hotkeys are off while the editor holds them, and which of
 * them another application took while they were off.
 *
 * Both are stated rather than left to be inferred from a shortcut that
 * has stopped working.
 */
function renderRegistrationNotices(hotkeys: HotkeyList): string {
  const suspended = hotkeys.captureSuspended
    ? `<p class="binding-notice" data-capture-suspended>Hotkeys are off while this window is open.</p>`
    : "";
  const missing =
    hotkeys.unregisteredCommands.length > 0
      ? `<p class="binding-notice warning" data-unregistered-bindings>Taken by another app · ${hotkeys.unregisteredCommands
          .map((command) => escapeHtml(bindingLabel(command)))
          .join(", ")}</p>`
      : "";
  return `${suspended}${missing}`;
}

function renderBindings(hotkeys: HotkeyList | undefined, hotkeyError: string | undefined): string {
  if (hotkeyError !== undefined) {
    return `<p data-hotkey-error>${escapeHtml(hotkeyError)}</p>`;
  }
  if (hotkeys === undefined) return `<p>Reading bindings…</p>`;
  const notices = renderRegistrationNotices(hotkeys);
  if (hotkeys.bindings.length === 0) return `${notices}<p>No commands to bind.</p>`;
  return `${notices}<ul class="binding-list">${hotkeys.bindings
    .map(
      (binding) => `
        <li class="binding" data-binding="${escapeHtml(binding.command)}" data-source="${escapeHtml(binding.source)}">
          <span class="binding-command">${escapeHtml(bindingLabel(binding.command))}</span>
          <kbd>${binding.combo === null ? "not bound" : escapeHtml(binding.combo)}</kbd>
          <small class="binding-file">${escapeHtml(binding.file ?? "")}</small>
          <span class="binding-actions">
            <button data-rebind="${escapeHtml(binding.command)}" title="${binding.combo === null ? "Bind" : "Rebind"}">⌨</button>
            <button data-reset-binding="${escapeHtml(binding.command)}" title="Reset to default" ${binding.combo === null ? "disabled" : ""}>↺</button>
          </span>
        </li>`,
    )
    .join("")}</ul>`;
}

/**
 * The "press your combination" dialog: what was captured, what the agent
 * said about it, where a save would land, and whether saving is allowed
 * yet.
 *
 * Rendered as part of the page rather than as a second window, because
 * suspension is already held for the editor's whole lifetime -- the
 * dialog's own job is only the fine scope, arming the buffer while it is
 * frontmost.
 */
function renderCaptureDialog(
  capture: CaptureDialog | undefined,
  hotkeys: HotkeyList | undefined,
): string {
  if (capture === undefined) return "";
  const combo = capturedCombo(capture.session);
  const binding = hotkeys?.bindings.find(
    (candidate) => candidate.command === capture.command,
  );
  const destination = bindingDestination(binding, hotkeys, capture.toBase);
  // Three verdicts are blocked rather than merely warned, and each for a
  // reason the user cannot argue with:
  //
  // - `reserved` -- Windows handles the combination itself, so a binding
  //   to it would be accepted and then never fire.
  // - `unsupported` -- Mosaix has no virtual-key code for the key.
  // - `mosaix_binding` -- whole-directory validation rejects two commands
  //   on one combination, so this write can only ever be refused. Saving a
  //   conflicting binding deliberately is about a combination *another
  //   application* owns, which Mosaix cannot arbitrate; one Mosaix owns is
  //   the case the user can resolve themselves, by freeing it first.
  const blocked =
    capture.verdict !== undefined &&
    ["reserved", "unsupported", "mosaix_binding"].includes(
      capture.verdict.availability,
    );
  const savable = combo !== null && !blocked;
  return `
    <div class="capture-backdrop" data-capture-dialog role="dialog" aria-modal="true" aria-label="Rebind ${escapeHtml(bindingLabel(capture.command))}">
      <div class="capture-dialog">
        <div class="panel-title">REBIND ${escapeHtml(bindingLabel(capture.command)).toUpperCase()}</div>
        <div class="capture-combo${combo === null ? " incomplete" : ""}" data-captured-combo>${escapeHtml(capturedLabel(capture.session))}</div>
        ${renderVerdict(capture.verdict)}${
          isBareKey(capture.session)
            ? `<p class="capture-verdict warning" data-bare-key>No modifier · taken from every app</p>`
            : ""
        }
        ${renderWriteDestination(destination, capture.toBase, "binding")}
        <div class="library-actions">
          <button class="primary-button" data-capture-save ${savable ? "" : "disabled"}>Save binding</button>
          <button class="soft-button" data-capture-cancel>Cancel</button>
        </div>
      </div>
    </div>`;
}

/**
 * The agent's verdict in a sentence, kept apart from the dialog's own
 * markup so each case reads as the thing the user has to decide about.
 *
 * A conflict names its owner and is still savable: Mosaix does not
 * overrule the user about their own machine.
 */
function renderVerdict(verdict: HotkeyProbeResult | undefined): string {
  if (verdict === undefined) return "";
  const notice = (kind: string, text: string): string =>
    `<p class="capture-verdict ${kind}" data-capture-verdict>${text}</p>`;
  const warning =
    verdict.warning === null
      ? ""
      : `<p class="capture-verdict warning" data-capture-warning>${escapeHtml(verdict.warning)}</p>`;
  switch (verdict.availability) {
    case "available":
      return `${notice("ok", "Available")}${warning}`;
    case "mosaix_binding":
      return `${notice("blocked", `Bound to ${escapeHtml(bindingLabel(verdict.command ?? ""))} · free it first`)}${warning}`;
    case "system_or_other_application":
      return `${notice("warning", "Taken by another app")}${warning}`;
    case "reserved":
      return `${notice("blocked", "Reserved by Windows")}${warning}`;
    case "unsupported":
      return `${notice("blocked", escapeHtml(verdict.reason ?? "Mosaix cannot express this combination"))}${warning}`;
  }
}

/** The open capture dialog's own state. */
interface CaptureDialog {
  /** The command being rebound, as its TOML path. */
  command: string;
  session: CaptureSession;
  /** The agent's answer about what is captured, once it has arrived. */
  verdict: HotkeyProbeResult | undefined;
  /** Whether the save is redirected to base config. */
  toBase: boolean;
}

/**
 * Mounts the editor into `root`. Returns a function that stops the
 * background reads it starts -- the window owns them for its lifetime, so
 * only a test normally calls it.
 */
export async function mountLayoutEditor(
  root: HTMLElement,
  bridge: DesktopBridge,
): Promise<() => void> {
  const [snapshot, initialTilingSettings] = await Promise.all([
    bridge.loadEditorSnapshot(),
    bridge.loadAutomaticTilingSettings(),
  ]);
  let tilingSettings = initialTilingSettings;
  let hotkeys: HotkeyList | undefined;
  let hotkeyError: string | undefined;
  let layouts: SavedLayoutList | undefined;
  let layoutError: string | undefined;
  let workspaceStatus: WorkspaceStatus | undefined;
  let workspaceError: string | undefined;
  let repairStatus: string | undefined;
  let selectedLayout: string | undefined;
  /** Whether the next save is redirected to base config. */
  let redirectToBase = false;
  /** The open capture dialog, if any. */
  let capture: CaptureDialog | undefined;
  let selectedDisplayIndex = 0;
  let selectedZoneId = snapshot.draft.zones[0]?.id ?? 0;
  let commandStatus = "Ready";
  const history: LayoutDraft[] = [];

  const rememberDraft = (): void => {
    history.push(structuredClone(snapshot.draft));
  };

  const nextZoneId = (): number => Math.max(0, ...snapshot.draft.zones.map((zone) => zone.id)) + 1;

  /**
   * Which field had focus, and where the caret was, so a re-render driven
   * by a watch tick does not interrupt someone typing.
   */
  const focusedField = (): { key: string; start: number | null } | undefined => {
    const active = document.activeElement;
    if (!(active instanceof HTMLInputElement)) return undefined;
    const key = Object.keys(active.dataset)[0];
    return key === undefined ? undefined : { key, start: active.selectionStart };
  };

  const restoreFocus = (field: { key: string; start: number | null } | undefined): void => {
    if (field === undefined) return;
    const input = root.querySelector<HTMLInputElement>(`[data-${camelToDataAttribute(field.key)}]`);
    if (input === null) return;
    input.focus();
    if (field.start !== null) input.setSelectionRange(field.start, field.start);
  };

  const render = (): void => {
    const focused = focusedField();
    const selectedZone = snapshot.draft.zones.find((zone) => zone.id === selectedZoneId);
    // Never undefined: the session always offers at least a nominal
    // display, so the canvas has proportions to draw at.
    const display = snapshot.displays[selectedDisplayIndex] ?? snapshot.displays[0]!;
    const destination = saveDestination(snapshot.draft.name, layouts, redirectToBase);
    document.body.className = snapshot.appearance === "dark" ? "night-tide" : "warm-paper";
    root.innerHTML = `
      <main class="spatial-editor">
        <div class="ambient ambient-one"></div><div class="ambient ambient-two"></div>
        <header class="brand-block">
          <span class="brand-mark" aria-label="Mosaix" role="img"><i></i><i></i><i></i><i></i></span>
        </header>
        <div class="top-actions">
          <nav class="appearance-toggle" aria-label="Appearance">
            <button data-appearance="dark" aria-pressed="${snapshot.appearance === "dark"}" title="Dark">☾</button>
            <button data-appearance="light" aria-pressed="${snapshot.appearance === "light"}" title="Light">☀</button>
          </nav>
          <button class="soft-button" data-preview><i></i>Live preview</button>
          <button class="primary-button" data-save-apply>Save &amp; apply</button>
        </div>
        <aside class="tool-dock" aria-label="Canvas tools">
          <button class="icon-button active" aria-label="Select zone" title="Select">↖</button>
          <button class="icon-button" data-command="split" aria-label="Split zone" title="Split">◫</button>
          <button class="icon-button" data-add-zone aria-label="Add zone" title="New zone">＋</button>
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
            <div class="work-area">${renderZones(snapshot, selectedZoneId)}</div>
          </div>
        </section>
        <div class="left-rail">
        <aside class="panel tiling-settings" aria-label="Automatic tiling settings">
          <div class="panel-title">AUTOMATIC TILING</div>
          <p><small>Current topology</small><br><code data-topology-fingerprint title="${escapeHtml(tilingSettings.topologyFingerprint)}">${escapeHtml(tilingSettings.topologyFingerprint)}</code></p>
          <p data-profile-status>${tilingSettings.matchedProfile ? "Matched topology profile" : "No profile yet — saving creates one"}</p>
          <label class="toggle-line"><span>Balanced grid</span><input data-auto-tiling type="checkbox" ${tilingSettings.enabled ? "checked" : ""} /></label>
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
          ${renderWriteDestination(destination, redirectToBase, "layout")}
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
        <aside class="panel workspace-status" aria-label="Experimental workspace switching">
          <div class="panel-title">WORKSPACES <small class="experimental-tag">EXPERIMENTAL</small></div>
          ${renderWorkspaceStatus(workspaceStatus, workspaceError, repairStatus)}
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
            <label class="toggle-line"><span>Allow zone overlap</span><input data-allow-overlap type="checkbox" ${snapshot.draft.allowOverlap ? "checked" : ""} /></label>
          </aside>` : ""}
        <nav class="command-dock" aria-label="Zone commands">
          <button data-command="undo" ${history.length === 0 ? "disabled" : ""}><kbd>⌘ Z</kbd> Undo</button><button data-command="split"><kbd>S</kbd> Split</button><button data-command="duplicate"><kbd>D</kbd> Duplicate</button><button data-command="delete"><kbd>⌫</kbd> Delete</button>
          <button class="new-zone" data-add-zone>＋ New zone</button>
        </nav>
        <div class="command-status" role="status">${commandStatus}</div>
        ${renderCaptureDialog(capture, hotkeys)}
      </main>`;

    bindWorkspaceRepairs(root, bridge, (message) => {
      repairStatus = message;
      render();
    });

    root.querySelector<HTMLSelectElement>("[data-display]")?.addEventListener("change", (event) => {
      selectedDisplayIndex = Number((event.currentTarget as HTMLSelectElement).value);
      render();
    });
    // `input`, not `change`: a watch tick can re-render the panel while the
    // user is still typing, and a name only committed on blur would be
    // thrown away with the old markup.
    root.querySelector<HTMLInputElement>("[data-layout-name]")?.addEventListener("input", (event) => {
      snapshot.draft.name = (event.currentTarget as HTMLInputElement).value;
      // Re-rendered so the destination follows the name being typed: a
      // name that starts matching a profile's layout changes where the
      // save would land, and saying so late is saying it too late.
      render();
    });
    root.querySelector<HTMLInputElement>("[data-layout-redirect]")?.addEventListener("change", (event) => {
      redirectToBase = (event.currentTarget as HTMLInputElement).checked;
      render();
    });
    /**
     * Runs one configuration write and reports what the agent said. The
     * saved-layout list is re-read afterwards so the panel reflects the
     * write the agent actually made, rather than the one asked for.
     */
    /**
     * Runs one configuration write and reports what the agent said, then
     * re-reads through `refresh` so the panel reflects the write the
     * agent actually made rather than the one asked for.
     */
    const request = <T,>(
      pending: string,
      call: () => Promise<T>,
      done: (result: T) => string,
      refresh: () => Promise<void>,
    ): void => {
      commandStatus = pending;
      render();
      void call()
        .then(async (result) => {
          commandStatus = done(result);
          await refresh();
          render();
        })
        .catch((error: unknown) => {
          commandStatus = `${pending.replace("…", "")} failed · ${String(error)}`;
          render();
        });
    };
    const reloadLayouts = async (): Promise<void> => {
      layouts = await bridge.loadSavedLayouts();
      layoutError = undefined;
    };
    const reloadHotkeys = async (): Promise<void> => {
      hotkeys = await bridge.loadHotkeyBindings();
      hotkeyError = undefined;
    };
    const write = (
      pending: string,
      done: (receipt: LayoutWriteReceipt) => string,
      call: () => Promise<LayoutWriteReceipt>,
    ): void => request(pending, call, done, reloadLayouts);
    root.querySelector<HTMLElement>("[data-save-layout]")?.addEventListener("click", () => {
      const draft = structuredClone(snapshot.draft);
      // Saving replaces the cells of a layout that already carries this
      // name. That is what a user editing an open layout means, and the
      // opposite of what a user naming a new drawing means -- so the
      // second case is reported here rather than silently overwriting a
      // layout they never opened.
      const clash = layouts?.layouts.find(
        (layout) =>
          layout.name !== selectedLayout &&
          layout.name.toLowerCase() === draft.name.trim().toLowerCase(),
      );
      if (clash !== undefined) {
        commandStatus = `A saved layout named ${clash.name} already exists · open it to edit, or choose another name`;
        render();
        return;
      }
      write(
        "Saving layout…",
        (receipt) => `Saved to ${receipt.file}`,
        () => bridge.saveLayout(draft, redirectToBase).then((receipt) => {
          selectedLayout = draft.name;
          // The layout now comes from wherever it was just written, so
          // the redirect has done its job and stops applying to the next
          // save of a layout that is no longer profile-supplied.
          redirectToBase = false;
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
        const layout = layouts?.layouts.find((candidate) => candidate.name === name);
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

    root.querySelectorAll<HTMLElement>("[data-rebind]").forEach((button) => {
      button.addEventListener("click", (event) => {
        // The click's own modifier flags are the snapshot: a user who
        // opened this while holding Ctrl must not have Ctrl baked into
        // every combination they then press.
        capture = {
          command: button.dataset.rebind!,
          session: openCapture(event as MouseEvent),
          verdict: undefined,
          toBase: false,
        };
        render();
      });
    });
    root.querySelectorAll<HTMLElement>("[data-reset-binding]").forEach((button) => {
      button.addEventListener("click", () => {
        const command = button.dataset.resetBinding!;
        request(
          "Resetting binding…",
          () => bridge.resetBinding(command),
          (receipt) =>
            receipt.combo === null
              ? `${bindingLabel(command)} is now unbound · ${receipt.file}`
              : `${bindingLabel(command)} reset to ${receipt.combo} · ${receipt.file}`,
          reloadHotkeys,
        );
      });
    });
    root.querySelector<HTMLInputElement>("[data-binding-redirect]")?.addEventListener("change", (event) => {
      if (capture === undefined) return;
      capture.toBase = (event.currentTarget as HTMLInputElement).checked;
      render();
    });
    root.querySelector<HTMLElement>("[data-capture-cancel]")?.addEventListener("click", () => {
      // Opening the dialog is not a commitment: cancelling leaves the
      // binding exactly as it was.
      capture = undefined;
      render();
    });
    root.querySelector<HTMLElement>("[data-capture-save]")?.addEventListener("click", () => {
      if (capture === undefined) return;
      const combo = capturedCombo(capture.session);
      if (combo === null) return;
      const command = capture.command;
      const toBase = capture.toBase;
      capture = undefined;
      request(
        "Saving binding…",
        () => bridge.setBinding(command, combo, toBase),
        (receipt) =>
          `${bindingLabel(command)} bound to ${receipt.combo ?? combo} · ${receipt.file}`,
        reloadHotkeys,
      );
    });

    restoreFocus(focused);
  };

  /**
   * Asks the agent about whatever the buffer now holds.
   *
   * Only a complete combination is worth asking about, and the answer is
   * discarded if the dialog moved on while it was in flight -- the user
   * presses faster than a round trip completes.
   */
  const probeCaptured = (): void => {
    if (capture === undefined) return;
    const combo = capturedCombo(capture.session);
    if (combo === null) return;
    const asked = capture.command;
    void bridge
      .probeHotkey(combo, asked)
      .then((verdict) => {
        if (capture === undefined || capture.command !== asked) return;
        if (capturedCombo(capture.session) !== combo) return;
        capture.verdict = verdict;
        render();
      })
      .catch((error: unknown) => {
        commandStatus = `Could not check that combination · ${String(error)}`;
        render();
      });
  };

  /**
   * The window-level key listeners the capture buffer needs.
   *
   * On `window` rather than on the dialog, because the dialog is markup
   * that is rebuilt on every render and a listener bound to it would not
   * survive the first keypress. The buffer's own `armed` flag is what
   * decides whether a key counts, not where the listener sits.
   */
  const onKeyDown = (event: KeyboardEvent): void => {
    if (capture === undefined) return;
    // Swallowed so a capture never also drives the page underneath -- an
    // arrow key would otherwise scroll the panel it was pressed over.
    event.preventDefault();
    capture.session = pressKey(capture.session, event);
    capture.verdict = undefined;
    render();
    probeCaptured();
  };
  const onKeyUp = (event: KeyboardEvent): void => {
    if (capture === undefined) return;
    capture.session = releaseKey(capture.session, event);
  };
  // A dialog that is not frontmost captures nothing, and throws away what
  // it held: a buffer left armed behind another window would record a
  // combination the user meant for something else.
  const onBlur = (): void => {
    if (capture === undefined) return;
    capture.session = blurCapture(capture.session);
    capture.verdict = undefined;
    render();
  };
  const onFocus = (): void => {
    if (capture === undefined) return;
    capture.session = focusCapture(capture.session);
    render();
  };
  window.addEventListener("keydown", onKeyDown);
  window.addEventListener("keyup", onKeyUp);
  window.addEventListener("blur", onBlur);
  window.addEventListener("focus", onFocus);

  // Suspension is held for this window's whole lifetime, not for one
  // dialog and not while it happens to have focus. Anything narrower
  // churns RegisterHotKey and risks losing a combination to another
  // application on each cycle. The cost -- no Mosaix hotkey works anywhere
  // while this window is open -- is what the hotkeys panel states rather
  // than leaving the user to infer.
  void bridge.startHotkeyCapture().catch((error: unknown) => {
    hotkeyError = `Could not suspend hotkeys for editing · ${String(error)}`;
    render();
  });

  render();

  // The saved-layout list is re-read on the same cadence as the bindings,
  // so a layout added by hand in the file shows up here. Only the *list*
  // is replaced: the draft on the canvas is the user's in-progress work,
  // and the echo of the editor's own write must not discard it.
  const stopLayoutWatch = watchSavedLayouts(bridge, {
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
  const stopHotkeyWatch = watchHotkeyBindings(bridge, {
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

  const stopWorkspaceWatch = watchWorkspaceStatus(bridge, {
    onChange: (status) => {
      workspaceStatus = status;
      workspaceError = undefined;
      render();
    },
    onError: (error) => {
      workspaceError = `Could not read workspace status · ${String(error)}`;
      render();
    },
  });

  return () => {
    // The agent would release suspension anyway when this connection
    // ends, so this is the clean-close path rather than the safety net.
    void bridge.endHotkeyCapture().catch(() => {});
    stopLayoutWatch();
    stopHotkeyWatch();
    stopWorkspaceWatch();
    window.removeEventListener("keydown", onKeyDown);
    window.removeEventListener("keyup", onKeyUp);
    window.removeEventListener("blur", onBlur);
    window.removeEventListener("focus", onFocus);
  };
}
