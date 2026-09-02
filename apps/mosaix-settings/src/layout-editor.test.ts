import { afterEach, describe, expect, it, vi } from "vitest";

import {
  bindingLabel,
  mountLayoutEditor,
  watchHotkeyBindings,
  type DesktopBridge,
  type EditorSnapshot,
  type HotkeyList,
  type SavedLayout,
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

const savedLayouts: SavedLayout[] = [
  {
    name: "writing",
    cells: [
      { id: 1, name: "Zone 1", x: 0, y: 0, width: 60, height: 100 },
      { id: 2, name: "Zone 2", x: 60, y: 0, width: 40, height: 100 },
    ],
  },
];

function bridge(): DesktopBridge {
  return {
    loadEditorSnapshot: vi.fn().mockResolvedValue(structuredClone(snapshot)),
    loadHotkeyBindings: vi.fn().mockResolvedValue(structuredClone(hotkeys)),
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
  it("renders the locked Layout Tab shell without prototype-only metadata", async () => {
    const root = document.createElement("div");
    document.body.append(root);

    await mountLayoutEditor(root, bridge());

    expect(root.querySelector("[data-product-subtitle]")?.textContent).toBe("Layout Tab");
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
    name.value = "Writing";
    name.dispatchEvent(new Event("change", { bubbles: true }));
    root.querySelector<HTMLElement>("[data-save-layout]")!.click();
    await vi.waitFor(() =>
      expect(root.querySelector(".command-status")?.textContent).toContain("desk.toml"),
    );

    expect(desktop.saveLayout).toHaveBeenCalledWith(
      expect.objectContaining({ name: "Writing", zones: expect.any(Array) }),
    );
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
    name.dispatchEvent(new Event("change", { bubbles: true }));
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
      .mockResolvedValue([
        ...structuredClone(savedLayouts),
        { name: "hand written", cells: [{ id: 1, name: "Zone 1", x: 0, y: 0, width: 100, height: 100 }] },
      ]);
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
    expect(root.querySelector(".work-label")?.textContent).toContain("1920 × 1040");
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
