use std::sync::Mutex;

use tauri::State;

use crate::editor::{Appearance, CommandReceipt, EditorSession, EditorSnapshot, LayoutDraft};

#[derive(Default)]
pub struct EditorState(Mutex<EditorSession>);

#[tauri::command]
pub fn load_editor_snapshot(state: State<'_, EditorState>) -> Result<EditorSnapshot, String> {
    let session = state.0.lock().map_err(|_| "editor state is unavailable")?;
    Ok(session.load())
}

#[tauri::command]
pub fn preview_layout(
    draft: LayoutDraft,
    state: State<'_, EditorState>,
) -> Result<CommandReceipt, String> {
    let mut session = state.0.lock().map_err(|_| "editor state is unavailable")?;
    session.preview(draft).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn save_and_apply_layout(
    draft: LayoutDraft,
    state: State<'_, EditorState>,
) -> Result<CommandReceipt, String> {
    let mut session = state.0.lock().map_err(|_| "editor state is unavailable")?;
    session.apply(draft).map_err(|error| error.to_string())
}

#[tauri::command]
pub fn set_appearance(appearance: Appearance, state: State<'_, EditorState>) -> Result<(), String> {
    let mut session = state.0.lock().map_err(|_| "editor state is unavailable")?;
    session.set_appearance(appearance);
    Ok(())
}
