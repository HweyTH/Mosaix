import { afterEach, describe, expect, it, vi } from "vitest";

import {
  mountLayoutEditor,
  type DesktopBridge,
  type EditorSnapshot,
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

function bridge(): DesktopBridge {
  return {
    loadEditorSnapshot: vi.fn().mockResolvedValue(structuredClone(snapshot)),
    previewLayout: vi.fn().mockResolvedValue({ revision: 1, status: "previewing" }),
    saveAndApplyLayout: vi.fn().mockResolvedValue({ revision: 2, status: "applied" }),
    setAppearance: vi.fn().mockResolvedValue(undefined),
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
});
