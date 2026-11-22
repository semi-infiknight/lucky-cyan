//! Tauri commands for MIDI device enumeration and test-send.

use std::sync::Arc;

use tauri::{Emitter, State};
use uuid::Uuid;

use crate::{
    cue::midi_cue::{send_midi_preview, send_midi_messages, MidiMessage, MidiMessageType},
    engine::midi_trigger::{self, MidiTrigger, MidiTriggerListener},
    machine_config::{self, MidiTriggerMachineConfig},
    state::AppState,
};

/// Return the names of all available MIDI output ports.
#[tauri::command]
pub fn list_midi_output_ports(_state: State<'_, AppState>) -> Vec<String> {
    match midir::MidiOutput::new("Inkue-list") {
        Ok(out) => out
            .ports()
            .iter()
            .filter_map(|p| out.port_name(p).ok())
            .collect(),
        Err(e) => {
            log::warn!("MIDI: failed to enumerate output ports: {e}");
            Vec::new()
        }
    }
}

/// Send a single MIDI message immediately.  Used by the inspector Test button.
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn send_midi_test(
    port_name: String,
    message_type: MidiMessageType,
    channel: u8,
    data1: u8,
    data2: u8,
    msc: Option<crate::cue::msc::MscMessage>,
    sysex: Option<String>,
    require_unique_output: Option<bool>,
    _state: State<'_, AppState>,
) -> Result<(), String> {
    let message = MidiMessage { port_name, message_type, channel, data1, data2, msc, sysex, fade: None };
    let result = if message_type == MidiMessageType::Sysex || require_unique_output.unwrap_or(false) {
        send_midi_preview(&message)
    } else {
        send_midi_messages(&[message])
    };
    result.map_err(|error| error.to_string())
}

// ---------------------------------------------------------------------------
// Per-cue MIDI triggers
// ---------------------------------------------------------------------------

/// Names of the MIDI **input** ports on this machine (trigger source picker).
#[tauri::command]
pub fn list_midi_input_ports() -> Vec<String> {
    midi_trigger::list_midi_input_ports()
}

/// The machine's MIDI-trigger settings (enabled + input port).
#[tauri::command]
pub fn get_midi_trigger_config() -> MidiTriggerMachineConfig {
    machine_config::load_midi_trigger_config()
}

/// Persist the MIDI-trigger settings and hot-apply them: start, stop or
/// re-open the listener so a port change takes effect without a restart.
#[tauri::command]
pub fn set_midi_trigger_config(
    config: MidiTriggerMachineConfig,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if config.msc_device_id.is_some() && config.msc_destination().is_none() {
        return Err("Choose a named MIDI input and a Show Control device ID from 0 to 126.".into());
    }
    let _workspace = state.workspace.lock().map_err(|e| e.to_string())?;
    let mut slot = state.midi_listener.lock().map_err(|e| e.to_string())?;
    machine_config::save_midi_trigger_config(&config).map_err(|e| e.to_string())?;
    drop(slot.take());
    *slot = MidiTriggerListener::open(config, Arc::clone(&state.midi_trigger_gate));
    Ok(())
}

#[tauri::command]
pub fn get_midi_trigger_override(state: State<'_, AppState>) -> bool {
    state.midi_trigger_gate.suspended()
}

#[tauri::command]
pub fn set_midi_trigger_override(
    suspended: bool, state: State<'_, AppState>, app_handle: tauri::AppHandle,
) -> Result<(), String> {
    // A transport tick drains and fires these inputs under the same lock.
    // The returned state therefore covers any in-flight dispatch.
    let _workspace = state.workspace.lock().map_err(|e| e.to_string())?;
    state.midi_trigger_gate.set_suspended(suspended);
    let _ = app_handle.emit("midi-trigger-override", suspended);
    Ok(())
}

/// The MIDI trigger bound to a cue, if any.
#[tauri::command]
pub fn get_cue_midi_trigger(
    cue_id: String,
    state: State<'_, AppState>,
) -> Option<MidiTrigger> {
    let id: Uuid = cue_id.parse().ok()?;
    let ws = state.workspace.lock().ok()?;
    ws.active_cue_list()?.midi_triggers.get(&id).copied()
}

/// Bind (or, with `trigger: null`, unbind) a MIDI trigger on a cue.
#[tauri::command]
pub fn set_cue_midi_trigger(
    cue_id: String,
    trigger: Option<MidiTrigger>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    ws.mark_modified();
    let cue_list = ws.active_cue_list_mut().ok_or("No active cue list")?;
    match trigger {
        Some(t) => { cue_list.midi_triggers.insert(id, t); }
        None => { cue_list.midi_triggers.remove(&id); }
    }
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// MIDI learn: the most recent message the listener saw, as a ready-made
/// trigger. Returns `None` until something arrives, so the UI can poll while
/// the operator presses the key they want.
#[tauri::command]
pub fn learn_midi_trigger(state: State<'_, AppState>) -> Option<MidiTrigger> {
    let slot = state.midi_listener.lock().ok()?;
    let listener = slot.as_ref()?;
    MidiTrigger::from_message(&listener.last_message()?)
}

/// Forget the last received message so a Learn starts from a clean slate.
#[tauri::command]
pub fn clear_midi_learn(state: State<'_, AppState>) -> Result<(), String> {
    let slot = state.midi_listener.lock().map_err(|e| e.to_string())?;
    if let Some(listener) = slot.as_ref() {
        listener.clear_last_message();
    }
    Ok(())
}
