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
}
