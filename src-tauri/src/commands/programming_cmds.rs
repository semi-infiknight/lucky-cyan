//! The operator previews the sheet, picks its media folder and explicitly
//! opens a new workspace. The external client only returns proposal text.
use crate::{commands::workspace_cmds::install_workspace, show::Workspace, state::AppState};
use anyhow::{ensure, Context, Result};
use inkue_control::CueSheet;
use serde_json::json;
use std::path::Path;
use tauri::State;
use uuid::Uuid;

#[tauri::command]
pub fn preview_audio_sheet(title: String, text: String) -> Result<CueSheet, String> {
    let result = if text.trim_start().starts_with('{') {
        serde_json::from_str::<CueSheet>(&text)
            .map_err(anyhow::Error::from)
            .and_then(|sheet| {
                sheet.validate()?;
                Ok(sheet)
            })
    } else {
        CueSheet::from_script(&title, &text)
    };
    result.map_err(|e| e.to_string())
}

fn build_audio_document(sheet: &CueSheet, folder: &Path) -> Result<String> {
    sheet.validate()?;
    let base = folder.canonicalize().context("open media directory")?;
    ensure!(base.is_dir(), "select a media directory");
    let list = Uuid::new_v4();
    let now = chrono::Utc::now().to_rfc3339();
    let mut cues = Vec::with_capacity(sheet.cues.len());
    for mark in &sheet.cues {
        let path = base
            .join(&mark.media)
            .canonicalize()
            .with_context(|| format!("cue {}: missing {}", mark.number, mark.media))?;
        ensure!(
            path.starts_with(&base) && path.is_file(),
            "cue {}: pick a file inside the selected media directory",
            mark.number
        );
        cues.push(json!({"id": Uuid::new_v4(), "type":"audio", "number":mark.number,
            "name": mark.name, "file_path":path, "volume_db": mark.level_db, "continue_mode":"do_not_continue"}));
    }
    Ok(serde_json::to_string(&json!({
        "schema_version":1, "workspace":{"name":sheet.title,"created_at":now,"modified_at":now},
        "active_cue_list_id":list, "cue_lists":[{"id":list, "name":sheet.title,
            "mode":"sequential","playhead_cue_id":cues[0]["id"],"cues":cues}],
        "programming_history":[{"time":now,"operation":"accept_audio_sheet","cue_count":sheet.cues.len()}]
    }))?)
}

#[tauri::command]
pub fn accept_audio_sheet(
    sheet: CueSheet,
    folder: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let doc = build_audio_document(&sheet, Path::new(&folder)).map_err(|e| format!("{e:#}"))?;
    let mut show = {
        let registry = state.registry.lock().map_err(|e| e.to_string())?;
        Workspace::from_json_str(&doc, None, &registry).map_err(|e| format!("{e:#}"))?
    };
    show.mark_modified();
    install_workspace(&state, &app_handle, show)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_and_accept_share_validation_and_never_fire_the_new_list() {
        let script = "[SOUND | S1 | Standby | standby.wav | -12]";
        let sheet = preview_audio_sheet("Sound check".into(), script.into()).unwrap();
        let base = Path::new(env!("CARGO_MANIFEST_DIR")).join("resources/rehearsal");
        let json: serde_json::Value =
            serde_json::from_str(&build_audio_document(&sheet, &base).unwrap()).unwrap();
        assert_eq!(
            json["cue_lists"][0]["cues"][0]["continue_mode"],
            "do_not_continue"
        );
        assert_eq!(
            json["programming_history"][0]["operation"],
            "accept_audio_sheet"
        );
        let mut missing = sheet;
        missing.cues[0].media = "missing.wav".into();
        assert!(build_audio_document(&missing, &base).is_err());
        missing.cues[0].media = "../standby.wav".into();
        assert!(build_audio_document(&missing, &base).is_err());
    }
}
