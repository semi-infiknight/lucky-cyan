//! Portable workspace preferences that have no cue, patch, or device references.
use std::{
    fs::File,
    io::{Read, Write},
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use uuid::Uuid;

use crate::{
    preferences::{CueColorStyle, CueRowHeight},
    show::workspace::Workspace,
    state::AppState,
};

const FORMAT: &str = "inkue-workspace-settings";
const VERSION: u32 = 1;
const THEME_VERSION: u32 = 2;
const MAX_FILE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum TransferTheme {
    Dark,
    Light,
    Navy,
    System,
}

impl TransferTheme {
    fn from_workspace(value: &str) -> Self {
        match value {
            "dark" => Self::Dark,
            "light" => Self::Light,
            "navy" => Self::Navy,
            _ => Self::System,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Dark => "dark",
            Self::Light => "light",
            Self::Navy => "navy",
            Self::System => "system",
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsValues {
    #[serde(skip_serializing_if = "Option::is_none")]
    double_go_protection_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    show_hard_stop_blackout: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    default_fade_out_ms: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    confirm_before_delete: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_scroll_to_playhead: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cue_row_height: Option<CueRowHeight>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cue_color_style: Option<CueColorStyle>,
    #[serde(skip_serializing_if = "Option::is_none")]
    theme: Option<TransferTheme>,
    #[serde(skip_serializing_if = "Option::is_none")]
    auto_renumber_on_reorder: Option<bool>,
}

impl SettingsValues {
    fn from_workspace(ws: &Workspace) -> Self {
        let prefs = &ws.preferences;
        Self {
            double_go_protection_ms: Some(prefs.general.double_go_protection_ms),
            show_hard_stop_blackout: Some(prefs.general.show_hard_stop_blackout),
            default_fade_out_ms: Some(prefs.audio.default_fade_out_ms),
            confirm_before_delete: Some(prefs.general.confirm_before_delete),
            auto_scroll_to_playhead: Some(prefs.general.auto_scroll_to_playhead),
            cue_row_height: Some(prefs.general.cue_row_height),
            cue_color_style: Some(prefs.display.cue_color_style),
            theme: Some(TransferTheme::from_workspace(&prefs.display.theme)),
            auto_renumber_on_reorder: Some(prefs.general.auto_renumber_on_reorder),
        }
    }

    fn apply(&self, ws: &mut Workspace) {
        let prefs = &mut ws.preferences;
        if let Some(v) = self.show_hard_stop_blackout {
            prefs.general.show_hard_stop_blackout = v;
        }
        if let Some(v) = self.double_go_protection_ms {
            prefs.general.double_go_protection_ms = v;
        }
        if let Some(v) = self.default_fade_out_ms {
            prefs.audio.default_fade_out_ms = v;
        }
        if let Some(v) = self.confirm_before_delete {
            prefs.general.confirm_before_delete = v;
        }
        if let Some(v) = self.auto_scroll_to_playhead {
            prefs.general.auto_scroll_to_playhead = v;
        }
        if let Some(v) = self.cue_row_height {
            prefs.general.cue_row_height = v;
        }
        if let Some(v) = self.cue_color_style {
            prefs.display.cue_color_style = v;
        }
        if let Some(v) = self.theme {
            prefs.display.theme = v.as_str().into();
        }
        if let Some(v) = self.auto_renumber_on_reorder {
            prefs.general.auto_renumber_on_reorder = v;
            ws.sync_auto_renumber();
        }
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsFile {
    format: String,
    version: u32,
    source_workspace: String,
    settings: SettingsValues,
}

#[derive(Deserialize, Serialize, PartialEq)]
pub struct SettingsSnapshot {
    name: String,
    path: Option<PathBuf>,
    document: Uuid,
    revision: u64,
    values: SettingsValues,
}

impl SettingsSnapshot {
    fn new(ws: &Workspace) -> Self {
        Self {
            name: ws.metadata.name.clone(),
            path: ws.file_path.clone(),
            document: ws.instance_id,
            revision: ws.revision,
            values: SettingsValues::from_workspace(ws),
        }
    }

    fn check(&self, ws: &Workspace) -> Result<(), String> {
        if self != &Self::new(ws) {
            return Err("The workspace changed. Review its current settings again.".into());
        }
        Ok(())
    }
}

#[tauri::command]
pub fn get_settings_snapshot(state: State<'_, AppState>) -> Result<SettingsSnapshot, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    Ok(SettingsSnapshot::new(&ws))
}

fn decode_file(text: &str) -> Result<SettingsFile, String> {
    // Check identity and version first, even for newer files with unfamiliar keys.
    let json: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("Invalid settings JSON: {e}"))?;
    if json.get("format").and_then(|v| v.as_str()) != Some(FORMAT) {
        return Err("Choose an Inkue workspace settings file.".into());
    }
    let version = json.get("version").and_then(|v| v.as_u64());
    if version != Some(u64::from(VERSION)) && version != Some(u64::from(THEME_VERSION)) {
        return Err("This desk supports workspace settings file versions 1 and 2.".into());
    }
    if json["settings"]
        .as_object()
        .is_some_and(|fields| fields.values().any(serde_json::Value::is_null))
    {
        return Err("Each selected setting must have a value.".into());
    }
    let doc: SettingsFile =
        serde_json::from_str(text).map_err(|e| format!("Invalid workspace settings: {e}"))?;
    if doc.version == VERSION && doc.settings.theme.is_some() {
        return Err("A workspace settings file with a desk theme requires version 2.".into());
    }
    if doc.settings == SettingsValues::default() {
        return Err("The file contains no transferable settings.".into());
    }
    Ok(doc)
}

#[tauri::command]
pub fn read_settings_file(path: PathBuf) -> Result<SettingsFile, String> {
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| {
            file.take((MAX_FILE_BYTES + 1) as u64)
                .read_to_end(&mut bytes)
        })
        .map_err(|e| format!("Unable to read the settings file: {e}"))?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("The settings file exceeds 64 KiB.".into());
    }
    decode_file(std::str::from_utf8(&bytes).map_err(|_| "The settings file must be UTF-8 JSON.")?)
}

fn save_file(path: &Path, doc: &SettingsFile) -> Result<(), String> {
    if !path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("inkuesettings"))
    {
        return Err("Save with the .inkuesettings extension.".into());
    }
    let parent = path.parent().ok_or("Choose a destination folder.")?;
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
    let data = serde_json::to_vec_pretty(doc).map_err(|e| e.to_string())?;
    if data.len() + 1 > MAX_FILE_BYTES {
        return Err("The workspace name makes this settings file too large.".into());
    }
    file.write_all(&data).map_err(|e| e.to_string())?;
    file.write_all(b"\n")
        .and_then(|_| file.as_file().sync_all())
        .map_err(|e| e.to_string())?;
    file.persist(path).map_err(|e| e.to_string())?;
    Ok(())
}

#[tauri::command]
pub fn export_settings_file(
    path: PathBuf,
    expected: SettingsSnapshot,
    settings: SettingsValues,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    expected.check(&ws)?;
    drop(ws);
    if settings == SettingsValues::default() {
        return Err("Select settings to export.".into());
    }
    save_file(
        &path,
        &SettingsFile {
            format: FORMAT.into(),
            version: if settings.theme.is_some() { THEME_VERSION } else { VERSION },
            source_workspace: expected.name,
            settings,
        },
    )
}

fn apply_settings(
    ws: &mut Workspace,
    expected: &SettingsSnapshot,
    settings: &SettingsValues,
) -> Result<(), String> {
    expected.check(ws)?;
    if settings == &SettingsValues::default() {
        return Err("Select settings to apply.".into());
    }
    let theme_changed = settings.theme.is_some_and(|theme| theme.as_str() != ws.preferences.display.theme);
    settings.apply(ws);
    if theme_changed || SettingsValues::from_workspace(ws) != expected.values {
        ws.mark_modified();
    }
    Ok(())
}

#[tauri::command]
pub fn apply_workspace_settings(
    expected: SettingsSnapshot,
    settings: SettingsValues,
    state: State<'_, AppState>,
    app: tauri::AppHandle,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    apply_settings(&mut ws, &expected, &settings)?;
    drop(ws);
    let _ = app.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{
        audio_cue::AudioCueFactory, registry::CueRegistry, traits::CueFactory, types::CueType,
    };
    use serde_json::{json, Value};

    #[test]
    fn selected_settings_roundtrip_without_changing_cues_routes_or_machine_buffer() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("crew.inkuesettings");
        let portable = SettingsFile {
            format: FORMAT.into(),
            version: VERSION,
            source_workspace: "Reference".into(),
            settings: SettingsValues {
                cue_row_height: Some(CueRowHeight::Tall),
                auto_renumber_on_reorder: Some(true),
                show_hard_stop_blackout: Some(true),
                ..Default::default()
            },
        };
        save_file(&file, &portable).unwrap();
        let loaded = read_settings_file(file).unwrap();
        let mut ws = Workspace::new("Performance");
        let patch = Uuid::new_v4();
        ws.output_patches.push(serde_json::from_value(json!({
            "id":patch, "name":"Backstage", "device_id":"venue-interface", "channels":[6,7], "gain_db":-6
        })).unwrap());
        ws.default_output_patch_id = Some(patch);
        ws.preferences.audio.audio_buffer_size = 512;
        let media = tmp.path().join("private-media.wav");
        std::fs::write(&media, b"media fixture").unwrap();
        let id = Uuid::new_v4();
        let cue = AudioCueFactory
            .from_json(json!({"type":"audio","id":id,"name":"Hold",
            "number":"S17.5","file_path":media,"output_patch_id":patch}))
            .unwrap();
        ws.cue_lists[0].push(cue);
        ws.cue_lists[0].set_playhead(Some(id)).unwrap();
        let before: Value = serde_json::from_str(&ws.to_recovery_json().unwrap()).unwrap();
        let expected = SettingsSnapshot::new(&ws);
        apply_settings(&mut ws, &expected, &loaded.settings).unwrap();
        assert_eq!(ws.revision, expected.revision + 1);
        assert_eq!(ws.preferences.audio.audio_buffer_size, 512);
        assert!(ws.cue_lists[0].auto_renumber);
        let after: Value = serde_json::from_str(&ws.to_recovery_json().unwrap()).unwrap();
        for field in [
            "cue_lists",
            "output_patches",
            "default_output_patch",
            "active_cue_list_id",
            "video_outputs",
            "input_patches",
            "fixtures",
        ] {
            assert_eq!(before[field], after[field], "{field}");
        }
        let mut registry = CueRegistry::new();
        registry.register(CueType::Audio, Box::new(AudioCueFactory));
        let path = tmp.path().join("production.inkue");
        ws.save(Some(path.clone())).unwrap();
        let reopened = Workspace::load(path, &registry).unwrap();
        assert_eq!(reopened.cue_lists[0].cues[0].serialize()["number"], "S17.5");
        assert_eq!(
            reopened.cue_lists[0].cues[0].serialize()["file_path"],
            json!(media)
        );
        assert_eq!(reopened.default_output_patch_id, Some(patch));
        assert_eq!(
            reopened.preferences.general.cue_row_height,
            CueRowHeight::Tall
        );
        assert!(reopened.preferences.general.auto_renumber_on_reorder);
        assert!(reopened.preferences.general.show_hard_stop_blackout);
    }

    #[test]
    fn malformed_or_incompatible_files_and_stale_reviews_leave_the_workspace_intact() {
        let mut doc = json!({"format":FORMAT, "version":VERSION, "source_workspace":"Reference",
            "settings":{"double_go_protection_ms":0}});
        assert!(decode_file(&doc.to_string()).is_ok());
        for invalid in [
            json!({"device_id":"other"}),
            json!({"cue_row_height":"extra"}),
            json!({"double_go_protection_ms":-1}),
            json!({}),
        ] {
            doc["settings"] = invalid;
            assert!(decode_file(&doc.to_string()).is_err());
        }
        doc["version"] = json!(3);
        assert!(decode_file(&doc.to_string())
            .err()
            .unwrap()
            .contains("versions 1 and 2"));
        assert!(decode_file(r#"{"schema_version":1,"cue_lists":[]}"#).is_err());
        let mut ws = Workspace::new("Test");
        let review = SettingsSnapshot::new(&ws);
        let changes = SettingsValues {
            confirm_before_delete: Some(true),
            ..Default::default()
        };
        ws.mark_modified();
        assert!(apply_settings(&mut ws, &review, &changes).is_err());
        let mut other = Workspace::new("Test");
        assert!(apply_settings(&mut other, &review, &changes).is_err());
        assert!(
            !ws.preferences.general.confirm_before_delete
                && !other.preferences.general.confirm_before_delete
        );
    }

    #[test]
    fn versioned_theme_transfer_is_selective_and_rejects_unknown_values() {
        let mut doc = json!({"format":FORMAT, "version":VERSION, "source_workspace":"Crew",
            "settings":{"theme":"navy"}});
        assert!(decode_file(&doc.to_string()).is_err());
        doc["version"] = json!(THEME_VERSION);
        let file = decode_file(&doc.to_string()).unwrap();
        let mut ws = Workspace::new("Show");
        ws.preferences.display.theme = "light".into();
        let prior = SettingsSnapshot::new(&ws);
        apply_settings(&mut ws, &prior, &file.settings).unwrap();
        assert_eq!(ws.preferences.display.theme, "navy");
        assert_eq!(ws.revision, prior.revision + 1);
        assert_eq!(SettingsSnapshot::new(&ws).values.cue_color_style, prior.values.cue_color_style);
        for invalid in [json!("pink"), json!(null)] {
            doc["settings"]["theme"] = invalid;
            assert!(decode_file(&doc.to_string()).is_err());
        }
    }
}
