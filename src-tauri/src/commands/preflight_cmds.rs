//! Preflight ("Check Workspace") + media relink commands.
//!
//! `check_workspace` walks every cue (all lists, nested groups) and returns the
//! ones whose external dependencies do not resolve — missing media file, dangling
//! Stop/Fade target, unpatched fixture, absent MIDI port, … — so the operator can
//! fix them before the show.  `relink_media` re-points a missing media file and,
//! as a convenience, re-points every other missing file found in the same folder.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Serialize;
use tauri::{Emitter, State};

use crate::{
    cue::{
        registry::CueRegistry,
        traits::Cue,
        types::{CueId, CueType},
        validation::{CueIssue, ValidationContext},
    },
    show::Workspace,
    state::AppState,
};

/// One cue's preflight result — only cues with at least one issue are returned.
#[derive(Debug, Clone, Serialize)]
pub struct CueValidation {
    pub owner: CheckOwner,
    pub cue_id: String,
    pub cue_number: Option<String>,
    pub cue_name: String,
    pub cue_type: CueType,
    pub issues: Vec<CueIssue>,
    /// The unresolved media path, when the cue's problem is a missing file.
    /// Drives the "Localiser…" relink action in the preflight panel.
    pub missing_file: Option<String>,
}

/// Result of a [`relink_media`] call.
#[derive(Debug, Clone, Serialize)]
pub struct RelinkResult {
    /// How many cues were re-pointed (the picked one plus auto-matched siblings).
    pub relinked: usize,
}

/// Names of MIDI output ports currently available on this machine.
fn available_midi_ports() -> Vec<String> {
    match midir::MidiOutput::new("Inkue-preflight") {
        Ok(out) => out
            .ports()
            .iter()
            .filter_map(|p| out.port_name(p).ok())
            .collect(),
        Err(e) => {
            log::warn!("[preflight] MIDI port enumeration failed: {e}");
            Vec::new()
        }
    }
}

/// Recursively collect every cue ID (groups + their children).
fn collect_ids(cues: &[Box<dyn Cue>], out: &mut HashSet<CueId>) {
    for c in cues {
        out.insert(c.id());
        if let Some(children) = c.child_cues() {
            collect_ids(children, out);
        }
    }
}

/// Every cue (nested included) that puts a picture on an output.
fn collect_visual_ids(
    cues: &[Box<dyn Cue>], visual: &mut HashSet<CueId>, rotations: &mut HashSet<CueId>,
) {
    for c in cues {
        if c.is_visual() {
            visual.insert(c.id());
        }
        if c.is_still_rotation_target() {
            rotations.insert(c.id());
        }
        if let Some(children) = c.child_cues() {
            collect_visual_ids(children, visual, rotations);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize, Serialize)]
pub struct CheckOwner {
    instance_id: uuid::Uuid,
    revision: u64,
    file_path: Option<PathBuf>,
}

impl CheckOwner {
    fn capture(ws: &Workspace) -> Self {
        Self {
            instance_id: ws.instance_id,
            revision: ws.revision,
            file_path: ws.file_path.clone(),
        }
    }

    fn verify(&self, ws: &Workspace) -> Result<(), String> {
        if self != &Self::capture(ws) {
            return Err(
                "The show changed during the media check. Check the workspace again.".into(),
            );
        }
        Ok(())
    }

    fn resolve(&self, path: &Path) -> PathBuf {
        self.file_path
            .as_deref()
            .and_then(Path::parent)
            .map_or_else(|| path.to_owned(), |parent| parent.join(path))
    }
}

struct CueCheck {
    row: CueValidation,
    media: Option<PathBuf>,
    files: Vec<crate::cue::validation::ValidationFile>,
    named_device: Option<String>,
    needs_main_output_for_effects: bool,
    camera_device: Option<String>,
}

fn capture_checks(
    list: &crate::show::cue_list::CueList,
    cues: &[Box<dyn Cue>],
    ctx: &ValidationContext,
    owner: &CheckOwner,
    workspace: &Workspace,
    out: &mut Vec<CueCheck>,
) {
    for cue in cues {
        let mut issues = cue.validate_programming(ctx);
        if list.flagged_cues.contains(&cue.id()) {
            issues.push(CueIssue::warning("Flagged for operator review"));
        }
        if let Some(fade) = cue.fade_specification() {
            if let (Some(destination), [target]) = (fade.effect_destination, fade.target_cue_ids.as_slice()) {
                let filter_exists = list.get_recursive(target).is_some_and(|audio| {
                    audio.accepts_effect_destination(destination)
                });
                if !filter_exists {
                    issues.push(CueIssue::error(match destination {
                        crate::cue::types::EffectDestination::Cutoff(_) => "Choose an enabled high-pass or low-pass filter on the target Audio cue",
                        crate::cue::types::EffectDestination::EqGain(_) => "Choose an enabled peaking EQ on the target Audio cue",
                    }));
                }
            }
        }
        if let Err(e) = list.target_destination(cue.as_ref()) {
            issues.push(CueIssue::error(e.to_string()));
        }
        let patch = cue.output_patch_id().or(workspace.default_output_patch_id)
            .and_then(|id| workspace.output_patches.iter().find(|p| p.id == id));
        if let Some(trim) = (cue.as_ref() as &dyn std::any::Any)
            .downcast_ref::<crate::cue::audio_cue::AudioCue>().and_then(|audio| audio.output_trim.as_ref())
        {
            if let Err(error) = trim.route(patch) {
                issues.push(CueIssue::error(error));
            }
        }
        let mut named_device = None;
        let mut needs_main_output_for_effects = false;
        if let Some(patch) = patch.filter(|p| p.require_named_device || p.has_audio_effects()) {
            match cue.cue_type() {
                CueType::Audio => match patch.named_output() {
                    Ok(route) => {
                        needs_main_output_for_effects = patch.has_audio_effects();
                        named_device = route.map(|route| route.device_id);
                    }
                    Err(message) => issues.push(CueIssue::error(message)),
                },
                cue_type if matches!(cue_type, CueType::Video | CueType::Mic)
                    || (cue_type == CueType::Timecode && cue.output_patch_id().is_some()) =>
                {
                    let setting = if patch.has_audio_effects() {
                        format!("Output Patch “{}” applies its output effects and named-device GO requirement to File Audio. This cue uses its existing dry path.", patch.name)
                    } else {
                        format!("The named-device GO requirement on Output Patch “{}” applies to Audio file cues. Check this cue’s routing separately.", patch.name)
                    };
                    issues.push(CueIssue::warning(setting));
                }
                _ => {}
            }
        }
        let media = cue
            .media_file_path()
            .filter(|path| !path.as_os_str().is_empty())
            .map(Path::to_owned);
        let files = cue.validation_files();
        #[cfg(target_os = "linux")]
        let camera_device = (cue.as_ref() as &dyn std::any::Any)
            .downcast_ref::<crate::cue::camera_cue::CameraCue>()
            .and_then(|camera| camera.source.linux_device_id())
            .map(str::to_owned);
        #[cfg(not(target_os = "linux"))]
        let camera_device = None;

        if !issues.is_empty() || media.is_some() || !files.is_empty()
            || named_device.is_some() || camera_device.is_some()
        {
            out.push(CueCheck {
                row: CueValidation {
                    owner: owner.clone(),
                    cue_id: cue.id().to_string(),
                    cue_number: cue.number().map(str::to_owned),
                    cue_name: cue.name().to_owned(),
                    cue_type: cue.cue_type(),
                    issues,
                    missing_file: None,
                },
                media,
                files,
                named_device,
                needs_main_output_for_effects,
                camera_device,
            });
        }
        if let Some(children) = cue.child_cues() {
            capture_checks(list, children, ctx, owner, workspace, out);
        }
    }
}

fn check_snapshot(ws: &Workspace, midi_ports: Vec<String>) -> (CheckOwner, Vec<CueCheck>) {
    let mut all_cue_ids = HashSet::new();
    let mut visual_cue_ids = HashSet::new();
    let mut rotation_cue_ids = HashSet::new();
    for cl in &ws.cue_lists {
        collect_ids(&cl.cues, &mut all_cue_ids);
        collect_visual_ids(&cl.cues, &mut visual_cue_ids, &mut rotation_cue_ids);
    }
    let ctx = ValidationContext {
        all_cue_ids,
        fixture_ids: ws.fixtures.iter().map(|f| f.id).collect(),
        fixture_group_ids: ws.fixture_groups.iter().map(|g| g.id).collect(),
        osc_patch_ids: ws.osc_patches.iter().map(|p| p.id).collect(),
        output_patch_ids: ws.output_patches.iter().map(|p| p.id).collect(),
        midi_ports,
        video_output_ids: ws.video_outputs.iter().map(|c| c.id).collect(),
        visual_cue_ids,
        rotation_cue_ids,
    };
    let owner = CheckOwner::capture(ws);
    let mut checks = Vec::new();
    for cl in &ws.cue_lists {
        capture_checks(cl, &cl.cues, &ctx, &owner, ws, &mut checks);
    }
    (owner, checks)
}

fn probe_checks(
    checks: Vec<CueCheck>,
    mut probe: impl FnMut(&Path) -> bool,
    devices: Option<&HashSet<String>>,
    main_output: Option<&str>,
) -> Vec<CueValidation> {
    let mut presence = std::collections::HashMap::new();
    let mut exists = |p: &Path| *presence.entry(p.to_owned()).or_insert_with(|| probe(p));
    #[cfg(target_os = "linux")]
    let mut camera_devices = std::collections::HashMap::new();
    checks
        .into_iter()
        .filter_map(|check| {
            let mut row = check.row;
            if let Some(id) = check.camera_device {
                #[cfg(target_os = "linux")]
                {
                    use crate::engine::camera_enum::{check_camera_device, is_numbered_camera};
                    match camera_devices.entry(id.clone()).or_insert_with(|| check_camera_device(&id)) {
                        Err(message) => row.issues.push(CueIssue::error(message.clone())),
                        Ok(()) if is_numbered_camera(&id) => row.issues.push(CueIssue::warning(format!(
                            "Numbered camera input {id} can change after reconnection. Confirm the feed or choose its port in the Camera inspector."
                        ))),
                        Ok(()) => {}
                    }
                }
                #[cfg(not(target_os = "linux"))]
                let _ = id;
            }
            if let Some(id) = check.named_device.as_ref() {
                if check.needs_main_output_for_effects && main_output != Some(id.as_str()) {
                    row.issues.push(CueIssue::error(format!(
                        "Select ‘{id}’ as the named main output in Audio Preferences before using this patch’s effects."
                    )));
                }
                match devices {
                    Some(available) if !available.contains(id) => row.issues.push(CueIssue::error(format!(
                        "Required output device ‘{id}’ is unavailable. Reconnect it or repair the Output Patch before GO."
                    ))),
                    None => row.issues.push(CueIssue::warning(format!(
                        "Could not verify required output device ‘{id}’. Confirm it in Preferences → Audio."
                    ))),
                    _ => {}
                }
            }
            for mut file in check.files {
                if check.media.as_ref() == Some(&file.path) {
                    file.path = row.owner.resolve(&file.path);
                }
                if let Some(issue) = file.resolve(&mut exists) {
                    row.issues.push(issue);
                }
            }
            if let Some(path) = check.media {
                if !exists(&row.owner.resolve(&path)) {
                    row.issues.insert(
                        0,
                        CueIssue::error(format!("File not found: {}", path.display())),
                    );
                    row.missing_file = Some(path.to_string_lossy().into_owned());
                }
            }
            (!row.issues.is_empty()).then_some(row)
        })
        .collect()
}

/// Validate a show snapshot against the supplied media probe, checking ownership after I/O.
pub fn check_workspace_with_probe(
    workspace: &std::sync::Mutex<Workspace>,
    midi_ports: Vec<String>,
    probe: impl FnMut(&Path) -> bool,
) -> Result<Vec<CueValidation>, String> {
    check_workspace_with_probes(workspace, midi_ports, probe, None, |_| None)
}

fn check_workspace_with_probes(
    workspace: &std::sync::Mutex<Workspace>,
    midi_ports: Vec<String>,
    probe: impl FnMut(&Path) -> bool,
    main_output: Option<&str>,
    named_outputs: impl FnOnce(HashSet<String>) -> Option<HashSet<String>>,
) -> Result<Vec<CueValidation>, String> {
    let (owner, checks) = {
        let ws = workspace.lock().map_err(|e| e.to_string())?;
        check_snapshot(&ws, midi_ports)
    };
    let ids: HashSet<_> = checks.iter().filter_map(|check| check.named_device.clone()).collect();
    let devices = if ids.is_empty() { None } else { named_outputs(ids) };
    let results = probe_checks(checks, probe, devices.as_ref(), main_output);
    let ws = workspace.lock().map_err(|e| e.to_string())?;
    owner.verify(&ws)?;
    Ok(results)
}

#[tauri::command]
pub async fn check_workspace(state: State<'_, AppState>) -> Result<Vec<CueValidation>, String> {
    let workspace = state.workspace.clone();
    let audio = state.audio_engine.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let main = audio.named_main_device();
        check_workspace_with_probes(&workspace, available_midi_ports(), Path::exists, main.as_deref(), |ids| {
            use crate::engine::{audio_host, device_manager};
            let main = main.clone();
            device_manager::run_bounded(device_manager::ENUM_TIMEOUT, move || {
                ids.into_iter().filter(|id| {
                    main.as_ref() == Some(id) || audio_host::output_device(Some(id)).is_ok_and(|resolved| {
                        resolved.resolved_named_id(Some(id)).is_some()
                    })
                }).collect()
            })
        })
    })
    .await
    .map_err(|e| e.to_string())?
}

struct RelinkSource {
    list_id: uuid::Uuid,
    id: CueId,
    path: Option<PathBuf>,
    json: serde_json::Value,
}

fn collect_sources(
    list_id: uuid::Uuid,
    cues: &[Box<dyn Cue>],
    selected: CueId,
    sources: &mut Vec<RelinkSource>,
) {
    for cue in cues {
        let path = cue.media_file_path().filter(|p| !p.as_os_str().is_empty());
        if matches!(
            cue.cue_type(),
            CueType::Audio | CueType::Image | CueType::Video | CueType::MidiFile
        ) && (cue.id() == selected || path.is_some())
        {
            sources.push(RelinkSource {
                list_id,
                id: cue.id(),
                path: path.map(Path::to_owned),
                json: cue.serialize(),
            });
        }
        if let Some(children) = cue.child_cues() {
            collect_sources(list_id, children, selected, sources);
        }
    }
}

struct Replacement {
    original: serde_json::Value,
    list_id: uuid::Uuid,
    id: CueId,
    path: PathBuf,
    cue: Box<dyn Cue>,
}

fn prepare_relink(
    owner: &CheckOwner,
    sources: Vec<RelinkSource>,
    selected: CueId,
    new_path: PathBuf,
    registry: &CueRegistry,
) -> Result<Vec<Replacement>, String> {
    if !new_path.is_file() {
        return Err("The selected media file cannot be found.".into());
    }
    let mut replacements = Vec::new();
    for source in sources {
        let path = if source.id == selected {
            new_path.clone()
        } else {
            let Some(old) = source.path.as_deref() else {
                continue;
            };
            if owner.resolve(old).exists() {
                continue;
            }
            let Some(candidate) = new_path
                .parent()
                .zip(old.file_name())
                .map(|(dir, file)| dir.join(file))
            else {
                continue;
            };
            if !candidate.is_file() {
                continue;
            }
            candidate
        };
        let mut json = source.json.clone();
        json["file_path"] = serde_json::to_value(&path).map_err(|e| e.to_string())?;
        // MIDI file factories read and parse their file here. Neither the workspace
        // nor the shared factory registry is locked during that operation.
        let cue = registry.from_json(json).map_err(|e| e.to_string())?;
        replacements.push(Replacement {
            original: source.json,
            list_id: source.list_id,
            id: source.id,
            path,
            cue,
        });
    }
    if !replacements.iter().any(|entry| entry.id == selected) {
        return Err("Media cue not found".into());
    }
    Ok(replacements)
}

fn apply_relink(
    ws: &mut Workspace,
    owner: &CheckOwner,
    replacements: Vec<Replacement>,
    undo: &mut crate::show::undo_stack::UndoHistory,
) -> Result<Vec<(CueId, CueType, PathBuf)>, String> {
    owner.verify(ws)?;
    let mut touched = HashSet::new();
    for entry in &replacements {
        let cue = ws
            .cue_lists
            .iter()
            .find(|list| list.id == entry.list_id)
            .and_then(|list| list.get_recursive(&entry.id))
            .ok_or("Media cue changed during relink")?;
        if cue.serialize() != entry.original {
            return Err(format!(
                "Cue {} changed during relink. Check the workspace again.",
                cue.name()
            ));
        }
        if cue.is_running() || cue.is_paused() || cue.playing_voice_id().is_some() {
            return Err(format!(
                "Stop cue {} before relinking its media.",
                cue.name()
            ));
        }
        touched.insert(entry.list_id);
    }
    for list in ws
        .cue_lists
        .iter()
        .filter(|list| touched.contains(&list.id))
    {
        undo.push_action(list.id, super::undo_cmds::take_snapshot(list));
    }
    let mut preloads = Vec::with_capacity(replacements.len());
    for entry in replacements {
        let list = ws
            .cue_lists
            .iter_mut()
            .find(|list| list.id == entry.list_id)
            .expect("list verified above");
        preloads.push((entry.id, entry.cue.cue_type(), entry.path));
        list.replace_cue_recursive(&entry.id, entry.cue);
    }
    ws.mark_modified();
    Ok(preloads)
}

#[tauri::command]
pub async fn relink_media(
    cue_id: CueId,
    new_path: PathBuf,
    owner: CheckOwner,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<RelinkResult, String> {
    let registry = state.registry.lock().map_err(|e| e.to_string())?.clone();
    let sources = {
        let ws = state.workspace.lock().map_err(|e| e.to_string())?;
        owner.verify(&ws)?;
        let mut sources = Vec::new();
        for list in &ws.cue_lists {
            collect_sources(list.id, &list.cues, cue_id, &mut sources);
        }
        sources
    };
    let captured = owner.clone();
    let replacements = tauri::async_runtime::spawn_blocking(move || {
        prepare_relink(&captured, sources, cue_id, new_path, &registry)
    })
    .await
    .map_err(|e| e.to_string())??;
    let preloads = {
        let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
        let mut undo = state.undo_stack.lock().map_err(|e| e.to_string())?;
        apply_relink(&mut ws, &owner, replacements, &mut undo)?
    };
    let relinked = preloads.len();
    for (id, cue_type, path) in preloads {
        if matches!(cue_type, CueType::Audio | CueType::Video) {
            spawn_media_preload(
                state.inner(),
                &app_handle,
                owner.instance_id,
                id,
                cue_type,
                path,
            );
        }
    }
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(RelinkResult { relinked })
}

/// Background preload after a relink: probe video duration + decode the audio
/// track so the re-pointed cue is ready to play.  Image cues need nothing.
pub(crate) fn spawn_media_preload(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    instance: uuid::Uuid,
    cue_id: CueId,
    cue_type: CueType,
    path: PathBuf,
) {
    if matches!(cue_type, CueType::Image) {
        return;
    }
    let request_id = uuid::Uuid::new_v4();
    {
        let Ok(ws) = state.workspace.lock() else {
            return;
        };
        let current = ws
            .cue_lists
            .iter()
            .find_map(|list| list.get_recursive(&cue_id));
        if ws.instance_id != instance
            || !current.is_some_and(|cue| super::cue_file_path_still(cue, &path))
        {
            return;
        }
        let Ok(mut loading) = state.loading_cues.lock() else { return };
        loading.begin(cue_id, request_id);
    }
    let workspace = state.workspace.clone();
    let loading_cues = state.loading_cues.clone();
    let output_engine = Arc::clone(&state.output_engine);
    let app_handle2 = app_handle.clone();
    let is_video = matches!(cue_type, CueType::Video);

    let worker = std::thread::Builder::new()
        .name("inkue-relink-preload".into())
        .spawn(move || {
            let duration = if is_video {
                output_engine
                    .try_mpv_lib()
                    .and_then(|lib| crate::engine::OutputEngine::probe_duration(lib, &path))
            } else {
                None
            };
            let audio = crate::cue::media_decode::decode_audio_track(&path);

            if let Ok(mut ws) = workspace.lock() {
                if ws.instance_id != instance {
                    return;
                }
                let Ok(mut loading) = loading_cues.lock() else { return };
                if !loading.finish(cue_id, request_id) {
                    return;
                }
                let cue = ws.cue_lists.iter_mut().find_map(|list| list.get_mut_recursive(&cue_id));
                if let Some(cue) = cue.filter(|cue| super::cue_file_path_still(&**cue, &path)) {
                    if let Some(duration) = duration {
                        cue.set_runtime_duration(duration);
                    }
                    if let Ok(Some((samples, channels, sample_rate))) = audio {
                        let duration = std::time::Duration::from_secs_f64(
                            samples.len() as f64
                                / channels.max(1) as f64
                                / sample_rate.max(1) as f64,
                        );
                        cue.accept_preloaded_audio(Arc::new(samples), channels, sample_rate, duration);
                    } else if is_video && matches!(audio, Ok(None)) {
                        crate::cue::video_cue::VideoCue::mark_silent_track(cue);
                    }
                }
            }
            let _ = app_handle2.emit("workspace-modified", serde_json::json!({}));
        });
    if let Err(error) = worker {
        log::warn!("Media preload worker could not start for {cue_id}: {error}");
        let Ok(mut loading) = state.loading_cues.lock() else { return };
        if loading.finish(cue_id, request_id) {
            drop(loading);
            let _ = app_handle.emit("workspace-modified", ());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cue::audio_cue::{AudioCue, AudioCueFactory},
        show::undo_stack::{HistoryDirection, UndoHistory},
    };

    fn with_file(path: PathBuf) -> AudioCue {
        let mut cue = AudioCue::new();
        cue.file_path = Some(path);
        cue
    }

    #[test]
    fn flagged_cues_are_reported_as_warnings_during_workspace_check() {
        let mut workspace = Workspace::new("Review");
        let list = workspace.active_cue_list_mut().unwrap();
        let cue = crate::cue::memo_cue::MemoCue::new();
        let id = cue.id();
        list.push(Box::new(cue));
        list.flagged_cues.insert(id);
        let checks = check_workspace_with_probe(&std::sync::Mutex::new(workspace), Vec::new(), |_| true).unwrap();
        assert_eq!(checks.len(), 1);
        assert_eq!(checks[0].cue_id, id.to_string());
        assert_eq!(checks[0].issues[0].severity, crate::cue::validation::Severity::Warning);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn workspace_check_reports_camera_input_separately_from_missing_media() {
        use crate::cue::{
            camera_cue::{CameraCue, CameraSource},
            validation::Severity,
        };
        let fixture = tempfile::tempdir().unwrap();
        let mut ws = Workspace::new("camera rehearsal");
        let list = ws.active_cue_list_mut().unwrap();
        let mut disconnected = CameraCue::new();
        disconnected.source = CameraSource::Device {
            id: fixture.path().join("missing-port").to_str().unwrap().to_owned(),
            name: "Auditorium".into(),
        };
        let disconnected_id = disconnected.id().to_string();
        let blank = CameraCue::new();
        let blank_id = blank.id().to_string();
        let mut network = CameraCue::new();
        network.source = CameraSource::Url { url: "rtsp://camera.invalid/live".into() };
        list.push(Box::new(disconnected));
        list.push(Box::new(blank));
        list.push(Box::new(network));

        let checks = check_workspace_with_probe(&std::sync::Mutex::new(ws), Vec::new(), |_| {
            panic!("no camera input should enter the media relink probe")
        }).unwrap();
        assert_eq!(checks.len(), 2);
        let disconnected = checks.iter().find(|row| row.cue_id == disconnected_id).unwrap();
        assert_eq!(disconnected.issues[0].severity, Severity::Error);
        assert!(disconnected.issues[0].message.contains("is missing"));
        assert!(disconnected.missing_file.is_none());
        let blank = checks.iter().find(|row| row.cue_id == blank_id).unwrap();
        assert_eq!(blank.issues[0].severity, Severity::Warning);
    }

    #[test]
    fn strict_default_patch_reports_missing_device_for_audio_and_scope_for_video() {
        use crate::{cue::video_cue::VideoCue, engine::device_manager::OutputPatch};
        let mut ws = Workspace::new("venue");
        let mut patch = OutputPatch::new("Backstage", "hw:CARD=Stage", vec![0, 1]);
        patch.require_named_device = true;
        ws.default_output_patch_id = Some(patch.id);
        ws.output_patches.push(patch);
        let audio = with_file("line.wav".into());
        let video = VideoCue::new();
        let audio_id = audio.id().to_string();
        let video_id = video.id().to_string();
        let list = ws.active_cue_list_mut().unwrap();
        list.push(Box::new(audio));
        list.push(Box::new(video));
        let ws = std::sync::Mutex::new(ws);

        let checks = check_workspace_with_probes(&ws, Vec::new(), |_| true, None, |ids| {
            assert_eq!(ids, HashSet::from(["hw:CARD=Stage".to_owned()]));
            Some(HashSet::new())
        }).unwrap();
        assert!(checks.iter().find(|row| row.cue_id == audio_id).unwrap().issues.iter()
            .any(|issue| issue.message.contains("Required output device")));
        assert!(checks.iter().find(|row| row.cue_id == video_id).unwrap().issues.iter()
            .any(|issue| issue.message.contains("applies to Audio file cues")));

        let checks = check_workspace_with_probes(&ws, Vec::new(), |_| true, None, Some).unwrap();
        assert!(!checks.iter().any(|row| row.cue_id == audio_id));
    }

    #[test]
    fn patch_effects_require_the_exact_main_device_even_if_an_aux_device_is_available() {
        use crate::engine::{cue_effects::CueEffect, device_manager::OutputPatch};
        let mut ws = Workspace::new("venue");
        let mut patch = OutputPatch::new("Backstage", "hw:CARD=Stage", vec![0, 1]);
        patch.require_named_device = true;
        patch.audio_effects.push(CueEffect::HighPass {
            id: uuid::Uuid::new_v4(), enabled: true, frequency_hz: 600.0,
        });
        ws.default_output_patch_id = Some(patch.id);
        ws.output_patches.push(patch);
        let audio = with_file("line.wav".into());
        let audio_id = audio.id().to_string();
        ws.active_cue_list_mut().unwrap().push(Box::new(audio));
        let ws = std::sync::Mutex::new(ws);

        let checks = check_workspace_with_probes(&ws, Vec::new(), |_| true, Some("hw:CARD=House"), Some).unwrap();
        assert!(checks.iter().find(|row| row.cue_id == audio_id).unwrap().issues.iter()
            .any(|issue| issue.message.contains("named main output")));
        let checks = check_workspace_with_probes(&ws, Vec::new(), |_| true, Some("hw:CARD=Stage"), Some).unwrap();
        assert!(!checks.iter().any(|row| row.cue_id == audio_id));
    }

    #[test]
    fn relink_respects_instance_and_revision_and_restores_each_lists_undo() {
        let temp = std::env::temp_dir().join(uuid::Uuid::new_v4().to_string());
        std::fs::create_dir(&temp).unwrap();
        for file in ["chosen.wav", "sibling.wav"] {
            std::fs::write(temp.join(file), b"test").unwrap();
        }
        let mut ws = Workspace::new("venue");
        ws.file_path = Some(temp.join("show.inkue"));
        let cue = with_file("gone/file.wav".into());
        let id = cue.id();
        ws.active_cue_list_mut().unwrap().push(Box::new(cue));
        let mut other = crate::show::cue_list::CueList::new("other");
        let sibling = with_file("gone/sibling.wav".into());
        let sibling_id = sibling.id();
        other.push(Box::new(sibling));
        ws.cue_lists.push(other);
        let mut registry = CueRegistry::new();
        registry.register(CueType::Audio, Box::new(AudioCueFactory));
        let prepare = |ws: &Workspace, owner: &CheckOwner| {
            let mut sources = Vec::new();
            for list in &ws.cue_lists {
                collect_sources(list.id, &list.cues, id, &mut sources);
            }
            prepare_relink(owner, sources, id, temp.join("chosen.wav"), &registry).unwrap()
        };
        let owner = CheckOwner::capture(&ws);
        let old = prepare(&ws, &owner);
        let mut history = UndoHistory::new();
        let mut other_show = Workspace::new("venue");
        assert!(apply_relink(&mut other_show, &owner, old, &mut history).is_err());
        let old = prepare(&ws, &owner);
        ws.mark_modified();
        assert!(apply_relink(&mut ws, &owner, old, &mut history).is_err());
        assert!(history
            .candidate(ws.active_cue_list_id, HistoryDirection::Undo)
            .is_none());
        let current = CheckOwner::capture(&ws);
        let replacements = prepare(&ws, &current);
        ws.cue_lists[1]
            .get_mut_recursive(&sibling_id)
            .unwrap()
            .set_disabled(true);
        assert!(apply_relink(&mut ws, &current, replacements, &mut history).is_err());
        let replacements = prepare(&ws, &current);
        assert_eq!(
            apply_relink(&mut ws, &current, replacements, &mut history)
                .unwrap()
                .len(),
            2
        );
        for list in &ws.cue_lists {
            assert!(history.candidate(list.id, HistoryDirection::Undo).is_some());
        }
        assert_eq!(
            ws.cue_lists[1]
                .get_recursive(&sibling_id)
                .unwrap()
                .media_file_path(),
            Some(temp.join("sibling.wav").as_path())
        );
        std::fs::remove_dir_all(temp).unwrap();
    }
}
