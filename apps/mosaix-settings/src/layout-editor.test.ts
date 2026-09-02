import { afterEach, describe, expect, it, vi } from "vitest";

import {
  bindingLabel,
  mountLayoutEditor,
  watchHotkeyBindings,
  type DesktopBridge,
  type EditorSnapshot,
  type HotkeyList,
} from "./layout-editor";

const snapshot: EditorSnapshot = {
  appearance: "light",
  display: {
    name: "Studio Display",
    resolution: "2560 × 1440",
    scalePercent: 100,
  },
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

function bridge(): DesktopBridge {
  return {
    loadEditorSnapshot: vi.fn().mockResolvedValue(structuredClone(snapshot)),
    loadHotkeyBindings: vi.fn().mockResolvedValue(structuredClone(hotkeys)),
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
