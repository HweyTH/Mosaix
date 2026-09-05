import type { WorkspaceStatus } from "./workspace-status";
// Static preview harness: mounts the real editor against fixture data so
// the rendered UI can be inspected without a Tauri backend. Not shipped.
import "./styles.css";

import {
  mountLayoutEditor,
  type DesktopBridge,
  type EditorSnapshot,
  type HotkeyList,
  type SavedLayoutList,
} from "./layout-editor";

const appearance = (new URLSearchParams(location.search).get("appearance") ??
  "dark") as "dark" | "light";

const snapshot: EditorSnapshot = {
  appearance,
  displays: [
    { name: "Primary display", resolution: "2560 × 1400", scalePercent: 100, workAreaWidth: 2560, workAreaHeight: 1400 },
    { name: "Display 2", resolution: "1920 × 1040", scalePercent: 125, workAreaWidth: 1920, workAreaHeight: 1040 },
  ],
  draft: {
    name: "Developer Focus",
    gap: 12,
    allowOverlap: true,
    zones: [
      { id: 1, name: "Primary", x: 0, y: 0, width: 62, height: 100 },
      { id: 2, name: "Reference", x: 62, y: 0, width: 38, height: 52 },
      { id: 3, name: "Console", x: 62, y: 52, width: 38, height: 48 },
    ],
  },
};

const tilingSettings = {
  topologyFingerprint: "DISPLAY-A@0,0 2560x1440 scale=1",
  matchedProfile: false,
  enabled: false,
  outerGap: 8,
  innerGap: 6,
  focusBorderEnabled: true,
  focusBorderColor: "#0078D7FF",
  focusBorderThickness: 2,
};

const hotkeys: HotkeyList = {
  topologyFingerprint: "DISPLAY-A@0,0 2560x1440 scale=1",
  captureSuspended: false,
  unregisteredCommands: [],
  baseFile: "config.toml",
  bindings: [
    { command: "snap-left", layout: null, combo: "ctrl+alt+left", source: "base", file: "config.toml" },
    { command: "apply-layout.writing", layout: "writing", combo: "ctrl+alt+1", source: "profile", file: "desk.toml" },
  ],
};

const savedLayouts: SavedLayoutList = {
  baseFile: "config.toml",
  layouts: [
    {
      name: "writing",
      source: "base",
      file: "config.toml",
      cells: [
        { id: 1, name: "Zone 1", x: 0, y: 0, width: 60, height: 100 },
        { id: 2, name: "Zone 2", x: 60, y: 0, width: 40, height: 100 },
      ],
    },
  ],
};

const workspaceStatus: WorkspaceStatus = {
  topologyFingerprint: "MON-A+MON-B",
  switchingStatus: "experimental",
  switchingReason: null,
  profileFile: "office.toml",
  mapping: [
    { display: "MON-A", workspace: "dev" },
    { display: "MON-B", workspace: "chat" },
  ],
  mappingComplete: true,
  displayCount: 2,
  parkingCapability: "verified",
  parkingCapabilityReason: null,
  workspaces: [
    { name: "dev", origin: "configuration", displayedOn: 1, memberCount: 3 },
    { name: "chat", origin: "command", displayedOn: null, memberCount: 0 },
  ],
  recoveryRequired: false,
  recoveryActions: [],
  parkedWindows: [],
}

const clone = <T,>(value: T): T => structuredClone(value);

const bridge: DesktopBridge = {
  loadEditorSnapshot: async () => clone(snapshot),
  loadHotkeyBindings: async () => clone(hotkeys),
  startHotkeyCapture: async () => undefined,
  endHotkeyCapture: async () => undefined,
  probeHotkey: async () => ({ availability: "available", command: null, warning: null, reason: null }),
  setBinding: async () => ({ file: "config.toml", combo: "ctrl+alt+j" }),
  resetBinding: async () => ({ file: "config.toml", combo: "ctrl+alt+left" }),
  loadSavedLayouts: async () => clone(savedLayouts),
  saveLayout: async () => ({ file: "config.toml" }),
  renameLayout: async () => ({ file: "config.toml" }),
  duplicateLayout: async () => ({ file: "config.toml" }),
  deleteLayout: async () => ({ file: "desk.toml" }),
  previewLayout: async () => ({ revision: 1, status: "previewing" }),
  saveAndApplyLayout: async () => ({ revision: 2, status: "applied" }),
  setAppearance: async () => undefined,
  loadAutomaticTilingSettings: async () => clone(tilingSettings),
  saveAutomaticTilingSettings: async (settings) => ({ ...settings, matchedProfile: true }),
  loadWorkspaceStatus: async () => clone(workspaceStatus),
  restoreParkedWindows: async () => ({ answer: { restored: [] } }),
  restoreWorkspaceSwitch: async () => ({ answer: { reconciled: [] } }),
};

const root = document.querySelector<HTMLElement>("#app")!;
void mountLayoutEditor(root, bridge);
