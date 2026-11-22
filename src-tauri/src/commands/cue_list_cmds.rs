//! Tauri commands for cue list management (create, delete, rename, switch).

use crate::show::cart_layout::{CartEdit, CartLayout};
use serde::Serialize;
use tauri::{Emitter, State};
use uuid::Uuid;

use crate::{show::{cue_list::{CueList, CueListMode}, workspace::Workspace}, state::AppState};

/// Compact info about a cue list, sent to the frontend.
#[derive(Debug, Clone, Serialize)]
pub struct CueListInfo {
    pub id: String,
    pub name: String,
    pub number: Option<String>,
    pub mode: CueListMode,
    pub chain_cart_cues: bool,
    pub cart_layout: Option<CartLayout>,
}

/// Returns the list of all cue lists and which one is active.
#[tauri::command]
pub fn get_cue_lists(state: State<'_, AppState>) -> Result<Vec<CueListInfo>, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    Ok(cue_list_infos(&ws))
}

/// Creates a new empty cue list, makes it active, and returns its ID.
#[tauri::command]
pub fn add_cue_list(
    state: State<'_, AppState>,
    handle: tauri::AppHandle,
    name: String,
) -> Result<String, String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let mut new_list = CueList::new(&name);
    new_list.auto_renumber = ws.preferences.general.auto_renumber_on_reorder;
    let new_id = new_list.id;
    ws.cue_lists.push(new_list);
    ws.active_cue_list_id = new_id;
    ws.mark_modified();
    emit_cue_lists_changed(&handle, &ws);
    let _ = handle.emit("playhead-moved", serde_json::json!({ "cue_id": null }));
    Ok(new_id.to_string())
}

/// Removes a cue list by ID. Errors if it is the last remaining list.
/// If the active list is removed, the first remaining list becomes active.
#[tauri::command]
pub fn remove_cue_list(
    state: State<'_, AppState>,
    handle: tauri::AppHandle,
    id: String,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if ws.cue_lists.len() <= 1 {
        return Err("Cannot remove the last cue list".into());
    }
    let target: Uuid = id.parse().map_err(|_| "Invalid cue list ID")?;
    let was_active = ws.active_cue_list_id == target;
    let stop_fade_ms = ws.preferences.audio.default_fade_out_ms;
    // Stop any cues still running in this list before it disappears — a
    // Group's hard_stop recurses into its children, so a top-level pass
    // covers the whole tree. Otherwise their engine voices would be
    // orphaned, with no cue row left to reach them.
    if let Some(cl) = ws.cue_lists.iter_mut().find(|cl| cl.id == target) {
        for cue in cl.cues.iter_mut() {
            super::cue_cmds::stop_if_live(cue.as_mut(), &state, stop_fade_ms);
        }
    }
    ws.cue_lists.retain(|cl| cl.id != target);
    if was_active {
        ws.active_cue_list_id = ws.cue_lists[0].id;
    }
    ws.mark_modified();
    emit_cue_lists_changed(&handle, &ws);
    if was_active {
        let new_ph = ws.active_cue_list().and_then(|cl| cl.playhead_cue_id);
        let _ = handle.emit("playhead-moved", serde_json::json!({ "cue_id": new_ph }));
    }
    Ok(())
}

/// Renames a cue list.
#[tauri::command]
pub fn rename_cue_list(
    state: State<'_, AppState>,
    handle: tauri::AppHandle,
    id: String,
    name: String,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let target: Uuid = id.parse().map_err(|_| "Invalid cue list ID")?;
    let cl = ws
        .cue_lists
        .iter_mut()
        .find(|cl| cl.id == target)
        .ok_or("Cue list not found")?;
    cl.name = name;
    ws.mark_modified();
    emit_cue_lists_changed(&handle, &ws);
    Ok(())
}

/// Update a saved list address only if the tab still belongs to the open show.
#[tauri::command]
pub fn set_cue_list_number(
    state: State<'_, AppState>,
    handle: tauri::AppHandle,
    workspace_id: Uuid,
    list_id: Uuid,
    expected: Option<String>,
    number: Option<String>,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if ws.instance_id() != workspace_id {
        return Err("The workspace changed. Reopen the cue list number field.".into());
    }
    let index = ws.cue_lists.iter().position(|list| list.id == list_id).ok_or("Cue list not found")?;
    let number = number.as_deref().map(str::trim).filter(|value| !value.is_empty()).map(str::to_owned);
    if ws.cue_lists[index].number != expected {
        return Err("The cue list number changed. Reopen the field to edit it.".into());
    }
    if ws.cue_lists[index].number == number {
        return Ok(());
    }
    if let Some(number) = &number {
        if ws.cue_lists.iter().any(|list| list.number.as_ref() == Some(number)) {
            return Err(format!("Cue list number {number} is already in use."));
        }
    }
    ws.cue_lists[index].number = number;
    ws.mark_modified();
    emit_cue_lists_changed(&handle, &ws);
    let _ = handle.emit("workspace-modified", ());
    Ok(())
}

/// Switches the active cue list. Emits `cue-lists-changed` and `playhead-moved`.
#[tauri::command]
pub fn set_active_cue_list(
    state: State<'_, AppState>,
    handle: tauri::AppHandle,
    id: String,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let target: Uuid = id.parse().map_err(|_| "Invalid cue list ID")?;
    if !ws.cue_lists.iter().any(|cl| cl.id == target) {
        return Err("Cue list not found".into());
    }
    ws.active_cue_list_id = target;
    let new_playhead = ws.active_cue_list().and_then(|cl| cl.playhead_cue_id);
    emit_cue_lists_changed(&handle, &ws);
    let _ = handle.emit("playhead-moved", serde_json::json!({ "cue_id": new_playhead }));
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

fn cue_list_infos(ws: &Workspace) -> Vec<CueListInfo> {
    ws.cue_lists
        .iter()
        .map(|cl| CueListInfo {
            id: cl.id.to_string(),
            name: cl.name.clone(),
            number: cl.number.clone(),
            mode: cl.mode,
            chain_cart_cues: cl.chain_cart_cues,
            cart_layout: cl.cart_layout.clone(),
        })
        .collect()
}

/// Set the playback mode of a cue list (Sequential or Cart).
#[tauri::command]
pub fn set_cue_list_mode(
    state: State<'_, AppState>,
    handle: tauri::AppHandle,
    id: String,
    mode: CueListMode,
    chain_cart_cues: Option<bool>,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let target: Uuid = id.parse().map_err(|_| "Invalid cue list ID")?;
    let cl = ws
        .cue_lists
        .iter_mut()
        .find(|cl| cl.id == target)
        .ok_or("Cue list not found")?;
    cl.set_mode(mode, chain_cart_cues);
    let playhead = cl.playhead_cue_id;
    if ws.active_cue_list_id == target {
        let _ = handle.emit("playhead-moved", serde_json::json!({ "cue_id": playhead }));
    }
    ws.mark_modified();
    emit_cue_lists_changed(&handle, &ws);
    Ok(())
}

/// Commit a layout edit against the named active list under the same lock as undo.
#[tauri::command]
pub fn edit_cart_layout(
    state: State<'_, AppState>, handle: tauri::AppHandle, list_id: Uuid, edit: CartEdit,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if ws.active_cue_list_id != list_id { return Err("The active cart changed".into()); }
    let cl = ws.active_cue_list_mut().ok_or("No active cart")?;
    let next = cl.edited_cart_layout(edit).map_err(|e| e.to_string())?;
    if cl.cart_layout == next { return Ok(()); }
    state.undo_stack.lock().map_err(|e| e.to_string())?
        .push_action(list_id, super::undo_cmds::take_snapshot(cl));
    cl.cart_layout = next;
    ws.mark_modified();
    emit_cue_lists_changed(&handle, &ws);
    let _ = handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

pub fn emit_cue_lists_changed(handle: &tauri::AppHandle, ws: &Workspace) {
    let _ = handle.emit(
        "cue-lists-changed",
        serde_json::json!({
            "cue_lists": cue_list_infos(ws),
            "active_cue_list_id": ws.active_cue_list_id.to_string(),
        }),
    );
}
