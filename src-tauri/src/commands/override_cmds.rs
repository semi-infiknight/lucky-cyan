//! Global engine overrides (the Override-window equivalents): DMX output,
//! MIDI in/out, MSC in/out, external network input and timecode input can
//! each be disabled process-wide. Also settable over OSC (`/overrides/...`).

use tauri::Emitter;

use crate::engine::overrides::OVERRIDES;

#[tauri::command]
pub fn set_global_override(key: String, enabled: bool, app_handle: tauri::AppHandle) -> Result<(), String> {
    OVERRIDES.set(&key, enabled)?;
    let _ = app_handle.emit("global-overrides-changed", OVERRIDES.snapshot());
    Ok(())
}

#[tauri::command]
pub fn get_global_overrides() -> serde_json::Value {
    OVERRIDES.snapshot()
}

/// QLab `/overrideWindow {bool}`: show or hide the Override window.
#[tauri::command]
pub fn set_override_window_open(open: bool, app_handle: tauri::AppHandle) -> Result<(), String> {
    OVERRIDES.set_override_window(open);
    let _ = app_handle.emit("override-window-changed", open);
    Ok(())
}

#[tauri::command]
pub fn get_override_window_open() -> bool {
    OVERRIDES.override_window()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_report_every_override_key() {
        let snap = get_global_overrides();
        for key in crate::engine::overrides::OVERRIDE_KEYS {
            assert!(snap[key].is_boolean(), "{key}");
        }
        assert!(OVERRIDES.set("bogus", true).is_err());
    }

    #[test]
    fn override_window_state_round_trips() {
        let local = crate::engine::overrides::GlobalOverrides::new();
        assert!(!local.override_window());
        local.set_override_window(true);
        assert!(local.override_window());
        local.set_override_window(false);
        assert!(!local.override_window());
    }
}
