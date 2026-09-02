import { invoke } from "@tauri-apps/api/core";

import type {
  Appearance,
  CommandReceipt,
  DesktopBridge,
  EditorSnapshot,
  HotkeyList,
  LayoutDraft,
} from "./layout-editor";

export type InvokeCommand = (
  command: string,
  args?: Record<string, unknown>,
) => Promise<unknown>;

const invokeTauri: InvokeCommand = (command, args) => invoke(command, args);

function fromNormalizedDraft(draft: LayoutDraft): LayoutDraft {
  return {
    ...draft,
    zones: draft.zones.map((zone) => ({
      ...zone,
      x: zone.x * 100,
      y: zone.y * 100,
      width: zone.width * 100,
      height: zone.height * 100,
    })),
  };
}

function toNormalizedDraft(draft: LayoutDraft): LayoutDraft {
  return {
    ...draft,
    zones: draft.zones.map((zone) => ({
      ...zone,
      x: zone.x / 100,
      y: zone.y / 100,
      width: zone.width / 100,
      height: zone.height / 100,
    })),
  };
}

export function createTauriDesktopBridge(
  invokeCommand: InvokeCommand = invokeTauri,
): DesktopBridge {
  return {
    loadEditorSnapshot: async () => {
      const snapshot = await invokeCommand("load_editor_snapshot") as EditorSnapshot;
      return { ...snapshot, draft: fromNormalizedDraft(snapshot.draft) };
    },
    loadHotkeyBindings: () =>
      invokeCommand("load_hotkey_bindings") as Promise<HotkeyList>,
    previewLayout: (draft: LayoutDraft) =>
      invokeCommand("preview_layout", { draft: toNormalizedDraft(draft) }) as Promise<CommandReceipt>,
    saveAndApplyLayout: (draft: LayoutDraft) =>
      invokeCommand("save_and_apply_layout", { draft: toNormalizedDraft(draft) }) as Promise<CommandReceipt>,
    setAppearance: (appearance: Appearance) =>
      invokeCommand("set_appearance", { appearance }) as Promise<void>,
    loadAutomaticTilingSettings: () =>
      invokeCommand("load_automatic_tiling_settings") as ReturnType<DesktopBridge["loadAutomaticTilingSettings"]>,
    saveAutomaticTilingSettings: (settings) =>
      invokeCommand("save_automatic_tiling_settings", { settings }) as ReturnType<DesktopBridge["saveAutomaticTilingSettings"]>,
  };
}
