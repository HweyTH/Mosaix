import type { WorkspaceStatus } from "./workspace-status";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  bindingLabel,
  mountLayoutEditor,
  saveDestination,
  watchHotkeyBindings,
  type DesktopBridge,
  type EditorSnapshot,
  type HotkeyList,
  type SavedLayoutList,
} from "./layout-editor";

const snapshot: EditorSnapshot = {
  appearance: "light",
  displays: [
    {
      name: "Primary display",
      resolution: "2560 × 1400",
      scalePercent: 100,
      workAreaWidth: 2560,
      workAreaHeight: 1400,
    },
    {
      name: "Display 2",
      resolution: "1920 × 1040",
      scalePercent: 125,
      workAreaWidth: 1920,
      workAreaHeight: 1040,
    },
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
    {
      command: "snap-left",
      layout: null,
      combo: "ctrl+alt+left",
      source: "base",
      file: "config.toml",
    },
    {
      command: "apply-layout.writing",
      layout: "writing",
      combo: "ctrl+alt+1",
      source: "profile",
      file: "desk.toml",
    },
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

function bridge(): DesktopBridge {
  return {
    loadWorkspaceStatus: vi.fn().mockResolvedValue(structuredClone(workspaceStatus)),
    restoreParkedWindows: vi.fn().mockResolvedValue({ answer: { restored: [] } }),
    restoreWorkspaceSwitch: vi.fn().mockResolvedValue({ answer: { reconciled: [] } }),
    loadEditorSnapshot: vi.fn().mockResolvedValue(structuredClone(snapshot)),
    loadHotkeyBindings: vi.fn().mockResolvedValue(structuredClone(hotkeys)),
    startHotkeyCapture: vi.fn().mockResolvedValue(undefined),
    endHotkeyCapture: vi.fn().mockResolvedValue(undefined),
    probeHotkey: vi.fn().mockResolvedValue({
      availability: "available",
      command: null,
      warning: null,
      reason: null,
    }),
    setBinding: vi.fn().mockResolvedValue({ file: "config.toml", combo: "ctrl+alt+j" }),
    resetBinding: vi.fn().mockResolvedValue({ file: "config.toml", combo: "ctrl+alt+left" }),
    loadSavedLayouts: vi.fn().mockResolvedValue(structuredClone(savedLayouts)),
    saveLayout: vi.fn().mockResolvedValue({ file: "config.toml" }),
    renameLayout: vi.fn().mockResolvedValue({ file: "config.toml" }),
    duplicateLayout: vi.fn().mockResolvedValue({ file: "config.toml" }),
    deleteLayout: vi.fn().mockResolvedValue({ file: "desk.toml" }),
    previewLayout: vi.fn().mockResolvedValue({ revision: 1, status: "previewing" }),
    saveAndApplyLayout: vi.fn().mockResolvedValue({ revision: 2, status: "applied" }),
    setAppearance: vi.fn().mockResolvedValue(undefined),
    loadAutomaticTilingSettings: vi.fn().mockResolvedValue(structuredClone(tilingSettings)),
    saveAutomaticTilingSettings: vi.fn().mockImplementation(async (settings) => ({
      ...settings,
      matchedProfile: true,
    })),
  };
}

afterEach(() => {
  document.body.className = "";
  document.body.innerHTML = "";
});

describe("layout editor", () => {
  it("renders the locked editor shell without prototype-only metadata", async () => {
    const root = document.createElement("div");
    document.body.append(root);

    await mountLayoutEditor(root, bridge());

    expect(root.querySelector(".brand-mark")).not.toBeNull();
    expect(root.querySelectorAll("[data-zone]")).toHaveLength(3);
    expect(root.querySelector("[data-layout-metadata]")).toBeNull();
    expect(root.textContent).not.toContain("Prototype state");
  });

  it("edits the selected zone through the inspector", async () => {
    const root = document.createElement("div");
    document.body.append(root);
    await mountLayoutEditor(root, bridge());

    root.querySelector<HTMLElement>("[data-zone='2']")?.click();
    const nameInput = root.querySelector<HTMLInputElement>("[data-zone-name]");

    expect(nameInput?.value).toBe("Reference");
    nameInput!.value = "Documentation";
    nameInput!.dispatchEvent(new Event("change", { bubbles: true }));

    expect(root.querySelector("[data-zone='2'] strong")?.textContent).toBe("Documentation");
  });

  it("switches between the locked light and dark appearances", async () => {
    const root = document.createElement("div");
    const desktop = bridge();
    document.body.append(root);
    await mountLayoutEditor(root, desktop);

    root.querySelector<HTMLElement>("[data-appearance='dark']")?.click();
    await Promise.resolve();

    expect(document.body.className).toBe("night-tide");
    expect(desktop.setAppearance).toHaveBeenCalledWith("dark");
    expect(root.querySelector("[data-appearance='dark']")?.getAttribute("aria-pressed")).toBe("true");
  });

  it("keeps the previous appearance and reports a rejected desktop command", async () => {
    const root = document.createElement("div");
    const desktop = bridge();
    vi.mocked(desktop.setAppearance).mockRejectedValue(new Error("agent unavailable"));
    document.body.append(root);
    await mountLayoutEditor(root, desktop);

    root.querySelector<HTMLElement>("[data-appearance='dark']")?.click();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(document.body.className).toBe("warm-paper");
    expect(root.querySelector("[role='status']")?.textContent).toContain("Appearance failed");
  });

  it("sends the edited draft through preview and save commands", async () => {
    const root = document.createElement("div");
    const desktop = bridge();
    document.body.append(root);
    await mountLayoutEditor(root, desktop);

    const gap = root.querySelector<HTMLInputElement>("[data-gap]")!;
    gap.value = "20";
    gap.dispatchEvent(new Event("change", { bubbles: true }));
    root.querySelector<HTMLInputElement>("[data-allow-overlap]")!.click();
    root.querySelector<HTMLElement>("[data-add-zone]")!.click();
    root.querySelector<HTMLElement>("[data-preview]")!.click();
    root.querySelector<HTMLElement>("[data-save-apply]")!.click();
    await Promise.resolve();

    expect(desktop.previewLayout).toHaveBeenCalledWith(expect.objectContaining({ gap: 20, allowOverlap: false }));
    expect(desktop.saveAndApplyLayout).toHaveBeenCalledWith(expect.objectContaining({ gap: 20, allowOverlap: false }));
    expect(vi.mocked(desktop.saveAndApplyLayout).mock.calls[0]![0].zones).toHaveLength(4);
  });

  it("supports split, duplicate, delete, and undo as draft operations", async () => {
    const root = document.createElement("div");
    document.body.append(root);
    await mountLayoutEditor(root, bridge());

    root.querySelector<HTMLElement>("[data-zone='1']")!.click();
    root.querySelector<HTMLElement>("[data-command='split']")!.click();
    expect(root.querySelectorAll("[data-zone]")).toHaveLength(4);

    root.querySelector<HTMLElement>("[data-command='undo']")!.click();
    expect(root.querySelectorAll("[data-zone]")).toHaveLength(3);

    root.querySelector<HTMLElement>("[data-command='duplicate']")!.click();
    expect(root.querySelectorAll("[data-zone]")).toHaveLength(4);
    root.querySelector<HTMLElement>("[data-command='delete']")!.click();
    expect(root.querySelectorAll("[data-zone]")).toHaveLength(3);
  });

  it("edits and saves the matched topology automatic-tiling surface", async () => {
    const root = document.createElement("div");
    const desktop = bridge();
    document.body.append(root);
    await mountLayoutEditor(root, desktop);

    expect(root.querySelector("[data-topology-fingerprint]")?.textContent).toContain("DISPLAY-A");
    root.querySelector<HTMLInputElement>("[data-auto-tiling]")!.click();
    const outer = root.querySelector<HTMLInputElement>("[data-outer-gap]")!;
    outer.value = "14";
    outer.dispatchEvent(new Event("change", { bubbles: true }));
    const thickness = root.querySelector<HTMLInputElement>("[data-border-thickness]")!;
    thickness.value = "4";
    thickness.dispatchEvent(new Event("change", { bubbles: true }));
    root.querySelector<HTMLElement>("[data-save-tiling]")!.click();
    await Promise.resolve();

    expect(desktop.saveAutomaticTilingSettings).toHaveBeenCalledWith(expect.objectContaining({
      enabled: true,
      outerGap: 14,
      focusBorderThickness: 4,
    }));
  });

  it("undoes inspector edits and does not expose inactive canvas tools", async () => {
    const root = document.createElement("div");
    document.body.append(root);
    await mountLayoutEditor(root, bridge());

    expect(root.querySelector("[aria-label='Snap to grid']")).toBeNull();
    expect(root.querySelector("[aria-label='Allow overlap']")).toBeNull();

    const name = root.querySelector<HTMLInputElement>("[data-zone-name]")!;
    name.value = "Writing";
    name.dispatchEvent(new Event("change", { bubbles: true }));
    root.querySelector<HTMLElement>("[data-command='undo']")!.click();
    expect(root.querySelector<HTMLInputElement>("[data-zone-name]")!.value).toBe("Primary");

    root.querySelector<HTMLInputElement>("[data-allow-overlap]")!.click();
    expect(root.querySelector<HTMLInputElement>("[data-allow-overlap]")!.checked).toBe(false);
    root.querySelector<HTMLElement>("[data-command='undo']")!.click();
    expect(root.querySelector<HTMLInputElement>("[data-allow-overlap]")!.checked).toBe(true);
  });

  it("lists every binding with the configuration file that supplies it", async () => {
    const root = document.createElement("div");
    document.body.append(root);

    await mountLayoutEditor(root, bridge());
    await vi.waitFor(() => expect(root.querySelector(".binding")).not.toBeNull());

    const rows = root.querySelectorAll<HTMLElement>(".binding");
    expect(rows).toHaveLength(2);
    expect(rows[0]!.textContent).toContain("Snap left");
    expect(rows[0]!.textContent).toContain("ctrl+alt+left");
    expect(rows[0]!.textContent).toContain("config.toml");
    expect(rows[0]!.dataset.source).toBe("base");

    // The layout binding is in the same list, not a separate one.
    expect(rows[1]!.textContent).toContain("Apply layout · writing");
    expect(rows[1]!.textContent).toContain("desk.toml");
    expect(rows[1]!.dataset.source).toBe("profile");
  });

  it("tells the user when the bindings could not be read at all", async () => {
    const root = document.createElement("div");
    document.body.append(root);
    const desktop = bridge();
    desktop.loadHotkeyBindings = vi
      .fn()
      .mockRejectedValue(new Error("the Mosaix agent is not running"));

    await mountLayoutEditor(root, desktop);
    await vi.waitFor(() =>
      expect(root.querySelector("[data-hotkey-error]")).not.toBeNull(),
    );

    expect(root.querySelector("[data-hotkey-error]")?.textContent).toContain(
      "the Mosaix agent is not running",
    );
  });
});

describe("saved layouts", () => {
  it("saves the drawn layout under the name in the panel and reports the file", async () => {
    const root = document.createElement("div");
    document.body.append(root);
    const desktop = bridge();
    desktop.saveLayout = vi.fn().mockResolvedValue({ file: "desk.toml" });
    await mountLayoutEditor(root, desktop);

    const name = root.querySelector<HTMLInputElement>("[data-layout-name]")!;
    name.value = "Drafting";
    name.dispatchEvent(new Event("input", { bubbles: true }));
    root.querySelector<HTMLElement>("[data-save-layout]")!.click();
    await vi.waitFor(() =>
      expect(root.querySelector(".command-status")?.textContent).toContain("desk.toml"),
    );

    expect(desktop.saveLayout).toHaveBeenCalledWith(
      expect.objectContaining({ name: "Drafting", zones: expect.any(Array) }),
      false,
    );
  });

  it("reports a name already taken before saving over the layout that holds it", async () => {
    // Saving replaces the cells under that name. That is right for a
    // layout the user opened and wrong for a drawing they just named, so
    // the second case has to be stopped before the write.
    const root = document.createElement("div");
    document.body.append(root);
    const desktop = bridge();
    await mountLayoutEditor(root, desktop);
    await vi.waitFor(() => expect(root.querySelector(".library-item")).not.toBeNull());

    const name = root.querySelector<HTMLInputElement>("[data-layout-name]")!;
    name.value = "Writing";
    name.dispatchEvent(new Event("input", { bubbles: true }));
    root.querySelector<HTMLElement>("[data-save-layout]")!.click();

    expect(root.querySelector(".command-status")?.textContent).toContain("already exists");
    expect(desktop.saveLayout).not.toHaveBeenCalled();
  });

  it("saves over the layout it has open without complaining about its own name", async () => {
    const root = document.createElement("div");
    document.body.append(root);
    const desktop = bridge();
    await mountLayoutEditor(root, desktop);
    await vi.waitFor(() => expect(root.querySelector("[data-open-layout]")).not.toBeNull());

    root.querySelector<HTMLElement>("[data-open-layout]")!.click();
    root.querySelector<HTMLElement>("[data-save-layout]")!.click();

    await vi.waitFor(() => expect(desktop.saveLayout).toHaveBeenCalled());
    expect(desktop.saveLayout).toHaveBeenCalledWith(
      expect.objectContaining({ name: "writing" }),
      false,
    );
  });

  it("keeps a half-typed layout name through a background refresh", async () => {
    vi.useFakeTimers();
    const root = document.createElement("div");
    document.body.append(root);
    const desktop = bridge();
    desktop.loadSavedLayouts = vi
      .fn()
      .mockResolvedValueOnce(structuredClone(savedLayouts))
      .mockResolvedValue({
        ...structuredClone(savedLayouts),
        layouts: [
          ...structuredClone(savedLayouts.layouts),
          {
            name: "hand written",
            source: "base",
            file: "config.toml",
            cells: [{ id: 1, name: "Zone 1", x: 0, y: 0, width: 100, height: 100 }],
          },
        ],
      });
    await mountLayoutEditor(root, desktop);

    const name = root.querySelector<HTMLInputElement>("[data-layout-name]")!;
    name.focus();
    name.value = "half typ";
    name.dispatchEvent(new Event("input", { bubbles: true }));
    await vi.advanceTimersByTimeAsync(2500);
    vi.useRealTimers();

    const refreshed = root.querySelector<HTMLInputElement>("[data-layout-name]")!;
    expect(refreshed.value).toBe("half typ");
    expect(document.activeElement).toBe(refreshed);
  });

  it("reports a rejected save as a failure carrying the agent's reason", async () => {
    const root = document.createElement("div");
    document.body.append(root);
    const desktop = bridge();
    desktop.saveLayout = vi
      .fn()
      .mockRejectedValue(new Error("config.toml: saved layout \"writing\" declares no cells"));
    await mountLayoutEditor(root, desktop);

    root.querySelector<HTMLElement>("[data-save-layout]")!.click();
    await vi.waitFor(() =>
      expect(root.querySelector(".command-status")?.textContent).toContain("failed"),
    );

    expect(root.querySelector(".command-status")?.textContent).toContain("declares no cells");
  });

  it("lists saved layouts and duplicates or deletes the one asked for", async () => {
    const root = document.createElement("div");
    document.body.append(root);
    const desktop = bridge();
    await mountLayoutEditor(root, desktop);
    await vi.waitFor(() => expect(root.querySelector(".library-item")).not.toBeNull());

    expect(root.querySelector(".library-item")?.textContent).toContain("writing");
    expect(root.querySelector(".library-item")?.textContent).toContain("2 zones");

    root.querySelector<HTMLElement>("[data-duplicate-layout]")!.click();
    await vi.waitFor(() => expect(desktop.duplicateLayout).toHaveBeenCalled());
    expect(desktop.duplicateLayout).toHaveBeenCalledWith("writing", "writing copy");

    root.querySelector<HTMLElement>("[data-delete-layout]")!.click();
    await vi.waitFor(() => expect(desktop.deleteLayout).toHaveBeenCalledWith("writing"));
  });

  it("renames the layout it is editing to the name in the panel", async () => {
    const root = document.createElement("div");
    document.body.append(root);
    const desktop = bridge();
    await mountLayoutEditor(root, desktop);
    await vi.waitFor(() => expect(root.querySelector("[data-open-layout]")).not.toBeNull());

    root.querySelector<HTMLElement>("[data-open-layout]")!.click();
    const name = root.querySelector<HTMLInputElement>("[data-layout-name]")!;
    name.value = "drafting";
    name.dispatchEvent(new Event("input", { bubbles: true }));
    root.querySelector<HTMLElement>("[data-rename-layout]")!.click();

    await vi.waitFor(() =>
      expect(desktop.renameLayout).toHaveBeenCalledWith("writing", "drafting"),
    );
  });

  it("opening a saved layout loads its cells onto the canvas", async () => {
    const root = document.createElement("div");
    document.body.append(root);
    await mountLayoutEditor(root, bridge());
    await vi.waitFor(() => expect(root.querySelector("[data-open-layout]")).not.toBeNull());

    root.querySelector<HTMLElement>("[data-open-layout]")!.click();

    expect(root.querySelectorAll(".zone")).toHaveLength(2);
    expect(root.querySelector<HTMLInputElement>("[data-layout-name]")!.value).toBe("writing");
  });

  it("does not discard in-progress edits when the write echoes back", async () => {
    // The editor's own save returns through the same list re-read that a
    // hand edit does. That echo has to leave the canvas alone.
    const root = document.createElement("div");
    document.body.append(root);
    const desktop = bridge();
    await mountLayoutEditor(root, desktop);
    await vi.waitFor(() => expect(root.querySelector(".library-item")).not.toBeNull());

    const zoneName = root.querySelector<HTMLInputElement>("[data-zone-name]")!;
    zoneName.value = "Editor";
    zoneName.dispatchEvent(new Event("change", { bubbles: true }));
    root.querySelector<HTMLElement>("[data-save-layout]")!.click();
    await vi.waitFor(() =>
      expect(root.querySelector(".command-status")?.textContent).toContain("Saved to"),
    );

    expect(root.querySelector<HTMLInputElement>("[data-zone-name]")!.value).toBe("Editor");
    expect(root.querySelectorAll(".zone")).toHaveLength(3);
  });

  it("shows a layout added by hand in the configuration file", async () => {
    vi.useFakeTimers();
    const root = document.createElement("div");
    document.body.append(root);
    const desktop = bridge();
    desktop.loadSavedLayouts = vi
      .fn()
      .mockResolvedValueOnce(structuredClone(savedLayouts))
      .mockResolvedValue({
        ...structuredClone(savedLayouts),
        layouts: [
          ...structuredClone(savedLayouts.layouts),
          {
            name: "hand written",
            source: "base",
            file: "config.toml",
            cells: [{ id: 1, name: "Zone 1", x: 0, y: 0, width: 100, height: 100 }],
          },
        ],
      });
    await mountLayoutEditor(root, desktop);

    await vi.advanceTimersByTimeAsync(2500);
    vi.useRealTimers();

    expect(root.textContent).toContain("hand written");
  });
});

describe("display selection", () => {
  it("draws the canvas at the selected display's work-area proportions", async () => {
    const root = document.createElement("div");
    document.body.append(root);
    await mountLayoutEditor(root, bridge());

    const monitor = () => root.querySelector<HTMLElement>("[data-monitor]")!;
    expect(monitor().style.aspectRatio.replace(/\s/g, "")).toBe("2560/1400");

    const picker = root.querySelector<HTMLSelectElement>("[data-display]")!;
    picker.value = "1";
    picker.dispatchEvent(new Event("change", { bubbles: true }));

    expect(monitor().style.aspectRatio.replace(/\s/g, "")).toBe("1920/1040");
    expect(root.querySelector(".display-meta")?.textContent).toContain("1920 × 1040");
  });
});

describe("binding labels", () => {
  it("reads a label off the command's own TOML path", () => {
    expect(bindingLabel("snap-left")).toBe("Snap left");
    expect(bindingLabel("toggle-automatic-tiling")).toBe("Toggle automatic tiling");
    expect(bindingLabel("apply-layout.writing")).toBe("Apply layout · writing");
    expect(bindingLabel("apply-layout.deep.work")).toBe("Apply layout · deep.work");
  });
});

describe("hotkey binding watch", () => {
  it("reports a new matched profile's bindings when the topology changes", async () => {
    vi.useFakeTimers();
    const docked: HotkeyList = {
      topologyFingerprint: "DISPLAY-A|DISPLAY-B",
      captureSuspended: false,
      unregisteredCommands: [],
      baseFile: "config.toml",
      bindings: [
        {
          command: "snap-left",
          layout: null,
          combo: "ctrl+shift+left",
          source: "profile",
          file: "desk.toml",
        },
      ],
    };
    const loadHotkeyBindings = vi
      .fn()
      .mockResolvedValueOnce(structuredClone(hotkeys))
      .mockResolvedValueOnce(structuredClone(hotkeys))
      .mockResolvedValue(structuredClone(docked));
    const onChange = vi.fn();
    const stop = watchHotkeyBindings({ loadHotkeyBindings }, { onChange, onError: vi.fn() }, 1000);

    await vi.advanceTimersByTimeAsync(2500);
    stop();
    vi.useRealTimers();

    // Three reads, two distinct answers: the unchanged second read is not
    // re-reported, and the docked one is.
    expect(loadHotkeyBindings).toHaveBeenCalledTimes(3);
    expect(onChange).toHaveBeenCalledTimes(2);
    expect(onChange.mock.calls[1]![0]).toEqual(docked);
  });

  it("reports a read failure once rather than on every tick", async () => {
    vi.useFakeTimers();
    const loadHotkeyBindings = vi.fn().mockRejectedValue(new Error("agent is not running"));
    const onError = vi.fn();
    const stop = watchHotkeyBindings(
      { loadHotkeyBindings },
      { onChange: vi.fn(), onError },
      1000,
    );

    await vi.advanceTimersByTimeAsync(3500);
    stop();
    vi.useRealTimers();

    expect(loadHotkeyBindings.mock.calls.length).toBeGreaterThan(1);
    expect(onError).toHaveBeenCalledTimes(1);
  });

  it("stops reading once its stop function is called", async () => {
    vi.useFakeTimers();
    const loadHotkeyBindings = vi.fn().mockResolvedValue(structuredClone(hotkeys));
    const stop = watchHotkeyBindings(
      { loadHotkeyBindings },
      { onChange: vi.fn(), onError: vi.fn() },
      1000,
    );

    await vi.advanceTimersByTimeAsync(1500);
    const readsBeforeStop = loadHotkeyBindings.mock.calls.length;
    stop();
    await vi.advanceTimersByTimeAsync(5000);
    vi.useRealTimers();

    expect(loadHotkeyBindings.mock.calls.length).toBe(readsBeforeStop);
  });
});

describe("hotkey registration notices", () => {
  it("states that hotkeys are off while capture holds them, and names the ones that did not come back", async () => {
    const desktop = bridge();
    desktop.loadHotkeyBindings = vi.fn().mockResolvedValue({
      ...structuredClone(hotkeys),
      captureSuspended: true,
      unregisteredCommands: ["snap-right"],
    });
    const root = document.createElement("div");

    const stop = await mountLayoutEditor(root, desktop);
    await vi.waitFor(() =>
      expect(root.querySelector("[data-capture-suspended]")).not.toBeNull(),
    );

    expect(root.querySelector("[data-capture-suspended]")?.textContent).toContain(
      "Hotkeys are off",
    );
    expect(root.querySelector("[data-unregistered-bindings]")?.textContent).toContain(
      "Snap right",
    );
    stop();
  });

  it("shows no notice when every binding registered and nothing is suspended", async () => {
    const root = document.createElement("div");

    const stop = await mountLayoutEditor(root, bridge());
    await vi.waitFor(() => expect(root.querySelector(".binding-list")).not.toBeNull());

    expect(root.querySelector("[data-capture-suspended]")).toBeNull();
    expect(root.querySelector("[data-unregistered-bindings]")).toBeNull();
    stop();
  });
});

describe("layout write destination", () => {
  const layered: SavedLayoutList = {
    baseFile: "config.toml",
    layouts: [
      {
        name: "writing",
        source: "base",
        file: "config.toml",
        cells: [{ id: 1, name: "Zone 1", x: 0, y: 0, width: 100, height: 100 }],
      },
      {
        name: "docked",
        source: "profile",
        file: "desk.toml",
        cells: [{ id: 1, name: "Zone 1", x: 0, y: 0, width: 50, height: 100 }],
      },
    ],
  };

  it("names base config for a layout that does not exist yet", () => {
    expect(saveDestination("brand new", layered, false)).toEqual({
      file: "config.toml",
      redirectable: false,
    });
  });

  it("names the profile for a layout the matched profile supplies", () => {
    expect(saveDestination("docked", layered, false)).toEqual({
      file: "desk.toml",
      redirectable: true,
    });
  });

  it("follows the redirect once it is set", () => {
    expect(saveDestination("docked", layered, true)).toEqual({
      file: "config.toml",
      redirectable: true,
    });
  });

  it("offers no redirect for a layout base config already supplies", () => {
    expect(saveDestination("writing", layered, false)).toEqual({
      file: "config.toml",
      redirectable: false,
    });
  });

  it("matches a name the way configuration does, ignoring case", () => {
    expect(saveDestination("  DoCkEd ", layered, false).file).toBe("desk.toml");
  });

  it("shows the destination before the save and sends the redirect with it", async () => {
    const desktop = bridge();
    desktop.loadSavedLayouts = vi.fn().mockResolvedValue(structuredClone(layered));
    const root = document.createElement("div");
    document.body.append(root);

    await mountLayoutEditor(root, desktop);
    await vi.waitFor(() =>
      expect(root.querySelector('[data-open-layout="docked"]')).not.toBeNull(),
    );
    expect(
      root.querySelector("[data-layout-redirect]"),
      "the untouched draft is a new layout, which goes to base config anyway",
    ).toBeNull();

    // Opened rather than typed: saving replaces the cells of a layout the
    // user has open, and a name typed over a layout they never opened is
    // refused by the clash guard before any of this matters.
    root.querySelector<HTMLElement>('[data-open-layout="docked"]')!.click();

    expect(root.querySelector("[data-layout-destination]")?.textContent).toContain("desk.toml");
    const redirect = root.querySelector<HTMLInputElement>("[data-layout-redirect]")!;
    redirect.checked = true;
    redirect.dispatchEvent(new Event("change", { bubbles: true }));
    expect(root.querySelector("[data-layout-destination]")?.textContent).toContain("config.toml");

    root.querySelector<HTMLElement>("[data-save-layout]")!.click();
    await vi.waitFor(() => expect(desktop.saveLayout).toHaveBeenCalled());
    expect(desktop.saveLayout).toHaveBeenCalledWith(
      expect.objectContaining({ name: "docked" }),
      true,
    );
  });

  it("distinguishes a profile-supplied layout from a base-supplied one in the list", async () => {
    const desktop = bridge();
    desktop.loadSavedLayouts = vi.fn().mockResolvedValue(structuredClone(layered));
    const root = document.createElement("div");

    const stop = await mountLayoutEditor(root, desktop);
    await vi.waitFor(() =>
      expect(root.querySelector('[data-layout="docked"]')).not.toBeNull(),
    );

    expect(root.querySelector('[data-layout="docked"]')?.getAttribute("data-source")).toBe(
      "profile",
    );
    expect(root.querySelector('[data-layout="docked"]')?.textContent).toContain("desk.toml");
    expect(root.querySelector('[data-layout="writing"]')?.getAttribute("data-source")).toBe(
      "base",
    );
    stop();
  });
});

describe("hotkey capture dialog", () => {
  /**
   * Mounts the editor with the hotkey list loaded, then opens the rebind
   * dialog for `command`.
   */
  async function openDialog(
    desktop: DesktopBridge,
    command = "snap-left",
  ): Promise<{ root: HTMLElement; stop: () => void }> {
    const root = document.createElement("div");
    document.body.append(root);
    const stop = await mountLayoutEditor(root, desktop);
    await vi.waitFor(() =>
      expect(root.querySelector(`[data-rebind="${command}"]`)).not.toBeNull(),
    );
    root.querySelector<HTMLElement>(`[data-rebind="${command}"]`)!.click();
    return { root, stop };
  }

  function pressCombination(
    root: HTMLElement,
    code: string,
    modifiers: Partial<KeyboardEventInit> = {},
  ): void {
    window.dispatchEvent(
      new KeyboardEvent("keydown", { code, bubbles: true, ...modifiers }),
    );
    void root;
  }

  it("opens a capture dialog that says it is listening", async () => {
    const { root, stop } = await openDialog(bridge());

    expect(root.querySelector("[data-capture-dialog]")).not.toBeNull();
    expect(root.querySelector("[data-captured-combo]")?.textContent).toContain(
      "Press a combination",
    );
    stop();
  });

  it("keeps save disabled until the combination is complete", async () => {
    const { root, stop } = await openDialog(bridge());
    const save = (): HTMLButtonElement =>
      root.querySelector<HTMLButtonElement>("[data-capture-save]")!;

    expect(save().disabled).toBe(true);

    // A key Mosaix has no name for stays incomplete.
    pressCombination(root, "Pause", { ctrlKey: true });
    expect(save().disabled).toBe(true);
    expect(root.querySelector("[data-captured-combo]")?.textContent).toContain(
      "Unsupported key",
    );

    pressCombination(root, "KeyJ", { ctrlKey: true, altKey: true });
    expect(save().disabled).toBe(false);
    expect(root.querySelector("[data-captured-combo]")?.textContent).toContain("ctrl+alt+j");
    stop();
  });

  it("asks the agent about the combination and shows the verdict", async () => {
    const desktop = bridge();
    desktop.probeHotkey = vi.fn().mockResolvedValue({
      availability: "mosaix_binding",
      command: "focus-down",
      warning: null,
      reason: null,
    });
    const { root, stop } = await openDialog(desktop);

    pressCombination(root, "KeyJ", { ctrlKey: true, altKey: true });

    await vi.waitFor(() =>
      expect(root.querySelector("[data-capture-verdict]")).not.toBeNull(),
    );
    expect(
      desktop.probeHotkey,
      "the command travels with the question so a binding cannot conflict with itself",
    ).toHaveBeenCalledWith("ctrl+alt+j", "snap-left");
    expect(root.querySelector("[data-capture-verdict]")?.textContent).toContain("Focus down");
    // Blocked, because whole-directory validation rejects two commands on
    // one combination -- so offering the save would offer a write that
    // can only be refused.
    expect(root.querySelector<HTMLButtonElement>("[data-capture-save]")!.disabled).toBe(true);
    stop();
  });

  it("blocks a combination Windows handles itself", async () => {
    const desktop = bridge();
    desktop.probeHotkey = vi.fn().mockResolvedValue({
      availability: "reserved",
      command: null,
      warning: null,
      reason: null,
    });
    const { root, stop } = await openDialog(desktop);

    pressCombination(root, "KeyL", { metaKey: true });

    await vi.waitFor(() =>
      expect(root.querySelector("[data-capture-verdict]")).not.toBeNull(),
    );
    expect(root.querySelector<HTMLButtonElement>("[data-capture-save]")!.disabled).toBe(true);
    stop();
  });

  it("warns about the debugger-reserved function key without blocking it", async () => {
    const desktop = bridge();
    desktop.probeHotkey = vi.fn().mockResolvedValue({
      availability: "available",
      command: null,
      warning: "Windows reserves F12 for the debugger, so this binding may not fire",
      reason: null,
    });
    const { root, stop } = await openDialog(desktop);

    pressCombination(root, "F12", { ctrlKey: true, altKey: true });

    await vi.waitFor(() =>
      expect(root.querySelector("[data-capture-warning]")).not.toBeNull(),
    );
    expect(root.querySelector<HTMLButtonElement>("[data-capture-save]")!.disabled).toBe(false);
    stop();
  });

  it("clears the buffer when the window loses foreground", async () => {
    const { root, stop } = await openDialog(bridge());
    pressCombination(root, "KeyJ", { ctrlKey: true, altKey: true });
    expect(root.querySelector<HTMLButtonElement>("[data-capture-save]")!.disabled).toBe(false);

    window.dispatchEvent(new Event("blur"));

    expect(root.querySelector<HTMLButtonElement>("[data-capture-save]")!.disabled).toBe(true);
    expect(root.querySelector("[data-captured-combo]")?.textContent).toContain(
      "Click to listen",
    );
    stop();
  });

  it("captures nothing while the window is not frontmost", async () => {
    const { root, stop } = await openDialog(bridge());
    window.dispatchEvent(new Event("blur"));

    pressCombination(root, "KeyJ", { ctrlKey: true, altKey: true });

    expect(root.querySelector<HTMLButtonElement>("[data-capture-save]")!.disabled).toBe(true);
    stop();
  });

  it("keeps the previous binding when the dialog is cancelled", async () => {
    const desktop = bridge();
    const { root, stop } = await openDialog(desktop);
    pressCombination(root, "KeyJ", { ctrlKey: true, altKey: true });

    root.querySelector<HTMLElement>("[data-capture-cancel]")!.click();

    expect(root.querySelector("[data-capture-dialog]")).toBeNull();
    expect(desktop.setBinding).not.toHaveBeenCalled();
    stop();
  });

  it("saves the captured combination and reports the file the agent wrote", async () => {
    const desktop = bridge();
    const { root, stop } = await openDialog(desktop);
    pressCombination(root, "KeyJ", { ctrlKey: true, altKey: true });

    root.querySelector<HTMLElement>("[data-capture-save]")!.click();

    await vi.waitFor(() => expect(desktop.setBinding).toHaveBeenCalled());
    expect(desktop.setBinding).toHaveBeenCalledWith("snap-left", "ctrl+alt+j", false);
    await vi.waitFor(() =>
      expect(root.querySelector(".command-status")?.textContent).toContain("config.toml"),
    );
    stop();
  });

  it("reports the agent's own reason when it refuses the write", async () => {
    const desktop = bridge();
    desktop.setBinding = vi
      .fn()
      .mockRejectedValue(new Error("config.toml: ctrl+alt+j is already bound to focus-down"));
    const { root, stop } = await openDialog(desktop);
    pressCombination(root, "KeyJ", { ctrlKey: true, altKey: true });

    root.querySelector<HTMLElement>("[data-capture-save]")!.click();

    await vi.waitFor(() =>
      expect(root.querySelector(".command-status")?.textContent).toContain("already bound"),
    );
    stop();
  });

  it("shows the destination before saving and offers a redirect for a profile binding", async () => {
    const desktop = bridge();
    const { root, stop } = await openDialog(desktop, "apply-layout.writing");

    expect(root.querySelector("[data-binding-destination]")?.textContent).toContain("desk.toml");
    const redirect = root.querySelector<HTMLInputElement>("[data-binding-redirect]")!;
    redirect.checked = true;
    redirect.dispatchEvent(new Event("change", { bubbles: true }));
    expect(root.querySelector("[data-binding-destination]")?.textContent).toContain("config.toml");

    pressCombination(root, "KeyJ", { ctrlKey: true, altKey: true });
    root.querySelector<HTMLElement>("[data-capture-save]")!.click();

    await vi.waitFor(() => expect(desktop.setBinding).toHaveBeenCalled());
    expect(desktop.setBinding).toHaveBeenCalledWith(
      "apply-layout.writing",
      "ctrl+alt+j",
      true,
    );
    stop();
  });

  it("offers no redirect for a binding base config already supplies", async () => {
    const { root, stop } = await openDialog(bridge(), "snap-left");

    expect(root.querySelector("[data-binding-destination]")?.textContent).toContain(
      "config.toml",
    );
    expect(root.querySelector("[data-binding-redirect]")).toBeNull();
    stop();
  });

  it("resets a binding to its default", async () => {
    const desktop = bridge();
    const root = document.createElement("div");
    document.body.append(root);
    const stop = await mountLayoutEditor(root, desktop);
    await vi.waitFor(() =>
      expect(root.querySelector('[data-reset-binding="snap-left"]')).not.toBeNull(),
    );

    root.querySelector<HTMLElement>('[data-reset-binding="snap-left"]')!.click();

    await vi.waitFor(() => expect(desktop.resetBinding).toHaveBeenCalledWith("snap-left"));
    await vi.waitFor(() =>
      expect(root.querySelector(".command-status")?.textContent).toContain("ctrl+alt+left"),
    );
    stop();
  });
});

describe("capture suspension lifetime", () => {
  it("holds suspension for the window's lifetime rather than for one dialog", async () => {
    const desktop = bridge();
    const root = document.createElement("div");

    const stop = await mountLayoutEditor(root, desktop);

    await vi.waitFor(() => expect(desktop.startHotkeyCapture).toHaveBeenCalledTimes(1));
    expect(desktop.endHotkeyCapture).not.toHaveBeenCalled();

    stop();
    expect(desktop.endHotkeyCapture).toHaveBeenCalledTimes(1);
  });
});

describe("unbound commands", () => {
  const withUnbound: HotkeyList = {
    ...structuredClone(hotkeys),
    bindings: [
      ...structuredClone(hotkeys).bindings,
      {
        command: "apply-layout.reading",
        layout: "reading",
        combo: null,
        source: "unbound",
        file: null,
      },
    ],
  };

  async function mounted(): Promise<{ root: HTMLElement; desktop: DesktopBridge; stop: () => void }> {
    const desktop = bridge();
    desktop.loadHotkeyBindings = vi.fn().mockResolvedValue(structuredClone(withUnbound));
    const root = document.createElement("div");
    document.body.append(root);
    const stop = await mountLayoutEditor(root, desktop);
    await vi.waitFor(() =>
      expect(root.querySelector('[data-binding="apply-layout.reading"]')).not.toBeNull(),
    );
    return { root, desktop, stop };
  }

  it("lists a saved layout nothing is bound to, so it can be given a combination", async () => {
    const { root, stop } = await mounted();

    const row = root.querySelector('[data-binding="apply-layout.reading"]')!;
    expect(row.querySelector("kbd")?.textContent).toBe("not bound");
    expect(
      row.querySelector<HTMLButtonElement>("[data-reset-binding]")!.disabled,
      "there is nothing to reset a binding to when nothing binds it",
    ).toBe(true);
    stop();
  });

  it("binds an unbound command through the same capture dialog", async () => {
    const { root, desktop, stop } = await mounted();

    root.querySelector<HTMLElement>('[data-rebind="apply-layout.reading"]')!.click();
    expect(root.querySelector("[data-binding-destination]")?.textContent).toContain(
      "config.toml",
    );
    window.dispatchEvent(
      new KeyboardEvent("keydown", { code: "Digit2", ctrlKey: true, altKey: true }),
    );
    root.querySelector<HTMLElement>("[data-capture-save]")!.click();

    await vi.waitFor(() => expect(desktop.setBinding).toHaveBeenCalled());
    expect(desktop.setBinding).toHaveBeenCalledWith("apply-layout.reading", "ctrl+alt+2", false);
    stop();
  });
});

describe("capture verdicts that block or warn", () => {
  async function verdict(availability: string, extra: Record<string, unknown> = {}) {
    const desktop = bridge();
    desktop.probeHotkey = vi.fn().mockResolvedValue({
      availability,
      command: null,
      warning: null,
      reason: null,
      ...extra,
    });
    const root = document.createElement("div");
    document.body.append(root);
    const stop = await mountLayoutEditor(root, desktop);
    await vi.waitFor(() => expect(root.querySelector('[data-rebind="snap-left"]')).not.toBeNull());
    root.querySelector<HTMLElement>('[data-rebind="snap-left"]')!.click();
    window.dispatchEvent(
      new KeyboardEvent("keydown", { code: "KeyJ", ctrlKey: true, altKey: true }),
    );
    await vi.waitFor(() => expect(root.querySelector("[data-capture-verdict]")).not.toBeNull());
    return { root, stop };
  }

  it("still lets a combination another application owns be saved deliberately", async () => {
    const { root, stop } = await verdict("system_or_other_application");

    expect(
      root.querySelector<HTMLButtonElement>("[data-capture-save]")!.disabled,
      "Mosaix cannot arbitrate another application's claim, so it does not overrule the user",
    ).toBe(false);
    stop();
  });

  it("blocks a key Mosaix cannot express", async () => {
    const { root, stop } = await verdict("unsupported", {
      reason: 'Mosaix has no key named "BREAK"',
    });

    expect(root.querySelector<HTMLButtonElement>("[data-capture-save]")!.disabled).toBe(true);
    stop();
  });

  it("warns about a bare key without refusing it", async () => {
    const desktop = bridge();
    const root = document.createElement("div");
    document.body.append(root);
    const stop = await mountLayoutEditor(root, desktop);
    await vi.waitFor(() => expect(root.querySelector('[data-rebind="snap-left"]')).not.toBeNull());
    root.querySelector<HTMLElement>('[data-rebind="snap-left"]')!.click();

    window.dispatchEvent(new KeyboardEvent("keydown", { code: "F13", bubbles: true }));

    expect(root.querySelector("[data-bare-key]")?.textContent).toContain("every app");
    expect(
      root.querySelector<HTMLButtonElement>("[data-capture-save]")!.disabled,
      "RegisterHotKey takes a bare key; the user is entitled to it on their own machine",
    ).toBe(false);
    stop();
  });
});
