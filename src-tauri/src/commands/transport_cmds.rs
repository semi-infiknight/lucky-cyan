//! Tauri commands for transport control (GO, STOP, PAUSE, RESUME).

use std::sync::atomic::Ordering;

use tauri::{Emitter, State};

use crate::{
    cue::{
        audio_cue::AudioCue,
        context::{CueContext, CueEvent},
        traits::Cue,
        types::{CueId, CueState, CueType},
    },
    show::{cue_list::CueList, transport::Transport, undo_stack::UndoHistory, Workspace},
    state::AppState,
};


// ---------------------------------------------------------------------------
// Shared helper
// ---------------------------------------------------------------------------

/// The caller owns the workspace lock. Build the routing snapshot from that
/// same workspace for keyboard GO, cart GO, resume and seek.
pub(super) fn workspace_context(state: &AppState, ws: &Workspace) -> CueContext {
    let (tx, _rx) = crossbeam_channel::unbounded::<CueEvent>();
    let mut context = CueContext::new(
        state.audio_engine.clone(),
        state.output_engine.clone(),
        tx,
        ws.preferences.audio.default_fade_out_ms,
        ws.output_patches.clone(),
        ws.default_output_patch_id,
        ws.osc_patches.clone(),
        state.dmx_engine.clone(),
        ws.fixtures.clone(),
        ws.fixture_groups.clone(),
        ws.input_patches.clone(),
        ws.preferences.audio.audio_buffer_size,
    );
    context.set_workspace(ws);
    context
}

/// Used while removing a cue that already holds a mutable borrow into its list.
/// Stopping only needs the engines and the operator's stop fade.
pub(super) fn stop_context(state: &AppState, stop_fade_ms: u32) -> CueContext {
    let (tx, _) = crossbeam_channel::unbounded();
    CueContext::new(
        state.audio_engine.clone(), state.output_engine.clone(), tx, stop_fade_ms,
        vec![], None, vec![], state.dmx_engine.clone(), vec![], vec![], vec![], 256,
    )
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Trigger the cue at the Playhead.
///
/// Audio files must already be decoded (pre-loaded) before GO is called.
/// Decoding is triggered automatically when a workspace is loaded or when a
/// file is assigned to a cue.  GO never decodes — if an Audio Cue is still
/// loading it is silently skipped so the command always returns instantly.
/// Video Cues stream directly from disk and are never skipped.
#[tauri::command]
pub fn go(state: State<'_, AppState>, app_handle: tauri::AppHandle) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    go_locked(&state, &app_handle, &mut ws).map(|_| ())
}

fn accept_chained_go(state: &AppState, protection_ms: u32) -> bool {
    // Double-GO protection: silently ignore a second GO that arrives within
    // `double_go_protection_ms` of the previous one (default 500 ms).
    // This catches duplicate UDP packets from OSC controllers and accidental
    // rapid double-presses without affecting intentional fast GOs.
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;

    if protection_ms > 0 {
        let last = state.last_go_at.load(Ordering::Relaxed);
        if now_ms.saturating_sub(last) < u64::from(protection_ms) {
            return false;
        }
    }
    state.last_go_at.store(now_ms, Ordering::Relaxed);
    true
}

/// Returns false if the desk suppressed GO (debounce, loading, or empty list).
pub(super) fn go_locked(state: &AppState, app_handle: &tauri::AppHandle, ws: &mut Workspace) -> Result<bool, String> {
    if ws.active_cue_list().is_some_and(|cl| cl.independent_pads())
        || !accept_chained_go(state, ws.preferences.general.double_go_protection_ms)
    {
        return Ok(false);
    }

    let context = workspace_context(state, ws);
    let mut transport = Transport::new(context);

    let cue_list = ws.active_cue_list_mut().ok_or("No active cue list")?;

    // If the Audio Cue at the playhead has no decoded samples yet (still
    // loading), skip silently.  Video Cues are exempt — they stream directly
    // from disk and need no pre-loading step.
    // NOTE: use file_duration() (raw decoded length) rather than duration()
    // to avoid blocking infinite-loop cues (loop_count = u32::MAX makes
    // duration() return None even when the file is fully decoded).
    if let Some(cue) = cue_list.playhead_cue() {
        if cue.cue_type() == CueType::Audio
            && cue.file_duration().is_none()
            && cue
                .serialize()
                .get("file_path")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false)
        {
            return Ok(false);
        }
    }

    let result = transport.go(cue_list).map_err(|e| e.to_string())?;

    // Emit state changes for cues stopped by a Stop Cue action.
    for id in &result.stopped {
        let _ = app_handle.emit(
            "cue-state-changed",
            serde_json::json!({
                "cue_id": id,
                "old_state": "running",
                "new_state": "standby",
            }),
        );
    }

    // Emit state changes for chained cues (skip the primary — the frontend
    // already shows it as triggered via playhead-moved + cue-list-refresh).
    for &id in result.triggered.iter().skip(1) {
        let _ = app_handle.emit(
            "cue-state-changed",
            serde_json::json!({
                "cue_id": id,
                "old_state": "standby",
                "new_state": "running",
            }),
        );
    }

    // Always emit — even when playhead_cue_id is None (cue was last in list).
    let _ = app_handle.emit("playhead-moved", serde_json::json!({
        "cue_id": cue_list.playhead_cue_id.map(|u| u.to_string())
    }));
    // Refresh the cue list so the frontend sees updated group inner-playhead state.
    let _ = app_handle.emit("cue-list-refresh", serde_json::json!({}));
    Ok(!result.triggered.is_empty())
}

/// Trigger a specific cue by ID.
///
/// Independent carts run the requested pad. Chained lists use their playhead.
/// The same loading guard as
/// `go` applies to Audio Cues whose file is still being decoded.
#[tauri::command]
pub fn go_cue(
    cue_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let id: uuid::Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let mut transport = Transport::new(workspace_context(&state, &ws));
    let cue_list = ws.active_cue_list_mut().ok_or("No active cue list")?;

    go_cue_in_list(&mut transport, cue_list, id, false, &app_handle)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkCueAction {
    Go,
    Start,
    Select,
    Stop,
    SetPause(NetworkPauseAction),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkPauseAction {
    Pause,
    Resume,
    Toggle,
}

/// Resolve and apply in the same workspace lock. A network cue number never
/// crosses the WebView queue to be resolved against a different show or list.
pub(crate) fn address_numbered_network_cue(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    number: &str,
    action: NetworkCueAction,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let Some(list) = ws.active_cue_list() else { return Ok(()); };
    let Some(id) = numbered_network_target(list, number) else { return Ok(()); };
    if action == NetworkCueAction::Go && !list.independent_pads()
        && !accept_chained_go(state, ws.preferences.general.double_go_protection_ms)
    {
        return Ok(());
    }
    let mut transport = Transport::new(workspace_context(state, &ws));
    let cue_list = ws.active_cue_list_mut().ok_or("No active cue list")?;

    match action {
        NetworkCueAction::Go | NetworkCueAction::Start => {
            go_cue_in_list(&mut transport, cue_list, id, action == NetworkCueAction::Start, app_handle)
        }
        NetworkCueAction::Select => super::cue_cmds::set_playhead_in_list(cue_list, Some(id), app_handle),
        NetworkCueAction::Stop => stop_cue_in_list(&mut transport, cue_list, id, app_handle),
        NetworkCueAction::SetPause(action) => network_pause_in_list(&transport.context, cue_list, Some(id), action, app_handle),
    }
}

fn numbered_network_target(list: &CueList, number: &str) -> Option<CueId> {
    list.unique_numbered_cue(number).map(|cue| cue.id())
}

pub(crate) fn control_network_list_pause(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    action: NetworkPauseAction,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let context = workspace_context(state, &ws);
    let Some(list) = ws.active_cue_list_mut() else { return Ok(()); };
    network_pause_in_list(&context, list, None, action, app_handle)
}

fn network_pause_in_list(
    context: &CueContext,
    list: &mut CueList,
    target: Option<CueId>,
    action: NetworkPauseAction,
    app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    let mut group_changed = false;
    let result = apply_network_pause(context, list, target, action, |cue, previous| {
        let _ = app_handle.emit("cue-state-changed", serde_json::json!({
            "cue_id": cue.id(),
            "old_state": previous,
            "new_state": cue.state(),
        }));
        group_changed |= cue.child_cues().is_some();
    });
    if group_changed {
        let _ = app_handle.emit("cue-list-refresh", ());
    }
    result
}

fn apply_network_pause(
    context: &CueContext,
    list: &mut CueList,
    target: Option<CueId>,
    action: NetworkPauseAction,
    mut changed: impl FnMut(&dyn Cue, CueState),
) -> Result<(), String> {
    let expected = match action {
        NetworkPauseAction::Pause => CueState::Running,
        NetworkPauseAction::Resume => CueState::Paused,
        NetworkPauseAction::Toggle if list.cues.iter().any(|cue| cue.is_running()) => CueState::Running,
        NetworkPauseAction::Toggle => CueState::Paused,
    };
    let mut first_error = None;
    for cue in &mut list.cues {
        if cue.state() != expected || target.is_some_and(|id| cue.id() != id) {
            continue;
        }
        let result = if expected == CueState::Running { cue.pause(context) } else { cue.resume(context) };
        if cue.state() != expected {
            changed(cue.as_ref(), expected);
        }
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    }
    first_error.map_or(Ok(()), |error| Err(error.to_string()))
}

/// A queued keypress must still belong to the visible show, list and binding.
#[tauri::command]
pub fn fire_cue_hotkey(
    workspace_id: uuid::Uuid, list_id: uuid::Uuid, cue_id: uuid::Uuid, key: String,
    window: tauri::WebviewWindow, app_handle: tauri::AppHandle, state: State<'_, AppState>,
) -> Result<(), String> {
    if window.label() != "main" { return Ok(()); }
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if ws.instance_id != workspace_id || ws.active_cue_list_id != list_id { return Ok(()); }
    let list = ws.active_cue_list().ok_or("No active cue list")?;
    if list.hotkeys.get(&cue_id) != Some(&key) || !list.hotkey_eligible(cue_id) { return Ok(()); }
    let mut transport = Transport::new(workspace_context(&state, &ws));
    go_cue_in_list(&mut transport, ws.active_cue_list_mut().ok_or("No active cue list")?, cue_id, false, &app_handle)
}

fn edit_cue_hotkey(
    ws: &mut Workspace, history: &mut UndoHistory,
    workspace_id: uuid::Uuid, list_id: uuid::Uuid, cue_id: CueId,
    expected_key: Option<String>, key: Option<String>,
) -> Result<bool, String> {
    if ws.instance_id != workspace_id || ws.active_cue_list_id != list_id {
        return Err("The show or cue list changed. Reopen the cue inspector.".into());
    }
    let list = ws.active_cue_list_mut().ok_or("No active cue list")?;
    if list.hotkeys.get(&cue_id) != expected_key.as_ref() {
        return Err("The keyboard trigger changed. Check its current assignment and try again.".into());
    }
    if expected_key == key {
        list.check_hotkey(cue_id, key.as_deref()).map_err(|error| error.to_string())?;
        return Ok(false);
    }
    let previous = list.replace_hotkey(cue_id, key).map_err(|error| error.to_string())?;
    history.push_hotkey_edit(list_id, cue_id, previous);
    ws.mark_modified();
    Ok(true)
}

/// Assign a key against the cue's current binding and the displayed owner.
#[tauri::command]
pub fn set_cue_hotkey(
    workspace_id: uuid::Uuid, list_id: uuid::Uuid, cue_id: uuid::Uuid,
    expected_key: Option<String>, key: Option<String>,
    state: State<'_, AppState>, handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    let changed = edit_cue_hotkey(&mut ws, &mut history, workspace_id, list_id, cue_id, expected_key, key)?;
    drop(history);
    drop(ws);
    if changed {
        let _ = handle.emit("workspace-modified", ());
        let _ = handle.emit("cue-list-refresh", ());
    }
    Ok(())
}

/// Save or remove a schedule. Editing it withdraws the session's authorization.
#[tauri::command]
pub fn set_wall_clock_trigger(
    workspace_id: uuid::Uuid,
    list_id: uuid::Uuid,
    cue_id: uuid::Uuid,
    trigger: Option<crate::show::wall_clock::WallClockTrigger>,
    state: State<'_, AppState>,
    handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if ws.instance_id != workspace_id || ws.active_cue_list_id != list_id {
        return Err("Active show or list changed".into());
    }
    let list = ws.active_cue_list_mut().ok_or("No active cue list")?;
    if list.get_recursive(&cue_id).is_none() {
        return Err("Cue no longer exists".into());
    }
    if let Some(trigger) = trigger {
        trigger.validate().map_err(|error| error.to_string())?;
        if !list.can_start_independently(cue_id) {
            return Err("Use a top-level cue that runs independently of the playhead".into());
        }
    }
    if list.wall_clock_triggers.get(&cue_id) == trigger.as_ref() {
        return Ok(());
    }
    state.undo_stack.lock().map_err(|e| e.to_string())?
        .push_action(list_id, super::undo_cmds::take_snapshot(list));
    if let Some(trigger) = trigger {
        list.wall_clock_triggers.insert(cue_id, trigger);
    } else {
        list.wall_clock_triggers.remove(&cue_id);
    }
    list.wall_clock_session.set_armed(cue_id, false, crate::show::wall_clock::Observation::now());
    ws.mark_modified();
    let _ = handle.emit("workspace-modified", ());
    let _ = handle.emit("cue-list-refresh", ());
    Ok(())
}

/// Require a local desk action after opening or recovering a show.
#[tauri::command]
pub fn arm_wall_clock_trigger(
    workspace_id: uuid::Uuid,
    list_id: uuid::Uuid,
    cue_id: uuid::Uuid,
    armed: bool,
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
    handle: tauri::AppHandle,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Arm wall clock cues from the main desk".into());
    }
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if ws.instance_id != workspace_id || ws.active_cue_list_id != list_id {
        return Err("Active show or list changed".into());
    }
    let list = ws.active_cue_list_mut().ok_or("No active cue list")?;
    if armed && (!list.wall_clock_triggers.contains_key(&cue_id) || !list.can_start_independently(cue_id)
        || !list.get(&cue_id).is_some_and(|cue| !cue.is_disabled()))
    {
        return Err("Save a schedule and arm a supported cue before enabling its wall clock trigger".into());
    }
    list.wall_clock_session.set_armed(cue_id, armed, crate::show::wall_clock::Observation::now());
    let _ = handle.emit("cue-list-refresh", ());
    Ok(())
}

/// Used by the main programming desk and windows bound to a specific independent cart.
pub(super) fn go_cue_in_list(
    transport: &mut Transport, cue_list: &mut crate::show::cue_list::CueList,
    id: uuid::Uuid, preserve_playhead: bool, app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    if let Some(cue) = cue_list.get_recursive(&id) {
        if cue.cue_type() == CueType::Audio
            && cue.file_duration().is_none()
            && cue
                .serialize()
                .get("file_path")
                .and_then(|v| v.as_str())
                .map(|s| !s.is_empty())
                .unwrap_or(false)
        {
            return Ok(());
        }
    }

    let result = if preserve_playhead {
        transport.start_independent(cue_list, id)
    } else {
        transport.go_by_id(cue_list, &id)
    }.map_err(|e| e.to_string())?;

    for stopped_id in &result.stopped {
        let _ = app_handle.emit(
            "cue-state-changed",
            serde_json::json!({
                "cue_id": stopped_id,
                "old_state": "running",
                "new_state": "standby",
            }),
        );
    }
    for &triggered_id in result.triggered.iter().skip(usize::from(!preserve_playhead)) {
        let _ = app_handle.emit(
            "cue-state-changed",
            serde_json::json!({
                "cue_id": triggered_id,
                "old_state": "standby",
                "new_state": "running",
            }),
        );
    }
    if !preserve_playhead && !cue_list.independent_pads() {
        let _ = app_handle.emit("playhead-moved", serde_json::json!({
            "cue_id": cue_list.playhead_cue_id.map(|u| u.to_string())
        }));
    }
    let _ = app_handle.emit("cue-list-refresh", serde_json::json!({}));
    Ok(())
}

/// Stop all running cues with a soft fade-out.
#[tauri::command]
pub fn stop_all(
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    stop_locked(&state, &app_handle, &mut ws, false)
}

/// Hard-stop all running cues (immediate cut, no fades).
#[tauri::command]
pub fn hard_stop_all(app_handle: tauri::AppHandle, state: State<'_, AppState>) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    stop_locked(&state, &app_handle, &mut ws, true)
}

/// An operator-selected action on the local desk. The ordinary hard stop, ESC,
/// OSC, MIDI and agent paths continue to use their own existing stop behavior.
#[tauri::command]
pub fn hard_stop_blackout(
    workspace_id: uuid::Uuid,
    window: tauri::WebviewWindow,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Use Hard stop + blackout on the main desk.".into());
    }
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if ws.instance_id != workspace_id || !ws.preferences.general.show_hard_stop_blackout {
        return Err("Hard stop + blackout is not enabled for this workspace.".into());
    }
    // Latch first. Even a stop failure must leave lights held at black until Restore.
    super::light_cmds::apply_blackout(true, &state, &app_handle);
    stop_locked(&state, &app_handle, &mut ws, true)
}

pub(super) fn stop_locked(state: &AppState, app_handle: &tauri::AppHandle, ws: &mut Workspace, hard: bool) -> Result<(), String> {
    let mut transport = Transport::new(workspace_context(state, ws));
    if hard && ws.cue_lists.iter().any(|list| list.wall_clock_session.has_armed()) {
        for list in &mut ws.cue_lists {
            list.wall_clock_session.disarm_all();
        }
        let _ = app_handle.emit("cue-list-refresh", ());
    }
    // Stop All must reach every cue list, not just the one on screen — a cue
    // list tab is a UI concept, cues in a non-active list keep playing in the
    // background and a global "Stop" button has to silence them too.
    let mut stopping: Vec<_> = Vec::new();
    for cue_list in ws.cue_lists.iter_mut() {
        stopping.extend(
            cue_list
                .cues
                .iter()
                .filter(|c| c.is_running() || c.is_paused())
                .map(|c| c.id()),
        );
        if hard {
            transport.hard_stop_all(cue_list)
        } else {
            transport.stop_all(cue_list)
        }.map_err(|e| e.to_string())?;
    }
    for id in stopping {
        let _ = app_handle.emit(
            "cue-state-changed",
            serde_json::json!({
                "cue_id": id,
                "old_state": "running",
                "new_state": "standby",
            }),
        );
    }
    Ok(())
}

/// Stop a specific cue with a soft fade-out.
#[tauri::command]
pub fn stop_cue(
    cue_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let id: uuid::Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let context = workspace_context(&state, &ws);
    let mut transport = Transport::new(context);
    let cue_list = ws.active_cue_list_mut().ok_or("No active cue list")?;

    stop_cue_in_list(&mut transport, cue_list, id, &app_handle)
}

fn stop_cue_in_list(
    transport: &mut Transport,
    cue_list: &mut crate::show::cue_list::CueList,
    id: uuid::Uuid,
    app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    transport.stop_cue(cue_list, &id).map_err(|e| e.to_string())?;
    let _ = app_handle.emit(
        "cue-state-changed",
        serde_json::json!({
            "cue_id": id.to_string(),
            "old_state": "running",
            "new_state": "standby",
        }),
    );
    Ok(())
}

/// Pause a specific cue.
#[tauri::command]
pub fn pause_cue(
    cue_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let id: uuid::Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let context = workspace_context(&state, &ws);
    let mut transport = Transport::new(context);
    let cue_list = ws.active_cue_list_mut().ok_or("No active cue list")?;
    transport.pause_cue(cue_list, &id).map_err(|e| e.to_string())?;
    let _ = app_handle.emit(
        "cue-state-changed",
        serde_json::json!({
            "cue_id": cue_id,
            "old_state": "running",
            "new_state": "paused",
        }),
    );
    Ok(())
}

/// Set the master output gain from a dB value.
///
/// The gain is applied atomically in the audio callback without any lock.
/// Values ≤ −60 dB are treated as silence (gain = 0.0).
#[tauri::command]
pub fn set_master_volume(db: f32, state: State<'_, AppState>) -> Result<(), String> {
    let gain = if db <= -60.0 {
        0.0_f32
    } else {
        10_f32.powf(db / 20.0)
    };
    state.audio_engine.set_master_gain(gain);
    Ok(())
}

/// Resume a paused cue.
#[tauri::command]
pub fn resume_cue(
    cue_id: String,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let id: uuid::Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let context = workspace_context(&state, &ws);
    let mut transport = Transport::new(context);
    let cue_list = ws.active_cue_list_mut().ok_or("No active cue list")?;
    transport.resume_cue(cue_list, &id).map_err(|e| e.to_string())?;
    let _ = app_handle.emit(
        "cue-state-changed",
        serde_json::json!({
            "cue_id": cue_id,
            "old_state": "paused",
            "new_state": "running",
        }),
    );
    Ok(())
}

/// Seek a running or paused cue to `position_ms` from its action start.
///
/// No-op for non-seekable cue types (Memo, Stop, …).  Does not change the
/// cue's [`CueState`] — the cue keeps running or stays paused at the new
/// position.
#[tauri::command]
pub fn seek_cue(
    cue_id: String,
    position_ms: u64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let id: uuid::Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let context = workspace_context(&state, &ws);
    let cue_list = ws.active_cue_list_mut().ok_or("No active cue list")?;
    if let Some(cue) = cue_list.get_mut_recursive(&id) {
        cue.seek(position_ms, &context);
    }
    Ok(())
}

/// Set or clear the next entrance of one idle File Audio cue on the local desk.
#[tauri::command]
pub fn load_audio_to_time(
    workspace_id: uuid::Uuid,
    list_id: uuid::Uuid,
    cue_id: CueId,
    offset_ms: Option<i64>,
    window: tauri::WebviewWindow,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Use Load to Time on the main desk.".into());
    }
    let mut ws = state.workspace.lock().map_err(|error| error.to_string())?;
    if ws.instance_id() != workspace_id || ws.active_cue_list_id != list_id {
        return Err("Open the Audio cue's original workspace and cue list.".into());
    }
    let context = workspace_context(&state, &ws);
    let cue = ws.active_cue_list_mut().and_then(|list| list.get_mut(&cue_id))
        .and_then(|cue| (cue as &mut dyn std::any::Any).downcast_mut::<AudioCue>())
        .ok_or("Load to Time is available for a top-level File Audio cue.")?;
    if let Some(offset) = offset_ms {
        cue.load_to_time(offset).map_err(|error| error.to_string())?;
        context.request_cue_refresh();
    } else {
        cue.clear_loaded_time(&context);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{memo_cue::MemoCue, types::ContinueMode, wait_cue::WaitCue};

    #[test]
    fn hotkey_capture_checks_the_displayed_binding_and_undo_keeps_standby_and_arming() {
        use crate::{
            commands::undo_cmds::apply_history,
            cue::registry::CueRegistry,
            show::{undo_stack::HistoryDirection, wall_clock::{Observation, WallClockTrigger}},
        };

        let mut workspace = Workspace::new("Keyboard rehearsal");
        let instance = workspace.instance_id();
        let list = workspace.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let first = WaitCue::new();
        let first_id = first.id();
        list.push(Box::new(first));
        let second = WaitCue::new();
        let second_id = second.id();
        list.push(Box::new(second));
        let mut history = UndoHistory::new();

        assert!(edit_cue_hotkey(
            &mut workspace, &mut history, uuid::Uuid::new_v4(), list_id, first_id,
            None, Some("4".into()),
        ).is_err());
        assert!(!history.can_undo(list_id));
        assert!(edit_cue_hotkey(
            &mut workspace, &mut history, instance, list_id, first_id, None, Some("4".into()),
        ).unwrap());
        assert!(edit_cue_hotkey(
            &mut workspace, &mut history, instance, list_id, first_id, None, Some("5".into()),
        ).is_err());
        assert!(edit_cue_hotkey(
            &mut workspace, &mut history, instance, list_id, second_id, None, Some("4".into()),
        ).is_err());

        let list = workspace.active_cue_list_mut().unwrap();
        list.set_playhead(Some(second_id)).unwrap();
        list.wall_clock_triggers.insert(second_id, WallClockTrigger {
            time: chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            weekdays: 127,
        });
        list.wall_clock_session.set_armed(second_id, true, Observation::now());
        let registry = CueRegistry::new();
        for (direction, key) in [(HistoryDirection::Undo, None), (HistoryDirection::Redo, Some("4"))] {
            assert!(apply_history(&mut workspace, &mut history, &registry, direction).unwrap());
            let list = workspace.active_cue_list().unwrap();
            assert_eq!(list.hotkeys.get(&first_id).map(String::as_str), key);
            assert_eq!(list.playhead_cue_id, Some(second_id));
            assert!(list.wall_clock_session.is_armed(second_id));
        }
    }

    #[test]
    fn numbered_network_target_requires_one_exact_match_in_the_addressed_list() {
        let mut cue = MemoCue::new();
        cue.set_number(Some("1.5".into()));
        let first_id = cue.id();
        let mut first = CueList::new("First");
        first.auto_renumber = false;
        first.push(Box::new(cue));

        let mut cue = MemoCue::new();
        cue.set_number(Some("1.5".into()));
        let second_id = cue.id();
        let mut second = CueList::new("Second");
        second.auto_renumber = false;
        second.push(Box::new(cue));

        assert_eq!(numbered_network_target(&first, "1.5"), Some(first_id));
        assert_eq!(numbered_network_target(&second, "1.5"), Some(second_id));
        assert_eq!(numbered_network_target(&first, "1.50"), None);

        let mut duplicate = MemoCue::new();
        duplicate.set_number(Some("1.5".into()));
        first.push(Box::new(duplicate));
        assert_eq!(numbered_network_target(&first, "1.5"), None);
    }

    #[test]
    fn native_pause_and_resume_use_independent_playback_without_moving_standby() {
        let (context, _, _) = crate::test_support::recording_context();
        let transport = Transport::new(context.clone());
        let mut workspace = Workspace::new("Pause rehearsal");
        let numbers = ["101", "102", "103"].map(|number| {
            let mut cue = WaitCue::new();
            cue.set_number(Some(number.into()));
            cue.set_continue_mode(ContinueMode::DoNotContinue);
            cue
        });
        let [first, second, standby] = numbers.each_ref().map(|cue| cue.id());
        let list = workspace.active_cue_list_mut().unwrap();
        list.auto_renumber = false;
        for cue in numbers {
            list.push(Box::new(cue));
        }
        list.set_playhead(Some(standby)).unwrap();
        assert_eq!(transport.start_independent(list, first).unwrap().triggered, [first]);
        assert_eq!(transport.start_independent(list, second).unwrap().triggered, [second]);
        let generation = list.get(&first).unwrap().play_generation();

        let mut other_list = CueList::new("Other list");
        let mut other = WaitCue::new();
        other.set_continue_mode(ContinueMode::DoNotContinue);
        let other_id = other.id();
        other_list.push(Box::new(other));
        transport.start_independent(&mut other_list, other_id).unwrap();
        workspace.cue_lists.push(other_list);

        let mut changes = Vec::new();
        let list = workspace.active_cue_list_mut().unwrap();
        let mut apply = |list: &mut CueList, target, action| {
            changes.clear();
            apply_network_pause(&context, list, target, action, |cue, before| {
                changes.push((cue.id(), before, cue.state()));
            }).unwrap();
            changes.clone()
        };
        use CueState::{Paused, Running};
        use NetworkPauseAction::{Pause, Resume, Toggle};
        assert_eq!(apply(list, None, Pause), [(first, Running, Paused), (second, Running, Paused)]);
        assert_eq!(apply(list, None, Resume), [(first, Paused, Running), (second, Paused, Running)]);
        let address = numbered_network_target(list, "102").unwrap();
        assert_eq!(apply(list, Some(address), Pause), [(second, Running, Paused)]);
        assert!(apply(list, Some(address), Pause).is_empty());
        assert!(apply(list, Some(standby), Resume).is_empty());
        assert_eq!(apply(list, Some(address), Resume), [(second, Paused, Running)]);
        assert_eq!(apply(list, Some(second), Pause), [(second, Running, Paused)]);
        assert_eq!(apply(list, None, Toggle), [(first, Running, Paused)]);
        assert_eq!(apply(list, None, Toggle), [(first, Paused, Running), (second, Paused, Running)]);
        assert_eq!(list.get(&first).unwrap().play_generation(), generation);
        assert_eq!(list.playhead_cue_id, Some(standby));
        assert!(workspace.cue_lists[1].get(&other_id).unwrap().is_running());
    }
}
