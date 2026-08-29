import { describe, expect, it, vi } from "vitest";

import { createTauriDesktopBridge } from "./desktop-bridge";
import type { EditorSnapshot, LayoutDraft } from "./layout-editor";

const draft: LayoutDraft = {
  name: "Developer Focus",
  gap: 12,
  allowOverlap: true,
  zones: [{ id: 1, name: "Primary", x: 0, y: 0, width: 100, height: 100 }],
};

describe("Tauri desktop bridge", () => {
  it("maps the editor interface to typed Tauri commands", async () => {
    const wireDraft = {
      ...draft,
      zones: [{ id: 1, name: "Primary", x: 0, y: 0, width: 1, height: 1 }],
    };
    const snapshot = { appearance: "dark", draft: wireDraft } as EditorSnapshot;
    const invoke = vi.fn()
      .mockResolvedValueOnce(snapshot)
      .mockResolvedValueOnce({ revision: 4, status: "previewing" })
      .mockResolvedValueOnce({ revision: 5, status: "applied" })
      .mockResolvedValueOnce(undefined);
    const bridge = createTauriDesktopBridge(invoke);

    const loaded = await bridge.loadEditorSnapshot();
    await bridge.previewLayout(draft);
    await bridge.saveAndApplyLayout(draft);
    await bridge.setAppearance("light");

    expect(invoke.mock.calls).toEqual([
      ["load_editor_snapshot"],
      ["preview_layout", { draft: wireDraft }],
      ["save_and_apply_layout", { draft: wireDraft }],
      ["set_appearance", { appearance: "light" }],
    ]);
    expect(loaded.draft.zones[0]).toMatchObject({ width: 100, height: 100 });
  });
});
