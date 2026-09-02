mod agent;
mod commands;
mod editor;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(commands::EditorState::default())
        .invoke_handler(tauri::generate_handler![
            commands::load_editor_snapshot,
            commands::preview_layout,
            commands::save_and_apply_layout,
            commands::load_hotkey_bindings,
            commands::set_appearance,
            commands::load_automatic_tiling_settings,
            commands::save_automatic_tiling_settings,
        ])
        .run(tauri::generate_context!())
        .expect("failed to run the Mosaix settings application");
}
