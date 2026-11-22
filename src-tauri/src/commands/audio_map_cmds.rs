//! Edit reusable calibrations separately from the cues that use them.

use serde::Serialize;
use tauri::{Emitter, State};
use uuid::Uuid;

use crate::{
    cue::point_mix::{AudioMap, AudioMapPatch, AudioMark},
    show::Workspace,
    state::AppState,
};

/// Calibration list tied to the currently open workspace instance.
#[derive(Serialize)]
pub struct AudioMapLibrary {
    workspace_id: Uuid,
    maps: Vec<AudioMap>,
}

/// Get the open workspace's saved maps.
#[tauri::command]
pub fn list_audio_maps(state: State<'_, AppState>) -> Result<AudioMapLibrary, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    Ok(AudioMapLibrary {
        workspace_id: ws.instance_id,
        maps: ws.audio_maps.clone(),
    })
}

/// Resolve the assignment without falling back to another output patch.
pub(super) fn resolve_map_patch(ws: &Workspace, id: Option<Uuid>) -> Result<AudioMapPatch, String> {
    match id {
        Some(id) => ws
            .output_patches
            .iter()
            .find(|patch| patch.id == id)
            .map(AudioMapPatch::from_output_patch)
            .ok_or_else(|| "Assign an available output patch first.".into()),
        None => Ok(AudioMapPatch::stereo()),
    }
}

/// Peak samples in the calibration's logical channel order on the live main device.
#[tauri::command]
pub fn read_audio_map_peaks(
    workspace_id: Uuid,
    patch: AudioMapPatch,
    state: State<'_, AppState>,
) -> Result<Option<Vec<f32>>, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    check_map_patch(&ws, workspace_id, &patch)?;
    let device = patch.device_id.as_deref()
        .ok_or("Assign a named output patch to see live meters.")?;
    state.audio_engine.map_output_peaks(device, &patch.channels)
}

fn check_map_patch(ws: &Workspace, workspace_id: Uuid, patch: &AudioMapPatch) -> Result<(), String> {
    if ws.instance_id != workspace_id {
        return Err("The workspace changed. Reopen the audio map editor.".into());
    }
    if *patch != resolve_map_patch(ws, patch.id)? {
        return Err("The output patch changed. Reopen the audio map editor.".into());
    }
    Ok(())
}

/// Create a map, or update the exact revision opened by the operator.
#[tauri::command]
pub fn save_audio_map(
    workspace_id: Uuid,
    previous: Option<AudioMap>,
    patch: AudioMapPatch,
    name: String,
    marks: Vec<AudioMark>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<AudioMap, String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let map = save_map(&mut ws, workspace_id, previous.as_ref(), patch, name, marks)?;
    drop(ws);
    emit_map_change(&app_handle);
    Ok(map)
}

fn save_map(
    ws: &mut Workspace,
    workspace_id: Uuid,
    previous: Option<&AudioMap>,
    patch: AudioMapPatch,
    name: String,
    mut marks: Vec<AudioMark>,
) -> Result<AudioMap, String> {
    check_map_patch(ws, workspace_id, &patch)?;
    let index = previous
        .map(|expected| map_index(ws, expected.id, expected.revision))
        .transpose()?;
    if index.is_some_and(|index| ws.audio_maps[index].patch != patch) {
        return Err("Save a new map to calibrate a different output assignment.".into());
    }
    for mark in &mut marks {
        mark.name = mark.name.trim().to_owned();
    }
    let map = AudioMap {
        id: previous.map(|map| map.id).unwrap_or_else(Uuid::new_v4),
        revision: Uuid::new_v4(),
        patch,
        name: name.trim().to_owned(),
        marks,
    };
    map.validate()?;
    if ws.audio_maps.iter().any(|existing| {
        existing.id != map.id && existing.name.to_lowercase() == map.name.to_lowercase()
    }) {
        return Err("Choose a name that is unique in this workspace.".into());
    }
    if let Some(index) = index {
        ws.audio_maps[index] = map.clone();
    } else {
        ws.audio_maps.push(map.clone());
    }
    ws.mark_modified();
    Ok(map)
}

/// Remove a library map. Any cue that used it keeps its applied snapshot.
#[tauri::command]
pub fn delete_audio_map(
    workspace_id: Uuid,
    id: Uuid,
    revision: Uuid,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if ws.instance_id != workspace_id {
        return Err("The workspace changed. Reopen the audio map editor.".into());
    }
    let index = map_index(&ws, id, revision)?;
    ws.audio_maps.remove(index);
    ws.mark_modified();
    drop(ws);
    emit_map_change(&app_handle);
    Ok(())
}

pub(super) fn checked_map<'a>(
    ws: &'a Workspace,
    workspace_id: Option<Uuid>,
    patch: Option<Uuid>,
    requested: &AudioMap,
) -> Result<&'a AudioMap, String> {
    if workspace_id != Some(ws.instance_id) {
        return Err("The workspace changed. Reload its maps before applying a mix.".into());
    }
    let map = &ws.audio_maps[map_index(ws, requested.id, requested.revision)?];
    if requested != map {
        return Err("Save or reload the calibration before applying it to a cue.".into());
    }
    if map.patch != resolve_map_patch(ws, patch)? {
        return Err("The map was calibrated for a different output assignment.".into());
    }
    Ok(map)
}

fn map_index(ws: &Workspace, id: Uuid, revision: Uuid) -> Result<usize, String> {
    ws.audio_maps
        .iter()
        .position(|map| map.id == id && map.revision == revision)
        .ok_or_else(|| "The audio map changed or was deleted. Reload the saved maps.".into())
}

fn emit_map_change(handle: &tauri::AppHandle) {
    let _ = handle.emit("audio-maps-changed", ());
    let _ = handle.emit("workspace-modified", serde_json::json!({}));
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::*;
    use crate::cue::{
        audio_cue::AudioCueFactory,
        point_mix::{stereo_calibration, PointMix},
        registry::CueRegistry,
        types::CueType,
    };

    #[test]
    fn map_revision_and_patch_checks_prevent_stale_apply() {
        let mut ws = Workspace::new("Test");
        let owner = ws.instance_id;
        let seed = stereo_calibration();
        let original = save_map(&mut ws, owner, None, seed.patch, seed.name, seed.marks).unwrap();
        assert!(checked_map(&ws, Some(owner), None, &original).is_ok());
        assert!(checked_map(&ws, Some(Uuid::new_v4()), None, &original).is_err());
        let mut patch: crate::engine::device_manager::OutputPatch = serde_json::from_value(serde_json::json!({
            "id": Uuid::new_v4(), "name": "House", "device_id": "card", "channels": [1, 2], "gain_db": 0.0,
        })).unwrap();
        let renamed = save_map(
            &mut ws,
            owner,
            Some(&original),
            original.patch.clone(),
            "Stage".into(),
            original.marks.clone(),
        )
        .unwrap();
        assert_eq!(original.id, renamed.id);
        assert_ne!(original.revision, renamed.revision);
        assert!(checked_map(&ws, Some(owner), None, &original).is_err());
        assert!(save_map(
            &mut ws,
            owner,
            Some(&original),
            original.patch.clone(),
            "Old".into(),
            original.marks.clone()
        )
        .is_err());
        ws.output_patches.push(patch.clone());
        assert!(checked_map(&ws, Some(owner), Some(patch.id), &renamed).is_err());
        let assignment = AudioMapPatch::from_output_patch(&patch);
        assert!(check_map_patch(&ws, owner, &assignment).is_ok());
        assert!(check_map_patch(&ws, Uuid::new_v4(), &assignment).is_err());
        let bound = save_map(
            &mut ws,
            owner,
            None,
            assignment,
            "House map".into(),
            renamed.marks.clone(),
        )
        .unwrap();
        assert!(checked_map(&ws, Some(owner), Some(patch.id), &bound).is_ok());
        patch.channels.reverse();
        ws.output_patches[0] = patch.clone();
        assert!(check_map_patch(&ws, owner, &bound.patch).is_err());
        assert!(checked_map(&ws, Some(owner), Some(patch.id), &bound).is_err());
    }

    #[test]
    fn applied_snapshot_keeps_routes_after_map_edit_removal_and_reload() {
        let mut ws = Workspace::new("Test");
        let owner = ws.instance_id;
        let mut registry = CueRegistry::new();
        registry.register(CueType::Audio, Box::new(AudioCueFactory));
        let seed = stereo_calibration();
        let map = save_map(&mut ws, owner, None, seed.patch, seed.name, seed.marks).unwrap();
        let point = PointMix {
            input: 2,
            source: map.marks[0].position.clone(),
            speakers: Vec::new(),
            map: Some(map.clone()),
        };
        let mut cue = registry.create(&CueType::Audio).unwrap().serialize();
        cue["level_matrix"] = serde_json::json!([[0, -60]]);
        let apply = serde_json::json!({"point_mix": point});
        cue["point_mix"] = apply["point_mix"].clone();
        crate::commands::cue_cmds::apply_point_routing(&mut cue, &apply, None, &[]).unwrap();
        let expected_routes = serde_json::json!([[0.0, -60.0], [0.0, -12.0]]);
        assert_eq!(cue["level_matrix"], expected_routes);
        ws.cue_lists[0].cues.push(registry.from_json(cue).unwrap());

        let saved = ws.to_json(Path::new("test.inkue")).unwrap();
        let mut reopened = Workspace::from_json_str(&saved, None, &registry).unwrap();
        assert_eq!(reopened.audio_maps, [map.clone()]);
        let transfer = tempfile::tempdir().unwrap();
        let collected = ws.collect_and_save(transfer.path()).unwrap();
        let collected = Workspace::load(collected.workspace_path.into(), &registry).unwrap();
        assert_eq!(collected.audio_maps, [map.clone()]);
        assert_eq!(
            collected.cue_lists[0].cues[0].serialize()["level_matrix"],
            expected_routes
        );
        let new_owner = reopened.instance_id;
        let mut marks = map.marks.clone();
        marks[0].levels[0] = -18.0;
        let updated = save_map(
            &mut reopened,
            new_owner,
            Some(&map),
            map.patch.clone(),
            "Different calibration".into(),
            marks,
        )
        .unwrap();
        let index = map_index(&reopened, updated.id, updated.revision).unwrap();
        reopened.audio_maps.remove(index);
        let recovery = reopened.to_recovery_json().unwrap();
        let restored = Workspace::from_json_str(&recovery, None, &registry).unwrap();
        let cue = restored.cue_lists[0].cues[0].serialize();
        assert!(restored.audio_maps.is_empty());
        assert_eq!(cue["level_matrix"], expected_routes);
        assert_eq!(cue["point_mix"], apply["point_mix"]);
    }
}
