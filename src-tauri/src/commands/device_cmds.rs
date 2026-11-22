//! Tauri commands for audio device and Output Patch management.
//!
//! Output Patches live in the **workspace** (`ws.output_patches`) — the same
//! table `CueContext::resolve_patch` reads at GO time — so edits here are
//! persisted in the `.inkue` file and affect playback immediately.

use tauri::{Emitter, Manager, State};
use uuid::Uuid;

use crate::{
    engine::{audio_input, cue_effects::{validate_effects, CueEffect}, device_manager::{CueOutputEffects, DeviceInfo, OutputPatch}},
    state::AppState,
};

/// Return the audio output devices Output Patches may target.
///
/// DAW convention (Ableton-style): the selected backend defines the output
/// universe.  On the ASIO backend only ASIO drivers are listed — their WASAPI
/// endpoints are held exclusively by ASIO anyway, and the patch channels route
/// inside the full-width main ASIO stream.  On shared backends the normal
/// WASAPI/CoreAudio/ALSA devices are listed (no ASIO entries).
/// `backend` overrides the persisted one so the Preferences panel can show the
/// device universe of the backend **currently selected in the UI**, before it
/// has been applied — switching the Backend dropdown to ASIO used to keep
/// listing WASAPI endpoints until Apply was clicked, which made it look as
/// though ASIO patches were impossible.
#[tauri::command]
pub fn list_output_devices(
    backend: Option<String>,
    state: State<'_, AppState>,
) -> Result<Vec<DeviceInfo>, String> {
    #[cfg(target_os = "linux")]
    use crate::engine::device_manager::linux_devices;

    // Build ALSA fallback from the engine's cached device list.
    let fallback = {
        let engine = &state.audio_engine;
        let mgr = engine.device_manager.lock().map_err(|e| e.to_string())?;
        mgr.devices().to_vec()
    };

    // Linux lists ALSA/PipeWire devices whatever backend the panel shows.
    #[cfg(target_os = "linux")]
    let _ = backend;
    #[cfg(target_os = "linux")]
    return Ok(linux_devices(false, fallback));
    #[cfg(not(target_os = "linux"))]
    {
        use crate::preferences::AudioBackend;
        let config = crate::machine_config::load();
        // What the operator is looking at wins over what is running.
        let selected = match backend.as_deref() {
            Some("asio") => AudioBackend::Asio,
            Some("wasapi_exclusive") => AudioBackend::WasapiExclusive,
            Some("wasapi_shared") => AudioBackend::WasapiShared,
            Some("system_default") => AudioBackend::SystemDefault,
            _ => config.backend,
        };

        // ASIO selected but not applied: the engine does not hold the driver
        // yet, so the live stream cannot describe it. Read the driver list
        // from the registry — fast, no cpal, safe to call either way.
        #[cfg(all(windows, feature = "asio-support"))]
        if matches!(selected, AudioBackend::Asio) && !matches!(config.backend, AudioBackend::Asio) {
            return Ok(crate::commands::preferences_cmds::list_asio_drivers_from_registry());
        }

        if cfg!(all(windows, feature = "asio-support"))
            && matches!(selected, AudioBackend::Asio)
        {
            // ASIO drivers are single-client and already held by the engine's
            // main stream — re-enumerating the ASIO host here fails or hangs.
            // Build the entry from the live stream state instead: same id as
            // `current_device_id` (→ patches route inside the full-width main
            // mix) with the open stream's real channel count.
            if let Some(id) = config.device_id.filter(|d| !d.is_empty()) {
                return Ok(vec![DeviceInfo {
                    name: format!("{id} (ASIO)"),
                    id,
                    channels: state.audio_engine.output_channels() as u16,
                    sample_rate: state.audio_engine.sample_rate(),
                }]);
            }
        }
        Ok(fallback)
    }
}

/// Return all available audio **input** devices (for Mic Cues / live capture).
///
/// Like output enumeration, the cpal input query is slow-to-hanging on Windows,
/// so it runs off the main thread and is time-bounded — the Preferences panel
/// never freezes on it.
#[tauri::command]
pub async fn list_input_devices() -> Result<Vec<DeviceInfo>, String> {
    let devices = tauri::async_runtime::spawn_blocking(|| {
        crate::engine::device_manager::run_bounded(
            crate::engine::device_manager::ENUM_TIMEOUT,
            audio_input::list_input_devices,
        )
        .unwrap_or_default()
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(devices)
}

/// Return the workspace's Output Patch table plus the default patch id.
#[derive(serde::Serialize)]
pub struct OutputPatchTable {
    pub patches: Vec<OutputPatch>,
    pub default_patch_id: Option<String>,
    pub workspace_id: Uuid,
}

/// Return all configured Output Patches (workspace-level, persisted).
#[tauri::command]
pub fn get_output_patches(state: State<'_, AppState>) -> Result<OutputPatchTable, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    Ok(OutputPatchTable {
        patches: ws.output_patches.clone(),
        default_patch_id: ws.default_output_patch_id.map(|id| id.to_string()),
        workspace_id: ws.instance_id,
    })
}

/// Create or update an Output Patch in the workspace.
#[tauri::command]
pub fn set_output_patch(
    patch_id: Option<String>,
    name: String,
    device_id: String,
    channels: Vec<u16>,
    channel_names: Option<Vec<String>>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let id = patch_id
        .as_deref()
        .and_then(|s| s.parse::<Uuid>().ok())
        .unwrap_or_else(Uuid::new_v4);

    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let reroute = match ws.output_patches.iter_mut().find(|p| p.id == id) {
        Some(existing) => {
            let mut updated = existing.clone();
            let reroute = updated.update_definition(name, device_id, channels, channel_names);
            if reroute { updated.validate_cue_output_effects()?; }
            if reroute && updated.has_audio_effects() {
                if state.audio_engine.patch_has_active_audio(id)? {
                    return Err("Stop File Audio using this patch before changing its effects route.".into());
                }
                updated.named_output()?;
            }
            *existing = updated;
            reroute
        }
        None => {
            let mut patch = OutputPatch::new(name, device_id, channels);
            patch.id = id;
            patch.channel_names = channel_names.unwrap_or_default();
            ws.output_patches.push(patch);
            true
        }
    };
    if reroute { state.audio_engine.stop_audition(None); }
    // The first patch ever created becomes the default automatically.
    if ws.default_output_patch_id.is_none() {
        ws.default_output_patch_id = Some(id);
    }
    ws.mark_modified();
    drop(ws);

    // Routing changed: drop aux streams from the previous patch table so
    // nothing keeps playing on a re-configured output; the next GO reopens
    // exactly what the new table needs.
    if reroute {
        state.audio_engine.close_all_aux();
    }
    let _ = app_handle.emit("output-patches-changed", ());
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(id.to_string())
}

/// Save the patch's File Audio admission rule for its next GO.
#[tauri::command]
pub fn set_output_patch_require_device(
    workspace_id: Uuid,
    patch_id: String,
    require_named_device: bool,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id = patch_id.parse::<Uuid>().map_err(|e| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if ws.instance_id != workspace_id {
        return Err("The workspace changed. Review its Output Patches before editing.".into());
    }
    let patch = ws.output_patches.iter_mut().find(|p| p.id == id)
        .ok_or_else(|| format!("Output patch {id} not found"))?;
    if !require_named_device && patch.has_audio_effects() {
        return Err("Bypass the Output Patch effects before turning off its required-device check.".into());
    }
    if patch.require_named_device != require_named_device {
        patch.require_named_device = require_named_device;
        ws.mark_modified();
        drop(ws);
        let _ = app_handle.emit("output-patches-changed", ());
        let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    }
    Ok(())
}

#[tauri::command]
pub(crate) fn set_output_patch_effects(
    workspace_id: Uuid,
    patch_id: Uuid,
    audio_effects: Vec<CueEffect>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    validate_effects(&audio_effects)?;
    edit_output_patch_effects(&state, &app_handle, workspace_id, patch_id, |patch| {
        patch.audio_effects = audio_effects;
        Ok(())
    })
}

#[tauri::command]
pub(crate) fn set_cue_output_effects(
    workspace_id: Uuid,
    patch_id: Uuid,
    output: usize,
    effects: Vec<CueEffect>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    validate_effects(&effects)?;
    edit_output_patch_effects(&state, &app_handle, workspace_id, patch_id, |patch| {
        if output >= patch.channels.len() {
            return Err("Select a cue output in this patch.".into());
        }
        patch.cue_output_effects.retain(|entry| entry.output != output);
        if !effects.is_empty() {
            patch.cue_output_effects.push(CueOutputEffects { output, effects });
            patch.cue_output_effects.sort_by_key(|entry| entry.output);
        }
        patch.validate_cue_output_effects()
    })
}

fn edit_output_patch_effects(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    workspace_id: Uuid,
    patch_id: Uuid,
    edit: impl FnOnce(&mut OutputPatch) -> Result<(), String>,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if ws.instance_id != workspace_id {
        return Err("The workspace changed. Review its Output Patches before editing.".into());
    }
    let patch = ws.output_patches.iter_mut().find(|patch| patch.id == patch_id)
        .ok_or_else(|| format!("Output patch {patch_id} not found"))?;
    let mut updated = patch.clone();
    edit(&mut updated)?;
    if patch.audio_effects == updated.audio_effects && patch.cue_output_effects == updated.cue_output_effects {
        return Ok(());
    }
    if state.audio_engine.patch_has_active_audio(patch_id)? {
        return Err("Stop File Audio using this patch before editing its effects.".into());
    }
    if updated.has_audio_effects() {
        updated.named_output()?;
    }
    *patch = updated;
    ws.mark_modified();
    drop(ws);
    let _ = app_handle.emit("output-patches-changed", ());
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Remove an Output Patch from the workspace.  Cues referencing it fall back
/// to the default patch (with a preflight warning).
#[tauri::command]
pub fn remove_output_patch(
    patch_id: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = patch_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if ws.output_patches.iter().any(|patch| patch.id == id && patch.has_audio_effects())
        && state.audio_engine.patch_has_active_audio(id)?
    {
        return Err("Stop File Audio using this patch before removing its effects route.".into());
    }
    let before = ws.output_patches.len();
    ws.output_patches.retain(|p| p.id != id);
    state.audio_engine.patch_faders.remove(id);
    if ws.output_patches.len() == before {
        return Err(format!("Output patch {id} not found"));
    }
    state.audio_engine.stop_audition(None);
    if ws.default_output_patch_id == Some(id) {
        ws.default_output_patch_id = ws.output_patches.first().map(|p| p.id);
    }
    ws.mark_modified();
    drop(ws);

    state.audio_engine.close_all_aux();
    let _ = app_handle.emit("output-patches-changed", ());
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Mixer fader: set an Output Patch's gain (dB), persisting it in the
/// workspace and hot-applying it to every playing voice routed through the
/// patch.
#[tauri::command]
pub fn set_output_patch_gain(
    patch_id: String,
    gain_db: f32,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = patch_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    {
        let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
        let patch = ws
            .output_patches
            .iter_mut()
            .find(|p| p.id == id)
            .ok_or_else(|| format!("Output patch {id} not found"))?;
        patch.gain_db = gain_db;
        let gain = crate::cue::types::db_to_linear(gain_db as f64) as f32;
        state.audio_engine.set_patch_gain(id, gain).map_err(|e| e.to_string())?;
        ws.mark_modified();
    }
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Set the workspace's default Output Patch (used by cues with no explicit patch).
#[tauri::command]
pub fn set_default_output_patch(
    patch_id: Option<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Option<Uuid> = patch_id
        .as_deref()
        .map(|s| s.parse::<Uuid>().map_err(|e| e.to_string()))
        .transpose()?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if let Some(id) = id {
        if !ws.output_patches.iter().any(|p| p.id == id) {
            return Err(format!("Output patch {id} not found"));
        }
    }
    ws.default_output_patch_id = id;
    ws.mark_modified();
    drop(ws);

    let _ = app_handle.emit("output-patches-changed", ());
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Show and focus the pre-created floating Output Mixer window.
///
/// The window is declared in `tauri.conf.json` with `visible: false` —
/// creating windows dynamically is unreliable on WebView2 (see the
/// preferences/float-timer windows, same pattern).
#[tauri::command]
pub fn open_mixer_window(app_handle: tauri::AppHandle) -> Result<(), String> {
    let w = app_handle
        .get_webview_window("mixer")
        .ok_or("mixer window not found")?;
    w.show().map_err(|e| e.to_string())?;
    w.set_focus().map_err(|e| e.to_string())?;
    Ok(())
}

/// Refresh the cached device list (call after hotplug events).
///
/// Enumeration is bounded and runs off the main thread, so a stuck device can
/// never freeze the UI on a manual refresh.
#[tauri::command]
pub async fn refresh_devices(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let engine = state.audio_engine.clone();
    let enumerated = tauri::async_runtime::spawn_blocking(move || {
        let devices = crate::engine::device_manager::run_bounded(
            crate::engine::device_manager::ENUM_TIMEOUT,
            crate::engine::device_manager::enumerate_output_devices,
        )
        .unwrap_or_default();
        if let Ok(mut mgr) = engine.device_manager.lock() {
            mgr.replace_cache(devices.clone());
        }
        devices
    })
    .await
    .map_err(|e| e.to_string())?;

    #[cfg(target_os = "linux")]
    let devices = crate::engine::device_manager::linux_devices(false, enumerated);
    #[cfg(not(target_os = "linux"))]
    let devices = enumerated;

    let _ = app_handle.emit("device-changed", serde_json::json!({ "devices": devices }));
    Ok(())
}
