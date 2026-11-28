//! Tauri commands for workspace save / load / new.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tauri::{Emitter, Manager, State};

use crate::{
    commands::cue_list_cmds,
    cue::types::CueType,
    machine_config,
    show::{workspace::{template_path, CollectReport}, Workspace},
    state::AppState,
};

/// Recursively collect media paths and readiness needs for Audio and Video,
/// including those nested inside groups at any depth.  The type lets the preload
/// worker surface a decode *failure* for Audio cues only (a video without an
/// audio track, or whose track fails, still plays — no operator warning).
fn collect_media_cues(
    cues: &[Box<dyn crate::cue::traits::Cue>],
    out: &mut Vec<(uuid::Uuid, PathBuf, CueType, bool)>,
) {
    for cue in cues {
        if matches!(cue.cue_type(), CueType::Audio | CueType::Video) {
            let json = cue.serialize();
            if let Some(p) = json.get("file_path").and_then(|v| v.as_str()) {
                if !p.is_empty() {
                    let probe_duration = cue.cue_type() == CueType::Video && cue.file_duration().is_none();
                    out.push((cue.id(), PathBuf::from(p), cue.cue_type(), probe_duration));
                }
            }
        }
        if let Some(children) = cue.child_cues() {
            collect_media_cues(children, out);
        }
    }
}

fn retire_workspace(state: &AppState, workspace: &mut Workspace) -> Result<(), String> {
    // QLab tells /updates subscribers the old workspace is gone.
    if crate::engine::osc_updates::has_subscribers() {
        let old_id = workspace.instance_id;
        crate::engine::osc_updates::push(&[crate::engine::osc_updates::update_workspace_disconnect(old_id)]);
    }
    state.audio_engine.stop_audition(None);
    state.agent.lock().map_err(|e| e.to_string())?.disconnect();
    let context = super::transport_cmds::workspace_context(state, workspace);
    let mut transport = crate::show::transport::Transport::new(context);
    for list in &mut workspace.cue_lists {
        transport.hard_stop_all(list).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Start the machine's default template or an empty show if blank is requested.
#[tauri::command]
pub fn new_workspace(
    owner: Option<SaveOwner>,
    blank: Option<bool>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    if !blank.unwrap_or(false) {
        let template = machine_config::load_default_workspace_template()
            .map_err(|error| default_template_error(&format!("{error:#}")))?;
        if let Some(path) = template {
            let loaded = load_workspace_template(state.inner(), &path)
                .map_err(|error| default_template_error(&error))?;
            return install_workspace_for_owner(state.inner(), &app_handle, loaded, owner.as_ref());
        }
    }

    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    check_replacement_owner(&ws, owner.as_ref())?;
    retire_workspace(&state, &mut ws)?;
    state.undo_stack.lock().map_err(|e| e.to_string())?.clear();
    state.loading_cues.lock().map_err(|e| e.to_string())?.clear();
    *ws = Workspace::new("Untitled");
    cue_list_cmds::emit_cue_lists_changed(&app_handle, &ws);
    let outputs = ws.universe_outputs.clone();
    drop(ws);
    state.output_engine.set_floating_timer_visible(false);
    // A fresh workspace has no DMX outputs — clear any from the previous show.
    state.dmx_engine.set_outputs(outputs);
    // Starting clean — retire any load-skip banner from the previous show.
    crate::health::clear("workspace-load-skips");
    let _ = app_handle.emit("preferences-applied", ());
    let _ = app_handle.emit("audio-maps-changed", ());
    let _ = app_handle.emit("output-patches-changed", ());
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// A Save or Save As is issued against the show the operator saw when starting
/// the action. Instance IDs distinguish even two opens of the same file.
#[derive(Debug, serde::Deserialize)]
pub struct SaveOwner {
    instance_id: uuid::Uuid,
    revision: u64,
    file_path: Option<PathBuf>,
}

impl SaveOwner {
    fn matches(&self, ws: &Workspace) -> bool {
        self.instance_id == ws.instance_id && self.revision == ws.revision && self.file_path == ws.file_path
    }

    fn verify(&self, ws: &Workspace) -> Result<(), String> {
        if !self.matches(ws) {
            return Err("The workspace changed after this save was requested. Review the open show and save again.".into());
        }
        Ok(())
    }
}

fn check_replacement_owner(ws: &Workspace, owner: Option<&SaveOwner>) -> Result<(), String> {
    if owner.is_some_and(|expected| !expected.matches(ws)) {
        return Err("The workspace changed before the new show could open. Review the open show and try again.".into());
    }
    Ok(())
}

fn default_template_error(error: &str) -> String {
    format!("Could not use the default workspace template. Open Workspace Launcher to choose another default or start blank.\n{error}")
}

struct CapturedSave {
    owner: SaveOwner,
    target: PathBuf,
    document: String,
}

impl CapturedSave {
    fn capture(ws: &Workspace, owner: SaveOwner, target: PathBuf) -> Result<Self, String> {
        owner.verify(ws)?;
        let document = ws.to_json(&target).map_err(|e| e.to_string())?;
        Ok(Self { owner, target, document })
    }

    fn commit(self, ws: &mut Workspace, clear_recovery: impl FnOnce()) -> Result<String, String> {
        self.owner.verify(ws)?;
        ws.commit_saved_document(self.target, &self.document).map_err(|e| e.to_string())?;
        clear_recovery();
        Ok(self.document)
    }
}

#[tauri::command(async)]
pub fn save_workspace(path: String, owner: SaveOwner, state: State<'_, AppState>) -> Result<(), String> {
    save_workspace_to(&state, PathBuf::from(path), owner)
}

/// QLab `/save` over OSC: write the open workspace to its current file.
pub(crate) fn save_current_workspace(state: &AppState) -> Result<(), String> {
    let (path, owner) = {
        let ws = state.workspace.lock().map_err(|e| e.to_string())?;
        let path = ws.file_path.clone().ok_or("Workspace has no file yet; use Save As first")?;
        let owner = SaveOwner {
            instance_id: ws.instance_id,
            revision: ws.revision,
            file_path: ws.file_path.clone(),
        };
        (path, owner)
    };
    save_workspace_to(state, path, owner)
}

pub(crate) fn save_workspace_to(state: &AppState, path: PathBuf, owner: SaveOwner) -> Result<(), String> {
    let captured = {
        let ws = state.workspace.lock().map_err(|e| e.to_string())?;
        CapturedSave::capture(&ws, owner, path)?
    };
    let _order = crate::revisions::SAVE_ORDER.lock().unwrap_or_else(|e| e.into_inner());
    let origin = crate::revisions::absolute_target(&captured.target);
    let previous = origin.as_ref().ok().map(|p| crate::revisions::previous(p));
    let saved = {
        let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
        // No edit, workspace switch, or recovery snapshot for a different
        // workspace can intervene between this check and marking this save clean.
        captured.commit(&mut ws, crate::recovery::delete)?
    };

    // Only history storage happens below. The show is already safely saved.
    let result = origin.and_then(|source| {
        let (prior, warning) = match previous.transpose() {
            Ok(prior) => (prior.flatten(), None),
            Err(error) => (None, Some(error)),
        };
        crate::revisions::live_store().record(&source, prior, saved)?;
        if let Some(error) = warning { return Err(error); }
        Ok(())
    });
    match result {
        Ok(()) => crate::health::clear("saved-revision-history"),
        Err(error) => crate::health::set(crate::health::HealthAlert::new(
            "saved-revision-history", crate::health::HealthLevel::Warning,
            format!("Workspace saved. Revision history was incomplete: {error:#}"),
        )),
    }
    Ok(())
}

fn capture_workspace_template(ws: &Workspace, owner: SaveOwner, path: &Path) -> Result<CapturedSave, String> {
    owner.verify(ws)?;
    let target = template_path(path).map_err(|error| format!("{error:#}"))?;
    let active_destination = ws.file_path.as_deref().is_some_and(|source| {
        crate::revisions::absolute_target(source).ok().as_ref() == Some(&target)
            || std::fs::canonicalize(source).ok().is_some_and(|canonical| {
                std::fs::canonicalize(&target).ok().as_ref() == Some(&canonical)
            })
    });
    if active_destination {
        return Err("Choose a separate file for this workspace template.".into());
    }
    let document = ws.to_template_json(&target).map_err(|error| format!("{error:#}"))?;
    Ok(CapturedSave { owner, target, document })
}

fn write_workspace_template(captured: CapturedSave, ws: &Workspace) -> Result<(), String> {
    captured.owner.verify(ws)?;
    crate::recovery::write_document(&captured.target, captured.document.as_bytes())
        .map_err(|error| format!("Failed to save template {}: {error}", captured.target.display()))
}

/// Export an exact workspace snapshot without retargeting or saving the current show.
#[tauri::command(async)]
pub fn save_workspace_template(path: String, owner: SaveOwner, state: State<'_, AppState>) -> Result<(), String> {
    let captured = {
        let ws = state.workspace.lock().map_err(|e| e.to_string())?;
        capture_workspace_template(&ws, owner, Path::new(&path))?
    };
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    write_workspace_template(captured, &ws)
}

fn load_workspace_template(state: &AppState, path: &Path) -> Result<Workspace, String> {
    let registry = state.registry.lock().map_err(|error| error.to_string())?;
    Workspace::load_template(path, &registry).map_err(|error| format!("{error:#}"))
}

/// Display information for the machine's selected default template file.
#[derive(serde::Serialize)]
pub struct DefaultWorkspaceTemplate {
    path: PathBuf,
    name: String,
    available: bool,
}

impl DefaultWorkspaceTemplate {
    fn from_path(path: PathBuf) -> Self {
        let name = path.file_stem().map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Workspace template".to_owned());
        let available = path.is_file();
        Self { path, name, available }
    }
}

/// Read the default choice without opening a workspace.
#[tauri::command(async)]
pub fn get_default_workspace_template() -> Result<Option<DefaultWorkspaceTemplate>, String> {
    machine_config::load_default_workspace_template()
        .map(|path| path.map(DefaultWorkspaceTemplate::from_path))
        .map_err(|error| format!("{error:#}"))
}

/// Change the local default after validating the selected template.
#[tauri::command(async)]
pub fn set_default_workspace_template(
    path: Option<String>,
    state: State<'_, AppState>,
) -> Result<Option<DefaultWorkspaceTemplate>, String> {
    let selected = if let Some(path) = path {
        let path = template_path(Path::new(&path)).map_err(|error| format!("{error:#}"))?;
        load_workspace_template(state.inner(), &path)?;
        Some(path)
    } else {
        None
    };
    machine_config::save_default_workspace_template(selected.clone())
        .map_err(|error| format!("{error:#}"))?;
    Ok(selected.map(DefaultWorkspaceTemplate::from_path))
}

/// Start a detached show from a template after rechecking the desk being replaced.
#[tauri::command]
pub fn new_workspace_from_template(
    path: String,
    owner: SaveOwner,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let loaded = load_workspace_template(state.inner(), Path::new(&path))?;
    install_workspace_for_owner(state.inner(), &app_handle, loaded, Some(&owner))
}

/// Load a workspace from the given path.
#[tauri::command]
pub fn load_workspace(
    path: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let registry = state.registry.lock().map_err(|e| e.to_string())?;
    let loaded = Workspace::load(PathBuf::from(&path), &registry)
        .map_err(|e| e.to_string())?;
    drop(registry);

    install_workspace(state.inner(), &app_handle, loaded)?;
    log::info!("Opened workspace: {path}");
    Ok(())
}

/// Open the bundled rehearsal as a fresh unsaved show. Save As and Collect
/// therefore cannot overwrite the installed example through a normal Save.
#[tauri::command]
pub fn open_operator_rehearsal(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let path = app_handle.path()
        .resolve("rehearsal/Operator rehearsal.inkue", tauri::path::BaseDirectory::Resource)
        .map_err(|e| e.to_string())?;
    let mut rehearsal = {
        let registry = state.registry.lock().map_err(|e| e.to_string())?;
        Workspace::load(path, &registry).map_err(|e| format!("{e:#}"))?
    };
    rehearsal.file_path = None;
    install_workspace(state.inner(), &app_handle, rehearsal)
}

/// Install a freshly loaded/restored workspace into the running app: store it,
/// emit cue-list + modified events, rebind DMX outputs + floating timer, and
/// kick off background media preload.  Shared by [`load_workspace`] and the
/// crash-recovery restore path.
pub(crate) fn install_workspace(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    loaded: Workspace,
) -> Result<(), String> {
    install_workspace_for_owner(state, app_handle, loaded, None)
}

fn install_workspace_for_owner(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    loaded: Workspace,
    owner: Option<&SaveOwner>,
) -> Result<(), String> {
    // Collect audio + video cue IDs + file paths before storing the workspace.
    // Scan ALL cue lists so non-active lists are also preloaded on open.
    let cues_to_preload = {
        let mut result = Vec::new();
        for cl in &loaded.cue_lists {
            collect_media_cues(&cl.cues, &mut result);
        }
        result
    };

    // Store the new workspace and apply display preferences.
    let show_floating = loaded.preferences.display.show_output_timer && loaded.preferences.display.timer_floating;
    let output_screen = loaded.preferences.display.output_screen;
    let outputs_config = loaded.outputs_config();
    let dmx_outputs = loaded.universe_outputs.clone();
    let instance = loaded.instance_id;
    let skipped = loaded.cues_skipped_on_load;
    {
        let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
        check_replacement_owner(&ws, owner)?;
        retire_workspace(state, &mut ws)?;
        state.undo_stack.lock().map_err(|e| e.to_string())?.clear();
        {
            let mut loading = state.loading_cues.lock().map_err(|e| e.to_string())?;
            loading.clear();
            for (id, _, _, _) in &cues_to_preload {
                loading.begin(*id, instance);
            }
        }
        *ws = loaded;
        // Mirror the (just-loaded) auto-renumber preference onto the cue lists.
        ws.sync_auto_renumber();
        cue_list_cmds::emit_cue_lists_changed(app_handle, &ws);
    }
    // Warn the operator when the file carried cues that could not be loaded
    // (unknown type / corrupt data) — otherwise they vanish with no signal.
    if skipped > 0 {
        crate::health::set(crate::health::HealthAlert::new(
            "workspace-load-skips",
            crate::health::HealthLevel::Warning,
            format!("{skipped} cue(s) could not be loaded (unknown type or corrupt data) and were skipped."),
        ));
    } else {
        crate::health::clear("workspace-load-skips");
    }

    state.output_engine.set_floating_timer_visible(show_floating);
    // Light the configured output screen right away (black fullscreen surface)
    // instead of waiting for the first visual GO.
    state.output_engine.sync_outputs_config(&outputs_config);
    state.output_engine.apply_output_screen_on_load(output_screen);
    state.output_engine.apply_extra_screens_on_load();
    // Bind the engine's sinks to the loaded show's universe outputs.
    state.dmx_engine.set_outputs(dmx_outputs);
    let _ = app_handle.emit("preferences-applied", ());
    let _ = app_handle.emit("audio-maps-changed", ());
    let _ = app_handle.emit("output-patches-changed", ());
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));

    // Background media preload. A show can hold dozens of audio cues (a 58-cue
    // QLab import melted the machine): spawning one decode thread per cue was a
    // thundering herd — every thread fought for the workspace lock and emitted
    // `workspace-modified`, and each of those triggers a full UI refresh.
    //
    // Three fixes: (1) dedup by file — many cues point at the same file (a Canon
    // reuses one clip across sections), so decode each distinct file once and
    // share the decoded buffer (`Arc`) across every cue that uses it; (2) drain
    // the work with a small bounded pool; (3) let one coordinator coalesce the UI
    // refresh to a few times a second instead of once per cue.
    let mut cues_by_path: HashMap<PathBuf, Vec<(uuid::Uuid, CueType, bool)>> = HashMap::new();
    for (id, path, cue_type, probe_duration) in cues_to_preload {
        cues_by_path
            .entry(path)
            .or_default()
            .push((id, cue_type, probe_duration));
    }

    let job_count = cues_by_path.len();
    if job_count > 0 {
        let (tx, rx) = crossbeam_channel::unbounded::<(PathBuf, Vec<(uuid::Uuid, CueType, bool)>)>();
        for job in cues_by_path {
            let _ = tx.send(job);
        }
        drop(tx); // close the queue so workers exit once it is drained

        let remaining = Arc::new(AtomicUsize::new(job_count));
        let dirty = Arc::new(AtomicBool::new(false));

        // Leave a core free for the audio callback + UI; cap peak decode memory.
        let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(2);
        let workers = cpus.saturating_sub(1).clamp(1, 4).min(job_count);

        for _ in 0..workers {
            let rx = rx.clone();
            let workspace = state.workspace.clone();
            let output_engine = Arc::clone(&state.output_engine);
            let loading_cues = state.loading_cues.clone();
            let dirty = Arc::clone(&dirty);
            let remaining = Arc::clone(&remaining);
            std::thread::Builder::new()
                .name("inkue-preload".into())
                .spawn(move || {
                    while let Ok((file_path, cues)) = rx.recv() {
                        let video_duration = if cues.iter().any(|(_, _, probe)| *probe) {
                            output_engine.try_mpv_lib()
                                .and_then(|lib| crate::engine::OutputEngine::probe_duration(lib, &file_path))
                        } else {
                            None
                        };
                        let decoded = crate::cue::media_decode::decode_audio_track(&file_path)
                            .map(|audio| audio.map(|(samples, channels, sample_rate)| {
                                let duration = Duration::from_secs_f64(
                                    samples.len() as f64
                                        / channels.max(1) as f64
                                        / sample_rate.max(1) as f64,
                                );
                                (Arc::new(samples), channels, sample_rate, duration)
                            }));
                        if let Err(error) = &decoded {
                            log::warn!("Preload failed for {}: {error}", file_path.display());
                        }

                        'publish: {
                            let Ok(mut ws) = workspace.lock() else { break 'publish };
                            if ws.instance_id != instance {
                                break 'publish;
                            }
                            let Ok(mut loading) = loading_cues.lock() else { break 'publish };
                            for (cue_id, cue_type, probe_duration) in &cues {
                                // A file setter or relink can replace this request
                                // even with the same path and cue ID.
                                if !loading.finish(*cue_id, instance) {
                                    continue;
                                }
                                dirty.store(true, Ordering::Relaxed);
                                let cue = ws.cue_lists.iter_mut().find_map(|list| list.get_mut_recursive(cue_id));
                                let Some(cue) = cue.filter(|cue| super::cue_file_path_still(&**cue, &file_path)) else {
                                    continue;
                                };
                                if let Some(duration) = video_duration.filter(|_| *probe_duration) {
                                    cue.set_runtime_duration(duration);
                                }
                                match &decoded {
                                    Ok(Some((samples, channels, rate, duration))) => {
                                        cue.accept_preloaded_audio(Arc::clone(samples), *channels, *rate, *duration);
                                        if *cue_type == CueType::Audio {
                                            super::clear_decode_failure(*cue_id);
                                        }
                                    }
                                    Ok(None) if *cue_type == CueType::Video =>
                                        crate::cue::video_cue::VideoCue::mark_silent_track(cue),
                                    Err(_) if *cue_type == CueType::Audio => super::surface_decode_failure(*cue_id, &file_path),
                                    _ => {}
                                }
                            }
                        }
                        remaining.fetch_sub(1, Ordering::Relaxed);
                    }
                })
                .expect("Failed to spawn preload worker");
        }

        // Coordinator: emit one coalesced `workspace-modified` at most ~4×/s while
        // preloads land, plus a final one when the batch is done. This replaces the
        // per-cue emit storm that re-rendered the whole cue list dozens of times.
        let app_handle2 = app_handle.clone();
        std::thread::Builder::new()
            .name("inkue-preload-coord".into())
            .spawn(move || loop {
                std::thread::sleep(Duration::from_millis(250));
                let done = remaining.load(Ordering::Relaxed) == 0;
                if dirty.swap(false, Ordering::Relaxed) || done {
                    let _ = app_handle2.emit("workspace-modified", serde_json::json!({}));
                }
                if done {
                    break;
                }
            })
            .expect("Failed to spawn preload coordinator");
    }

    Ok(())
}

/// Copy all media files into a self-contained project folder and write a
/// new `.inkue` file there with updated relative paths.
///
/// `target_dir` is the parent directory chosen by the user; the command
/// creates `{target_dir}/{workspace_name}/` automatically.
///
/// The workspace currently open in memory is not affected.
#[tauri::command]
pub fn collect_and_save_workspace(
    target_dir: String,
    state: State<'_, AppState>,
) -> Result<CollectReport, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    ws.collect_and_save(std::path::Path::new(&target_dir))
        .map_err(|e| e.to_string())
}

/// Return basic workspace metadata for the title bar.
#[tauri::command]
pub fn get_workspace_info(
    state: State<'_, AppState>,
) -> Result<serde_json::Value, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    Ok(serde_json::json!({
        "name": ws.metadata.name,
        "instance_id": ws.instance_id(),
        "revision": ws.revision,
        "is_modified": ws.is_modified,
        "file_path": ws.file_path.as_ref().map(|p| p.to_string_lossy().to_string()),
    }))
}

// ---------------------------------------------------------------------------
// QLab import
// ---------------------------------------------------------------------------

/// Import a QLab workspace (`.qlab4` / `.qlab5`) and make it the current show.
///
/// The imported workspace is **unsaved and has no file path**: media paths are
/// resolved against the QLab bundle folder, so the show plays straight away
/// without anything being written into the user's QLab project. Save As puts
/// it where the operator wants it; Collect and Save makes it self-contained.
#[tauri::command]
pub fn import_qlab_workspace(
    path: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<crate::qlab_import::ImportReport, String> {
    let source = PathBuf::from(&path);
    let (document, report) =
        crate::qlab_import::import_workspace(&source).map_err(|e| format!("{e:#}"))?;

    // The bundle folder is the base for the document's relative media paths.
    let base_dir = source.parent().map(|p| p.to_path_buf());
    let registry = state.registry.lock().map_err(|e| e.to_string())?;
    let mut imported = Workspace::from_json_str(&document, base_dir.as_deref(), &registry)
        .map_err(|e| format!("{e:#}"))?;
    drop(registry);

    // No file path: this is an import, not an open. The title bar shows it as
    // modified, and Save As is the next step.
    imported.file_path = None;
    imported.mark_modified();

    install_workspace(state.inner(), &app_handle, imported)?;
    Ok(report)
}

#[cfg(test)]
mod save_identity_tests {
    use super::*;
    use crate::cue::{memo_cue::MemoCue, traits::Cue};
    use std::sync::{Arc, Barrier, Mutex};

    fn owner(ws: &Workspace) -> SaveOwner {
        SaveOwner { instance_id: ws.instance_id, revision: ws.revision, file_path: ws.file_path.clone() }
    }

    fn show(name: &str, path: PathBuf) -> Workspace {
        let mut ws = Workspace::new(name);
        let mut cue = MemoCue::new();
        cue.set_name(format!("Cue for {name}"));
        ws.cue_lists[0].cues.push(Box::new(cue));
        ws.file_path = Some(path);
        ws.mark_modified();
        ws
    }

    #[test]
    fn new_show_owner_expires_after_a_reopen_or_edit() {
        let path = PathBuf::from("Show.inkue");
        let mut workspace = show("First", path.clone());
        let expected = owner(&workspace);
        assert!(check_replacement_owner(&workspace, Some(&expected)).is_ok());
        let reopened = show("First", path);
        assert!(check_replacement_owner(&reopened, Some(&expected)).is_err());
        workspace.mark_modified();
        assert!(check_replacement_owner(&workspace, Some(&expected)).is_err());
    }

    #[test]
    fn template_roundtrip_keeps_programming_and_media_but_starts_a_detached_idle_show() {
        use crate::{
            cue::{
                audio_cue::{AudioCue, AudioCueFactory},
                group_cue::{GroupCue, GroupCueFactory},
                memo_cue::MemoCueFactory,
                registry::CueRegistry,
                types::{CueState, GroupMode},
                wait_cue::{WaitCue, WaitCueFactory},
            },
            show::wall_clock::{Observation, WallClockTrigger},
        };

        let directory = tempfile::tempdir().unwrap();
        let base = directory.path().canonicalize().unwrap();
        let templates = base.join("House files");
        std::fs::create_dir(&templates).unwrap();
        let source = base.join("Source show.inkue");
        std::fs::write(&source, b"earlier saved source").unwrap();
        let starter = templates.join("Starter.inkue-template");
        let media = base.join("Stage media").join("House cue.wav");
        let local_media = templates.join("Reusable bed.wav");
        let mut original = Workspace::new("Source show");
        original.file_path = Some(source.clone());
        original.preferences.general.double_go_protection_ms = 725;
        original.programming_history.push(serde_json::json!({"status":"approved"}));
        original.mark_modified();

        let list = original.active_cue_list_mut().unwrap();
        let mut disarmed = MemoCue::new();
        disarmed.set_disabled(true);
        list.push(Box::new(disarmed));
        let mut group = GroupCue::new();
        group.mode = GroupMode::StartFirstAndEnter;
        let group_id = group.id();
        let entrance = WaitCue::new();
        let entrance_id = entrance.id();
        group.children.push(Box::new(entrance));
        let mut cue = AudioCue::new();
        cue.file_path = Some(media.clone());
        let media_id = cue.id();
        group.children.push(Box::new(cue));
        list.push(Box::new(group));
        let mut local_audio = AudioCue::new();
        local_audio.file_path = Some(local_media.clone());
        let local_media_id = local_audio.id();
        list.push(Box::new(local_audio));
        let mut active = WaitCue::new();
        let (context, _, _) = crate::test_support::recording_context();
        active.go(&context).unwrap();
        let active_id = active.id();
        list.push(Box::new(active));
        list.set_playhead(Some(media_id)).unwrap();
        list.replace_hotkey(group_id, Some("4".into())).unwrap();
        list.wall_clock_triggers.insert(active_id, WallClockTrigger {
            time: chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            weekdays: 127,
        });
        list.wall_clock_session.set_armed(active_id, true, Observation::now());
        let revision = original.revision;

        let captured = capture_workspace_template(&original, owner(&original), &starter).unwrap();
        write_workspace_template(captured, &original).unwrap();
        let bytes = std::fs::read(&starter).unwrap();
        let template: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(template["cue_lists"][0]["cues"][2]["file_path"], "Reusable bed.wav");
        assert!(template["cue_lists"][0].get("playhead_child_id").is_none());
        assert!(template["cue_lists"][0].get("playhead_cue_id").is_none());
        assert_eq!(original.file_path.as_ref(), Some(&source));
        assert_eq!(original.revision, revision);
        assert!(original.is_modified);
        assert_eq!(std::fs::read(&source).unwrap(), b"earlier saved source");
        assert_eq!(original.active_cue_list().unwrap().get(&active_id).unwrap().state(), CueState::Running);
        assert_eq!(original.active_cue_list().unwrap().enter_standby_child_id(), Some(media_id));

        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        registry.register(CueType::Wait, Box::new(WaitCueFactory));
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        registry.register(CueType::Audio, Box::new(AudioCueFactory));
        assert!(Workspace::load(starter.clone(), &registry).is_err());
        let mut next = Workspace::load_template(&starter, &registry).unwrap();
        assert_ne!(next.instance_id(), original.instance_id());
        assert_ne!(next.instance_id(), Workspace::load_template(&starter, &registry).unwrap().instance_id());
        assert!(next.file_path.is_none());
        assert!(next.is_modified);
        assert!(next.programming_history.is_empty());
        assert_eq!(next.preferences.general.double_go_protection_ms, 725);
        let list = next.active_cue_list().unwrap();
        assert_eq!(list.playhead_cue_id, Some(group_id));
        assert_eq!(list.enter_standby_child_id(), Some(entrance_id));
        assert_eq!(list.hotkeys.get(&group_id).map(String::as_str), Some("4"));
        assert!(list.wall_clock_triggers.contains_key(&active_id));
        assert!(!list.wall_clock_session.is_armed(active_id));
        assert_eq!(list.get(&active_id).unwrap().state(), CueState::Standby);
        assert_eq!(list.get_recursive(&media_id).unwrap().media_file_path(), Some(media.as_path()));
        assert_eq!(list.get(&local_media_id).unwrap().media_file_path(), Some(local_media.as_path()));
        let new_document = base.join("New show.inkue");
        next.save(Some(new_document.clone())).unwrap();
        assert!(Workspace::load(new_document, &registry).is_ok());
        assert_eq!(std::fs::read(&starter).unwrap(), bytes);
    }

    #[test]
    fn template_export_rejects_stale_owners_and_never_targets_the_open_document() {
        let directory = tempfile::tempdir().unwrap();
        let starter = directory.path().join("Starter.inkue-template");
        let source = directory.path().join("Source.inkue");
        let mut ws = show("Source", source.clone());
        std::fs::write(&starter, b"existing bytes").unwrap();

        assert!(capture_workspace_template(&ws, owner(&ws), &source).is_err());
        let old = capture_workspace_template(&ws, owner(&ws), &starter).unwrap();
        ws.mark_modified();
        assert!(write_workspace_template(old, &ws).is_err());
        assert_eq!(std::fs::read(&starter).unwrap(), b"existing bytes");

        let old = capture_workspace_template(&ws, owner(&ws), &starter).unwrap();
        let switched = show("Another open", source);
        assert!(write_workspace_template(old, &switched).is_err());
        ws.file_path = Some(starter.clone());
        assert!(capture_workspace_template(&ws, owner(&ws), &starter).is_err());
        assert_eq!(std::fs::read(&starter).unwrap(), b"existing bytes");
    }

    #[test]
    fn two_queued_saves_cannot_write_a_switched_show_or_clear_its_recovery() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("A.inkue");
        let b = dir.path().join("B.inkue");
        let recovery = dir.path().join("recovery.inkue");
        let initial = show("A", a.clone());
        let first = CapturedSave::capture(&initial, owner(&initial), a.clone()).unwrap();
        let queued = CapturedSave::capture(&initial, owner(&initial), a.clone()).unwrap();
        let active = Arc::new(Mutex::new(initial));
        first.commit(&mut active.lock().unwrap(), || {}).unwrap();
        let bytes = std::fs::read(&a).unwrap();
        let waiting = Barrier::new(2);
        let release = Barrier::new(2);
        std::thread::scope(|scope| {
            let next = scope.spawn(|| {
                waiting.wait();
                release.wait();
                queued.commit(&mut active.lock().unwrap(), || { std::fs::remove_file(&recovery).unwrap(); })
            });
            waiting.wait();
            *active.lock().unwrap() = show("B", b.clone());
            std::fs::write(&recovery, b"B unsaved edits").unwrap();
            release.wait();
            assert!(next.join().unwrap().is_err());
        });
        let ws = active.lock().unwrap();
        assert_eq!(ws.file_path.as_ref(), Some(&b));
        assert!(ws.is_modified);
        assert_eq!(std::fs::read(&a).unwrap(), bytes);
        assert!(String::from_utf8(bytes).unwrap().contains("Cue for A"));
        assert_eq!(std::fs::read(&recovery).unwrap(), b"B unsaved edits");
    }

    #[test]
    fn stale_save_as_dialog_cannot_save_another_instance_at_the_same_path() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("Identical path.inkue");
        let target = dir.path().join("Other.inkue");
        let dialog_owner = owner(&show("First open", source.clone()));
        let reopened = show("Second open", source);
        assert_eq!(reopened.revision, dialog_owner.revision);
        assert!(CapturedSave::capture(&reopened, dialog_owner, target.clone()).is_err());
        assert!(!target.exists());
    }

    #[test]
    fn subsequent_edit_or_save_as_preserves_later_state_and_destination_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("Original.inkue");
        std::fs::write(&target, b"earlier disk version").unwrap();
        let mut ws = show("A", target.clone());
        let old_edit = CapturedSave::capture(&ws, owner(&ws), target.clone()).unwrap();
        ws.mark_modified();
        assert!(old_edit.commit(&mut ws, || panic!("cleared newer recovery")).is_err());
        let old_path = CapturedSave::capture(&ws, owner(&ws), target.clone()).unwrap();
        ws.file_path = Some(dir.path().join("Renamed.inkue"));
        assert!(old_path.commit(&mut ws, || panic!("cleared current recovery")).is_err());
        assert!(ws.is_modified);
        assert_eq!(std::fs::read(target).unwrap(), b"earlier disk version");
    }
}
