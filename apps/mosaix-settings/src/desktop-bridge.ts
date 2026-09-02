import { invoke } from "@tauri-apps/api/core";

import type {
  Appearance,
  CommandReceipt,
  DesktopBridge,
  EditorSnapshot,
  HotkeyList,
  LayoutDraft,
  LayoutWriteReceipt,
  SavedLayout,
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

/**
 * A saved layout's cells arrive normalized, like an editor snapshot's --
 * the canvas works in percentages, so both cross the bridge the same way.
 */
function fromNormalizedLayout(layout: SavedLayout): SavedLayout {
  return {
    ...layout,
    cells: layout.cells.map((cell) => ({
      ...cell,
      x: cell.x * 100,
      y: cell.y * 100,
      width: cell.width * 100,
      height: cell.height * 100,
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
    loadSavedLayouts: async () => {
      const layouts = await invokeCommand("load_saved_layouts") as SavedLayout[];
      return layouts.map(fromNormalizedLayout);
    },
    saveLayout: (draft: LayoutDraft) =>
      invokeCommand("save_layout", { draft: toNormalizedDraft(draft) }) as Promise<LayoutWriteReceipt>,
    renameLayout: (from: string, to: string) =>
      invokeCommand("rename_layout", { from, to }) as Promise<LayoutWriteReceipt>,
    duplicateLayout: (from: string, to: string) =>
      invokeCommand("duplicate_layout", { from, to }) as Promise<LayoutWriteReceipt>,
    deleteLayout: (name: string) =>
      invokeCommand("delete_layout", { name }) as Promise<LayoutWriteReceipt>,
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
