use std::sync::Mutex;

use tauri::State;

use crate::editor::{
    Appearance, CommandReceipt, EditorSession, EditorSnapshot, HotkeyList, LayoutDraft,
};

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AutomaticTilingSettings {
    pub topology_fingerprint: String,
    pub matched_profile: bool,
    pub enabled: bool,
    pub outer_gap: i32,
    pub inner_gap: i32,
    pub focus_border_enabled: bool,
    pub focus_border_color: String,
    pub focus_border_thickness: u16,
}

fn config_directory() -> Result<std::path::PathBuf, String> {
    let app_data = std::env::var_os("APPDATA")
        .ok_or_else(|| "APPDATA is unavailable; cannot locate Mosaix config".to_owned())?;
    Ok(std::path::PathBuf::from(app_data)
        .join("Mosaix")
        .join("config"))
}

fn format_color(color: mosaix_config::RgbaColor) -> String {
    format!(
        "#{:02X}{:02X}{:02X}{:02X}",
        color.red, color.green, color.blue, color.alpha
    )
}

fn parse_color(value: &str) -> Result<mosaix_config::RgbaColor, String> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 8 || !hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err("focus border color must be #RRGGBBAA".to_owned());
    }
    let component = |start| u8::from_str_radix(&hex[start..start + 2], 16).unwrap();
    Ok(mosaix_config::RgbaColor {
        red: component(0),
        green: component(2),
        blue: component(4),
        alpha: component(6),
    })
}

#[cfg(windows)]
fn load_tiling_settings() -> Result<AutomaticTilingSettings, String> {
    let directory = config_directory()?;
    mosaix_config::ensure_default_config(&directory).map_err(|error| error.to_string())?;
    let set = mosaix_config::load(&directory)
        .map_err(|error| error.to_string())?
        .map_err(|errors| {
            errors
                .into_iter()
                .map(|error| error.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        })?;
    let displays =
        mosaix_platform_windows::enumerate_displays().map_err(|error| error.to_string())?;
    let topology_fingerprint = mosaix_domain::topology_fingerprint(&displays);
    let matched = set
        .profiles
        .iter()
        .find(|profile| profile.fingerprint == topology_fingerprint);
    let config = matched.map(|profile| &profile.config).unwrap_or(&set.base);
    Ok(AutomaticTilingSettings {
        topology_fingerprint,
        matched_profile: matched.is_some(),
        enabled: matched.is_some_and(|profile| profile.config.automatic_tiling_enabled),
        outer_gap: config.gaps.outer,
        inner_gap: config.gaps.inner,
        focus_border_enabled: config.focus_border.enabled,
        focus_border_color: format_color(config.focus_border.color),
        focus_border_thickness: config.focus_border.thickness,
    })
}

#[cfg(not(windows))]
fn load_tiling_settings() -> Result<AutomaticTilingSettings, String> {
    Err("automatic tiling settings are available on Windows only".to_owned())
}

#[tauri::command]
pub fn load_automatic_tiling_settings() -> Result<AutomaticTilingSettings, String> {
    load_tiling_settings()
}

#[tauri::command]
pub fn save_automatic_tiling_settings(
    settings: AutomaticTilingSettings,
) -> Result<AutomaticTilingSettings, String> {
    if !(1..=16).contains(&settings.focus_border_thickness) {
        return Err("focus border thickness must be between 1 and 16".to_owned());
    }
    let color = parse_color(&settings.focus_border_color)?;
    mosaix_config::save_profile_settings(
        &config_directory()?,
        mosaix_config::ProfileSettingsUpdate {
            fingerprint: settings.topology_fingerprint,
            automatic_tiling_enabled: settings.enabled,
            gaps: mosaix_config::GapsOverride {
                outer: Some(settings.outer_gap),
                inner: Some(settings.inner_gap),
            },
            focus_border: mosaix_config::FocusBorderOverride {
                enabled: Some(settings.focus_border_enabled),
                color: Some(color),
                thickness: Some(settings.focus_border_thickness),
            },
        },
    )
    .map_err(|error| error.to_string())?;
    load_tiling_settings()
}

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

/// Every hotkey binding in effect, for the read-only list in the
/// interface. Read from the agent, not from disk.
#[tauri::command]
pub fn load_hotkey_bindings(state: State<'_, EditorState>) -> Result<HotkeyList, String> {
    let mut session = state.0.lock().map_err(|_| "editor state is unavailable")?;
    session.hotkeys().map_err(|error| error.to_string())
}

#[tauri::command]
pub fn set_appearance(appearance: Appearance, state: State<'_, EditorState>) -> Result<(), String> {
    let mut session = state.0.lock().map_err(|_| "editor state is unavailable")?;
    session.set_appearance(appearance);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn focus_border_color_requires_platform_neutral_rgba() {
        assert_eq!(
            parse_color("#0078D7FF").unwrap(),
            mosaix_config::RgbaColor {
                red: 0,
                green: 120,
                blue: 215,
                alpha: 255,
            }
        );
        assert!(parse_color("#123456").is_err());
    }
}
