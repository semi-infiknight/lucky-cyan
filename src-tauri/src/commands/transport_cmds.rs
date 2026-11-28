//! Tauri commands for transport control (GO, STOP, PAUSE, RESUME).

use std::collections::HashSet;
use std::sync::atomic::Ordering;
use std::sync::Mutex;

use tauri::{Emitter, Manager, State};
use uuid::Uuid;

use crate::{
    cue::{
        context::{CueContext, CueEvent},
        traits::Cue,
        types::{ContinueMode, CueColor, CueId, CueState, CueType},
    },
    show::{cue_list::CueList, transport::{stage_media_load_time, Transport}, undo_stack::UndoHistory, Workspace},
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
    context.workspace_ref = Some(std::sync::Arc::clone(&state.workspace));
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

pub(crate) fn accept_chained_go(state: &AppState, protection_ms: u32) -> bool {
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
    let ws_id = ws.instance_id;
    let msc_prefs = ws.preferences.midi.clone();

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

    let playhead_args = cue_list.playhead_cue().map(|cue| (
        cue.number().unwrap_or("").to_string(),
        cue.name().to_string(),
        cue.id(),
        cue.cue_type().qlab_name().to_string(),
    ));

    let result = transport.go(cue_list).map_err(|e| e.to_string())?;

    if let Some((number, name, id, cue_type)) = playhead_args {
        // QLab MSC broadcast: GO reports the cue number of the cue which ran.
        crate::engine::msc_broadcast::broadcast(&msc_prefs, crate::cue::msc::MscCommand::Go, &number);
        if crate::engine::osc_broadcast::has_listeners() {
            crate::engine::osc_broadcast::broadcast(ws_id, "go", Some(crate::engine::osc_broadcast::CueEventArgs {
                number: &number,
                name: &name,
                unique_id: id,
                cue_type: &cue_type,
            }));
        }
    }

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

/// QLab video-cue OSC property (`/cue/{n}/opacity|layer|blendMode|saturation|
/// scale|rotation|translateX|translateY`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoProp {
    Opacity,
    Layer,
    BlendMode,
    Saturation,
    Scale,
    Rotation,
    PanX,
    PanY,
}

/// Write payload for a video-cue property (blendMode takes the mode name).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum VideoPropValue {
    Num(f64),
    Text(String),
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum NetworkCueAction {
    Go,
    Start,
    /// QLab `/cue/{n}/preview`: start the cue now through the normal outputs
    /// without moving the playhead and without triggering auto-continue or
    /// auto-follow chains (Inkue's `start_independent`).
    Preview,
    Select,
    Stop,
    /// QLab `/cue/{n}/hardStop`: immediate stop, effects cut too.
    HardStop,
    /// QLab `/cue/{n}/devamp`: exit the loop at the end of the current pass.
    Devamp,
    /// QLab `/cue/{n}/panic [seconds]`: fade out and stop the cue; `Some(0)`
    /// or a second panic while the fade runs hard-stops immediately.
    Panic(Option<f64>),
    /// QLab `/cue/{n}/delete`: remove the cue from its list (undoable).
    Delete,
    /// QLab `/cue/{n}/infiniteLoop {bool}`; `None` toggles.
    SetInfiniteLoop(Option<bool>),
    /// QLab `/cue/{n}/sliceMarker/{index}/playCount {n}`: play count of slice
    /// `index` (0-based segment; `PLAY_COUNT_INFINITE` vamps until a Devamp).
    SetSlicePlayCount { index: usize, count: u32 },
    /// QLab `/cue/{n}/tempCueTargetNumber {m}`: temporary (live) target cue,
    /// resolved by cue number; cleared on reset, never saved.
    SetTemporaryTargetNumber(String),
    /// QLab `/cue/{n}/tempCueTargetID {uuid}`: same, resolved by unique ID.
    SetTemporaryTargetId(Uuid),
    /// QLab `/cue/{n}/cueTargetNumber {m}`: saved write of the target cue by number.
    SetTargetNumber(String),
    /// QLab `/cue/{n}/cueTargetID {uuid}`: saved write of the target cue by unique ID.
    SetTargetId(Uuid),
    /// QLab `/cue/{n}/move {position} [{target_group_id}]`: undoable reorder
    /// (0-based position) or move into the target Group at that position.
    Move { position: usize, target: Option<Uuid> },
    /// QLab `/cue/{n}/load {seconds}`: arm at a time (negative counts from the end).
    Load(f64),
    /// QLab `/cue/{n}/duration {seconds}` write (cues with an editable duration).
    SetDuration(f64),
    /// QLab `/cue/{n}/duration/+ {delta}` increment/decrement.
    AdjustDuration(f64),
    Reset,
    SetPause(NetworkPauseAction),
    /// QLab property writes (`/cue/{n}/preWait 5`, `/armed 0`, ...). These edit
    /// saved cue data, so they are control-level and need an allowlisted sender.
    SetPreWait(f64),
    SetPostWait(f64),
    SetArmed(bool),
    SetQName(String),
    SetContinueMode(ContinueMode),
    /// QLab `/cue/{n}/notes {string}`.
    SetNotes(String),
    /// QLab `/cue/{n}/colorName {string}`.
    SetColor(CueColor),
    /// QLab `/cue/{n}/flagged {bool}`; `None` toggles (the `toggle` argument or
    /// `/toggle` suffix). Flagging lives on the owning cue list, not the cue.
    SetFlagged(Option<bool>),
    /// QLab `/cue/{n}/rate {number}` (or `/rate/live {number}`): saved or live
    /// playback rate of a media cue.
    SetRate { value: f64, live: bool },
    /// QLab `/cue/{n}/level/{row}/{col} {db}` (or `/live` form): saved or live
    /// crosspoint level of a media cue.
    SetLevel { row: u32, col: u32, db: f64, live: bool },
    /// QLab video-cue property write (`/cue/{n}/opacity 0.5`, `/blendMode
    /// multiply`, ...; `/live` drives the on-screen voice without saving).
    SetVideoProp { prop: VideoProp, value: VideoPropValue, live: bool },
    /// QLab 5.5 `/cue/{n}/preWait/+ {delta}` (or `/preWait/+/1`): add delta seconds.
    AdjustPreWait(f64),
    /// QLab 5.5 `/cue/{n}/postWait/- {delta}`: subtract delta seconds.
    AdjustPostWait(f64),
    /// QLab 5.5 `/cue/{n}/continueMode/+ {steps}`: cycle through the modes.
    AdjustContinueMode(i64),
}

impl NetworkCueAction {
    pub(crate) fn is_write(&self) -> bool {
        matches!(
            self,
            NetworkCueAction::SetPreWait(_)
                | NetworkCueAction::SetPostWait(_)
                | NetworkCueAction::SetArmed(_)
                | NetworkCueAction::SetQName(_)
                | NetworkCueAction::SetContinueMode(_)
                | NetworkCueAction::SetNotes(_)
                | NetworkCueAction::SetColor(_)
                | NetworkCueAction::SetFlagged(_)
                | NetworkCueAction::AdjustPreWait(_)
                | NetworkCueAction::AdjustPostWait(_)
                | NetworkCueAction::AdjustContinueMode(_)
                | NetworkCueAction::SetDuration(_)
                | NetworkCueAction::AdjustDuration(_)
                | NetworkCueAction::SetRate { .. }
                | NetworkCueAction::SetLevel { .. }
                | NetworkCueAction::SetVideoProp { .. }
                | NetworkCueAction::Delete
                | NetworkCueAction::SetInfiniteLoop(_)
                | NetworkCueAction::SetSlicePlayCount { .. }
                | NetworkCueAction::SetTemporaryTargetNumber(_)
                | NetworkCueAction::SetTemporaryTargetId(_)
                | NetworkCueAction::SetTargetNumber(_)
                | NetworkCueAction::SetTargetId(_)
                | NetworkCueAction::Move { .. }
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkPauseAction {
    Pause,
    Resume,
    Toggle,
}

/// Resolve and apply under the same workspace lock so the addressed show or
/// list cannot change between matching the request and acting on it.
pub(crate) fn address_numbered_network_cue(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    action: NetworkCueAction,
    resolve: impl FnOnce(&Workspace) -> Option<(usize, CueId)>,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let Some((index, id)) = resolve(&ws) else { return Ok(()); };
    let Some(list) = ws.cue_lists.get(index) else { return Ok(()); };
    if action == NetworkCueAction::Go && !list.independent_pads()
        && !accept_chained_go(state, ws.preferences.general.double_go_protection_ms)
    {
        return Ok(());
    }
    let mut transport = Transport::new(workspace_context(state, &ws));
    // Resolve a temporary-target argument while the workspace is still
    // immutably borrowed; the addressed cue list is borrowed mutably below.
    let temp_target = match &action {
        NetworkCueAction::SetTemporaryTargetNumber(number) =>
            go_to_cue_target(&ws, number).map(|(_, id)| id),
        NetworkCueAction::SetTemporaryTargetId(uuid) => {
            let uuid = *uuid;
            crate::engine::osc_server::unique_workspace_cue(&ws, |cue| cue.id() == uuid).map(|(_, cue)| cue.id())
        }
        _ => None,
    };
    // Resolve a saved-target argument (cue number or unique ID) to
    // (target id, target number label) before the mutable list borrow.
    let saved_target = match &action {
        NetworkCueAction::SetTargetNumber(number) =>
            go_to_cue_target(&ws, number).map(|(_, id)| (id, Some(number.clone()))),
        NetworkCueAction::SetTargetId(uuid) => {
            let uuid = *uuid;
            crate::engine::osc_server::unique_workspace_cue(&ws, |cue| cue.id() == uuid)
                .map(|(_, cue)| (cue.id(), cue.number().map(str::to_owned)))
        }
        _ => None,
    };
    let cue_list = &mut ws.cue_lists[index];

    match &action {
        NetworkCueAction::SetTemporaryTargetNumber(_) | NetworkCueAction::SetTemporaryTargetId(_) => {
            if let Some(cue) = cue_list.get_mut_recursive(&id) {
                cue.set_temporary_target(temp_target);
            }
            let _ = app_handle.emit("cue-list-refresh", ());
            Ok(())
        }
        // Saved target write: an unresolvable target leaves the saved data
        // untouched (unlike the temporary form, which clears on None).
        NetworkCueAction::SetTargetNumber(_) | NetworkCueAction::SetTargetId(_) => {
            if let Some((target_id, label)) = saved_target {
                if edit_cue_field(&mut ws, index, id, |cue| cue.set_target_cue(target_id, label)) {
                    ws.mark_modified();
                    let _ = app_handle.emit("cue-list-refresh", ());
                    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
                }
            }
            Ok(())
        }
        NetworkCueAction::Go | NetworkCueAction::Start => {
            go_cue_in_list(&mut transport, cue_list, id, action == NetworkCueAction::Start, app_handle)
        }
        NetworkCueAction::Preview => {
            let result = transport.start_independent(cue_list, id).map(|_| ()).map_err(|e| e.to_string());
            let _ = app_handle.emit("cue-list-refresh", ());
            result
        }
        NetworkCueAction::Select => super::cue_cmds::set_playhead_in_list(cue_list, Some(id), app_handle),
        NetworkCueAction::Stop => stop_cue_in_list(&mut transport, cue_list, id, app_handle),
        NetworkCueAction::HardStop => {
            clear_panicking(&id);
            let result = transport.hard_stop_cue(cue_list, &id).map_err(|error| error.to_string());
            let _ = app_handle.emit("cue-list-refresh", ());
            result
        }
        NetworkCueAction::Devamp => {
            let result = transport.devamp_cue(cue_list, id).map(|_| ()).map_err(|error| error.to_string());
            let _ = app_handle.emit("cue-list-refresh", ());
            result
        }
        NetworkCueAction::Panic(duration) => {
            let result = panic_cue_in_list(&mut transport, cue_list, id, *duration, app_handle);
            let _ = app_handle.emit("cue-list-refresh", ());
            result
        }
        NetworkCueAction::Load(secs) => {
            let secs = *secs;
            let Some(cue) = cue_list.get_mut_recursive(&id) else { return Ok(()) };
            let ms = if secs < 0.0 {
                let total = cue.duration().map(|d| d.as_millis() as i64).unwrap_or(0);
                (total + (secs * 1000.0) as i64).max(0)
            } else {
                (secs * 1000.0) as i64
            };
            cue.seek(ms.max(0) as u64, &transport.context);
            let _ = app_handle.emit("cue-list-refresh", ());
            Ok(())
        }
        NetworkCueAction::Reset => {
            let result = transport.reset_cue(cue_list, id).map_err(|error| error.to_string());
            let _ = app_handle.emit("cue-list-refresh", ());
            result
        }
        NetworkCueAction::SetPause(pause) => network_pause_in_list(&transport.context, cue_list, Some(id), *pause, app_handle),
        NetworkCueAction::Delete => {
            let result = delete_cue_locked(state, &mut ws, index, id, app_handle);
            let _ = app_handle.emit("cue-list-refresh", ());
            result
        }
        NetworkCueAction::Move { position, target } => {
            let result = move_cue_locked(state, &mut ws, index, id, *position, *target, app_handle);
            let _ = app_handle.emit("cue-list-refresh", ());
            result
        }
        // QLab `/live` writes drive the running voice and record a temporary
        // override; they never touch saved data and are not undoable.
        NetworkCueAction::SetRate { value, live: true } => {
            apply_live_rate(transport.context.audio_engine.as_ref(), cue_list, id, *value);
            let _ = app_handle.emit("cue-list-refresh", ());
            Ok(())
        }
        NetworkCueAction::SetLevel { row, col, db, live: true } => {
            apply_live_level(transport.context.audio_engine.as_ref(), cue_list, id, *row, *col, *db);
            let _ = app_handle.emit("cue-list-refresh", ());
            Ok(())
        }
        NetworkCueAction::SetVideoProp { prop, value, live: true } => {
            apply_live_video_prop(transport.context.output_engine.as_ref(), cue_list, id, *prop, value);
            let _ = app_handle.emit("cue-list-refresh", ());
            Ok(())
        }
        write if write.is_write() => apply_write(&mut ws, index, id, write, app_handle),
        _ => Ok(()),
    }
}

/// QLab `/cue/{n}/delete`: remove the cue (undoable), stopping it first when
/// running. The caller holds the workspace lock.
fn delete_cue_locked(
    state: &AppState,
    ws: &mut Workspace,
    index: usize,
    id: CueId,
    app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    let Some(list) = ws.cue_lists.get(index) else { return Ok(()) };
    let list_id = list.id;
    let workspace_id = ws.instance_id;
    let stop_fade_ms = ws.preferences.audio.default_fade_out_ms;
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    let context = stop_context(state, stop_fade_ms);
    let result = crate::commands::cue_cmds::apply_cue_removal(
        ws,
        &mut history,
        workspace_id,
        list_id,
        &[id],
        None,
        |cue| crate::commands::cue_cmds::stop_removed_subtree(cue, &context),
    );
    drop(history);
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    result.map(|_| ()).map_err(|error| error.to_string())
}

/// QLab `/cue/{n}/move {position} [{target_group_id}]`: reorder a top-level cue
/// to 0-based `position`, or move it into the target Group at `position`
/// (clamped to the Group's length). Undoable; the caller holds the workspace
/// lock.
fn move_cue_locked(
    state: &AppState,
    ws: &mut Workspace,
    index: usize,
    id: CueId,
    position: usize,
    target: Option<Uuid>,
    app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    let result = (|| {
        let list = ws.cue_lists.get_mut(index).ok_or_else(|| "No such cue list".to_string())?;
        let before = super::undo_cmds::take_snapshot(list);
        let result = list.move_cue_to(&id, position, target.as_ref());
        if result.is_ok() {
            history.push_action(list.id, before);
            ws.mark_modified();
        }
        result.map_err(|error| error.to_string())
    })();
    drop(history);
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    result
}

/// QLab `/new {cue_type}` (optionally workspace-scoped): create a cue of the
/// given type at the end of the active cue list.
/// QLab `/new {cue_type} [{cue_ID}]`: create a cue in the active list and
/// return its unique ID (sent back as the reply's `data`).  `Ok(None)` means
/// the workspace selector did not match, so no reply is owed.
pub(crate) fn create_network_cue(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    workspace: Option<crate::engine::osc_server::WorkspaceSelector<'_>>,
    cue_type: CueType,
    cue_id: Option<String>,
) -> Result<Option<CueId>, String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if let Some(requested) = workspace {
        if !requested.matches(&ws) {
            return Ok(None);
        }
    }
    let requested_id = match cue_id {
        Some(text) => Some(text.parse::<Uuid>().map_err(|_| format!("Invalid cue ID: {text}"))?),
        None => None,
    };
    let id = crate::commands::cue_cmds::create_cue_in_active_list(state, &mut ws, app_handle, cue_type, requested_id)?;
    Ok(Some(id))
}

/// QLab `/cue/{n}/rate/live {number}`: set the running voice's rate and record
/// the temporary override on the cue (cleared on reset).
fn apply_live_rate(audio_engine: &dyn crate::engine::engine_traits::AudioEngineApi, cue_list: &mut CueList, id: CueId, rate: f64) {
    let Some(cue) = cue_list.get_mut_recursive(&id) else { return };
    cue.set_live_rate(Some(rate));
    if let Some(voice) = cue.media_voice_id() {
        if let Err(error) = audio_engine.set_voice_rate(voice, rate as f32) {
            log::debug!("OSC live rate on cue {id}: {error}");
        }
    }
}

/// QLab `/cue/{n}/level/{row}/{col}/live {db}`: set the running voice's
/// crosspoint and record the temporary override on the cue.
fn apply_live_level(
    audio_engine: &dyn crate::engine::engine_traits::AudioEngineApi,
    cue_list: &mut CueList,
    id: CueId,
    row: u32,
    col: u32,
    db: f64,
) {
    let Some(cue) = cue_list.get_mut_recursive(&id) else { return };
    cue.set_live_level(row as usize, col as usize, Some(db));
    if let Some(voice) = cue.media_voice_id() {
        let gain = crate::cue::types::db_to_linear(db) as f32;
        if let Err(error) = audio_engine.set_voice_crosspoint(voice, row as u8, col as u8, gain, cue.level_mirror_mono()) {
            log::debug!("OSC live level on cue {id}: {error}");
        }
    }
}

/// Apply a wildcard (`/cue/*/...`) action to every resolved top-level cue.
/// Start/Select/Go never reach here — the OSC parser rejects them on patterns.
pub(crate) fn address_network_cue_pattern(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    action: NetworkCueAction,
    resolve: impl FnOnce(&Workspace) -> Vec<(usize, CueId)>,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let targets = resolve(&ws);
    if targets.is_empty() {
        return Ok(());
    }
    // Resolve a saved-target argument once for all wildcard matches.
    let saved_target = match &action {
        NetworkCueAction::SetTargetNumber(number) =>
            go_to_cue_target(&ws, number).map(|(_, id)| (id, Some(number.clone()))),
        NetworkCueAction::SetTargetId(uuid) => {
            let uuid = *uuid;
            crate::engine::osc_server::unique_workspace_cue(&ws, |cue| cue.id() == uuid)
                .map(|(_, cue)| (cue.id(), cue.number().map(str::to_owned)))
        }
        _ => None,
    };
    let mut transport = Transport::new(workspace_context(state, &ws));
    let mut first_error: Option<String> = None;
    let mut wrote = false;
    for (index, id) in targets {
        let result = match &action {
            NetworkCueAction::Stop => {
                let list = &mut ws.cue_lists[index];
                stop_cue_in_list(&mut transport, list, id, app_handle)
            }
            NetworkCueAction::HardStop => {
                let list = &mut ws.cue_lists[index];
                clear_panicking(&id);
                transport.hard_stop_cue(list, &id).map_err(|error| error.to_string())
            }
            NetworkCueAction::Panic(duration) => {
                let list = &mut ws.cue_lists[index];
                panic_cue_in_list(&mut transport, list, id, *duration, app_handle)
            }
            NetworkCueAction::Reset => {
                let list = &mut ws.cue_lists[index];
                transport.reset_cue(list, id).map_err(|error| error.to_string())
            }
            NetworkCueAction::SetPause(pause) => {
                let list = &mut ws.cue_lists[index];
                network_pause_in_list(&transport.context, list, Some(id), *pause, app_handle)
            }
            NetworkCueAction::Delete => {
                delete_cue_locked(state, &mut ws, index, id, app_handle)
            }
            NetworkCueAction::Move { position, target } => {
                let result = move_cue_locked(state, &mut ws, index, id, *position, *target, app_handle);
                if result.is_ok() {
                    wrote = true;
                }
                result
            }
            NetworkCueAction::SetTargetNumber(_) | NetworkCueAction::SetTargetId(_) => {
                match &saved_target {
                    Some((target_id, label)) => {
                        let (target_id, label) = (*target_id, label.clone());
                        if edit_cue_field(&mut ws, index, id, |cue| cue.set_target_cue(target_id, label)) {
                            wrote = true;
                        }
                        Ok(())
                    }
                    None => Ok(()),
                }
            }
            NetworkCueAction::SetRate { value, live: true } => {
                let list = &mut ws.cue_lists[index];
                apply_live_rate(transport.context.audio_engine.as_ref(), list, id, *value);
                Ok(())
            }
            NetworkCueAction::SetLevel { row, col, db, live: true } => {
                let list = &mut ws.cue_lists[index];
                apply_live_level(transport.context.audio_engine.as_ref(), list, id, *row, *col, *db);
                Ok(())
            }
            NetworkCueAction::SetVideoProp { prop, value, live: true } => {
                let list = &mut ws.cue_lists[index];
                apply_live_video_prop(transport.context.output_engine.as_ref(), list, id, *prop, value);
                Ok(())
            }
            write if write.is_write() => {
                match write_edit(write) {
                    Some(edit) => {
                        if edit_cue_field(&mut ws, index, id, edit) {
                            wrote = true;
                        }
                        Ok(())
                    }
                    None => Ok(()),
                }
            }
            _ => Ok(()),
        };
        if let Err(error) = result {
            first_error.get_or_insert(error);
        }
    }
    if wrote {
        ws.mark_modified();
        let _ = app_handle.emit("cue-list-refresh", ());
        let _ = app_handle.emit("workspace-modified", ());
    }
    if matches!(action, NetworkCueAction::Reset) {
        let _ = app_handle.emit("cue-list-refresh", ());
    }
    first_error.map_or(Ok(()), Err)
}

type CueEdit<'a> = Box<dyn FnOnce(&mut dyn Cue) + 'a>;

/// Map a property-write action to the cue edit it performs.
fn write_edit(action: &NetworkCueAction) -> Option<CueEdit<'_>> {
    match action {
        NetworkCueAction::SetPreWait(secs) => {
            let secs = *secs;
            Some(Box::new(move |cue| cue.set_pre_wait(std::time::Duration::from_secs_f64(secs.max(0.0)))))
        }
        NetworkCueAction::SetPostWait(secs) => {
            let secs = *secs;
            Some(Box::new(move |cue| cue.set_post_wait(std::time::Duration::from_secs_f64(secs.max(0.0)))))
        }
        NetworkCueAction::SetArmed(armed) => {
            let armed = *armed;
            Some(Box::new(move |cue| cue.set_disabled(!armed)))
        }
        NetworkCueAction::SetQName(name) => {
            let name = name.clone();
            Some(Box::new(move |cue| cue.set_name(name)))
        }
        NetworkCueAction::SetContinueMode(mode) => {
            let mode = *mode;
            Some(Box::new(move |cue| cue.set_continue_mode(mode)))
        }
        NetworkCueAction::SetNotes(notes) => {
            let notes = notes.clone();
            Some(Box::new(move |cue| cue.set_notes(notes)))
        }
        NetworkCueAction::SetColor(color) => {
            let color = *color;
            Some(Box::new(move |cue| cue.set_color(color)))
        }
        NetworkCueAction::AdjustPreWait(delta) => {
            let delta = *delta;
            Some(Box::new(move |cue| {
                let next = (cue.pre_wait().as_secs_f64() + delta).max(0.0);
                cue.set_pre_wait(std::time::Duration::from_secs_f64(next));
            }))
        }
        NetworkCueAction::AdjustPostWait(delta) => {
            let delta = *delta;
            Some(Box::new(move |cue| {
                let next = (cue.post_wait().as_secs_f64() + delta).max(0.0);
                cue.set_post_wait(std::time::Duration::from_secs_f64(next));
            }))
        }
        NetworkCueAction::AdjustContinueMode(steps) => {
            let steps = *steps;
            Some(Box::new(move |cue| {
                let mode = cue.continue_mode().cycled(steps);
                cue.set_continue_mode(mode);
            }))
        }
        NetworkCueAction::SetDuration(secs) => {
            let secs = *secs;
            Some(Box::new(move |cue| {
                cue.set_duration(Some(std::time::Duration::from_secs_f64(secs)));
            }))
        }
        NetworkCueAction::AdjustDuration(delta) => {
            let delta = *delta;
            Some(Box::new(move |cue| {
                if let Some(current) = cue.duration() {
                    let next = (current.as_secs_f64() + delta).max(0.0);
                    cue.set_duration(Some(std::time::Duration::from_secs_f64(next)));
                }
            }))
        }
        NetworkCueAction::SetRate { value, live: false } => {
            let value = *value;
            Some(Box::new(move |cue| cue.set_rate(value)))
        }
        NetworkCueAction::SetLevel { row, col, db, live: false } => {
            let (row, col, db) = (*row, *col, *db);
            Some(Box::new(move |cue| cue.set_level(row as usize, col as usize, db)))
        }
        NetworkCueAction::SetInfiniteLoop(on) => {
            let on = *on;
            Some(Box::new(move |cue| {
                let next = on.unwrap_or_else(|| !cue.infinite_loop().unwrap_or(false));
                cue.set_infinite_loop(next);
            }))
        }
        NetworkCueAction::SetSlicePlayCount { index, count } => {
            let (index, count) = (*index, *count);
            Some(Box::new(move |cue| { let _ = cue.set_slice_play_count(index, count); }))
        }
        NetworkCueAction::SetVideoProp { prop, value, live: false } => {
            let (prop, value) = (*prop, value.clone());
            Some(Box::new(move |cue| {
                let mut style = cue.video_style().unwrap_or_default();
                let mut geometry = cue.video_geometry().unwrap_or_default();
                apply_video_prop_to(&mut style, &mut geometry, prop, &value);
                cue.set_video_style(style);
                cue.set_video_geometry(geometry);
            }))
        }
        _ => None,
    }
}

/// Mutate a style/geometry pair for one QLab video-cue property write.
fn apply_video_prop_to(
    style: &mut crate::engine::output_engine::LayerStyle,
    geometry: &mut crate::engine::output_engine::VideoGeometry,
    prop: VideoProp,
    value: &VideoPropValue,
) {
    match (prop, value) {
        (VideoProp::Opacity, VideoPropValue::Num(v)) => style.opacity = v.clamp(0.0, 1.0),
        (VideoProp::Layer, VideoPropValue::Num(v)) => style.layer = Some(*v as u32),
        (VideoProp::BlendMode, VideoPropValue::Text(name)) => {
            if let Some(mode) = crate::engine::output_engine::BlendMode::from_osc_name(name) {
                style.blend_mode = mode;
            }
        }
        (VideoProp::Saturation, VideoPropValue::Num(v)) => style.saturation = v.clamp(0.0, 2.0),
        (VideoProp::Scale, VideoPropValue::Num(v)) => geometry.scale = *v,
        (VideoProp::Rotation, VideoPropValue::Num(v)) => geometry.rotation = (*v as u32) % 360,
        (VideoProp::PanX, VideoPropValue::Num(v)) => geometry.pan_x = v.clamp(-1.0, 1.0),
        (VideoProp::PanY, VideoPropValue::Num(v)) => geometry.pan_y = v.clamp(-1.0, 1.0),
        _ => {}
    }
}

/// QLab `/cue/{n}/{prop}/live {value}`: push the change to the on-screen voice
/// without touching saved data (a reset clears it, like all temporary values).
fn apply_live_video_prop(
    output_engine: &dyn crate::engine::engine_traits::OutputEngineApi,
    cue_list: &mut CueList,
    id: CueId,
    prop: VideoProp,
    value: &VideoPropValue,
) {
    let Some(cue) = cue_list.get_mut_recursive(&id) else { return };
    let mut style = cue.video_style().unwrap_or_default();
    let mut geometry = cue.video_geometry().unwrap_or_default();
    apply_video_prop_to(&mut style, &mut geometry, prop, value);
    cue.push_live_video(output_engine, &style, &geometry);
}

fn apply_write(
    ws: &mut Workspace,
    index: usize,
    id: CueId,
    action: &NetworkCueAction,
    app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    if let NetworkCueAction::SetFlagged(flag) = action {
        // Flagging is a cue-list property; toggle reads the current state.
        if apply_flagged_write(ws, index, id, *flag) {
            ws.mark_modified();
            let _ = app_handle.emit("cue-list-refresh", ());
            let _ = app_handle.emit("workspace-modified", ());
        }
        return Ok(());
    }
    match write_edit(action) {
        Some(edit) => apply_cue_write(ws, index, id, edit, app_handle),
        None => Ok(()),
    }
}

/// Set or clear a cue's flag in its owning list (`None` toggles). Returns false
/// when the list is gone, so callers can skip the modified/refresh signals.
fn apply_flagged_write(ws: &mut Workspace, index: usize, id: CueId, flag: Option<bool>) -> bool {
    let Some(list) = ws.cue_lists.get(index) else { return false };
    if list.get_recursive(&id).is_none() {
        return false;
    }
    let flag = flag.unwrap_or_else(|| !list.flagged_cues.contains(&id));
    let list = ws.cue_lists.get_mut(index).unwrap();
    if flag { list.flagged_cues.insert(id); } else { list.flagged_cues.remove(&id); }
    true
}

/// Apply one saved-field edit to the addressed cue, then mark the workspace
/// modified and refresh the list so the change is visible in the desk flow.
fn apply_cue_write(
    ws: &mut Workspace,
    index: usize,
    id: CueId,
    edit: impl FnOnce(&mut dyn Cue),
    app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    if !edit_cue_field(ws, index, id, edit) {
        return Ok(());
    }
    ws.mark_modified();
    let _ = app_handle.emit("cue-list-refresh", ());
    let _ = app_handle.emit("workspace-modified", ());
    Ok(())
}

/// Apply the edit to the addressed cue (recursively). Returns false when the
/// cue is gone, so callers can skip the modified/refresh signals.
fn edit_cue_field(ws: &mut Workspace, index: usize, id: CueId, edit: impl FnOnce(&mut dyn Cue)) -> bool {
    let Some(cue) = ws.cue_lists.get_mut(index).and_then(|list| list.get_mut_recursive(&id)) else {
        return false;
    };
    edit(cue);
    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NetworkWorkspaceAction<'a> {
    Go,
    /// QLab `/go/{cue_number}`: park the playhead on the numbered cue, then GO.
    GoToCue(&'a str),
    /// QLab `/auditionGo`: preview the playhead cue on the dedicated audition
    /// output without starting it in the show or moving the playhead.
    AuditionGo,
    /// QLab-style `/auditionGo/{cue_number}`: audition the numbered cue
    /// (resolved like `/go/{n}`) without moving the playhead.
    AuditionGoToCue(&'a str),
    Stop,
    HardStop,
    /// QLab `/panic`: stops like `/stop` but broadcasts `panicAll`.
    Panic,
    Reset,
    SetPaused(bool),
    /// QLab `/togglePause`: pause everything running, else resume all.
    TogglePaused,
    /// QLab `/alwaysAudition {bool}`: when on, workspace GO previews the
    /// playhead cue on the audition output instead of starting it.
    SetAlwaysAudition(bool),
    /// QLab `/save`: write the open workspace to its current file.
    Save,
    /// QLab `/undo` / `/redo`.
    Undo,
    Redo,
    Step { forward: bool, sequence: bool },
    ClearStandby,
}

/// Resolve `/go/{cue_number}`: an exact match in the active list always wins;
/// otherwise the number must be unique across the workspace's lists.
pub(crate) fn go_to_cue_target(show: &Workspace, number: &str) -> Option<(Uuid, CueId)> {
    let mut matches = show
        .cue_lists
        .iter()
        .filter_map(|list| list.goto_number_target(number).ok().map(|id| (list.id, id)));
    let first = matches.next()?;
    if show.active_cue_list_id == first.0 || matches.next().is_none() {
        return Some(first);
    }
    None
}

/// Resolve an `/auditionGo` target to (list, cue): the playhead cue of the
/// active list, or the numbered cue for `/auditionGo/{n}`.  The playhead is
/// not moved — auditioning previews a cue on the monitor output only.
pub(crate) fn audition_target(show: &Workspace, action: &NetworkWorkspaceAction) -> Option<(Uuid, CueId)> {
    match action {
        NetworkWorkspaceAction::AuditionGo => {
            let list = show.active_cue_list()?;
            list.playhead_cue_id.map(|id| (show.active_cue_list_id, id))
        }
        NetworkWorkspaceAction::AuditionGoToCue(number) => go_to_cue_target(show, number),
        _ => None,
    }
}

pub(crate) fn control_network_workspace<'a>(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    action: NetworkWorkspaceAction<'a>,
    matches_workspace: impl FnOnce(&Workspace) -> bool,
) -> Result<(), String> {
    use NetworkWorkspaceAction::*;
    // Save/Undo/Redo lock the workspace themselves, so run them without holding it.
    if matches!(action, Save | Undo | Redo) {
        let matches = {
            let show = state.workspace.lock().map_err(|error| error.to_string())?;
            matches_workspace(&show)
        };
        if !matches {
            return Ok(());
        }
        return match action {
            Save => super::workspace_cmds::save_current_workspace(state),
            Undo => super::undo_cmds::change_history(
                state, app_handle, crate::show::undo_stack::HistoryDirection::Undo),
            Redo => super::undo_cmds::change_history(
                state, app_handle, crate::show::undo_stack::HistoryDirection::Redo),
            _ => unreachable!(),
        };
    }
    let mut show = state.workspace.lock().map_err(|error| error.to_string())?;
    if !matches_workspace(&show) {
        return Ok(());
    }
    match action {
        Go if show.preferences.general.always_audition => {
            // QLab "always audition": GO previews standby on the audition output.
            if let Some((list_id, id)) = audition_target(&show, &NetworkWorkspaceAction::AuditionGo) {
                drop(show);
                audition_network_cue(app_handle, list_id, id);
            }
            Ok(())
        }
        Go => go_locked(state, app_handle, &mut show).map(|_| ()),
        GoToCue(number) => {
            if show.preferences.general.always_audition {
                if let Some((list_id, id)) = audition_target(&show, &NetworkWorkspaceAction::AuditionGoToCue(number)) {
                    drop(show);
                    audition_network_cue(app_handle, list_id, id);
                }
                return Ok(());
            }
            let Some((list_id, id)) = go_to_cue_target(&show, number) else { return Ok(()) };
            show.switch_active_list(list_id);
            let list = show
                .cue_lists
                .iter_mut()
                .find(|list| list.id == list_id)
                .ok_or("No active cue list")?;
            super::cue_cmds::set_playhead_in_list(list, Some(id), app_handle)?;
            go_locked(state, app_handle, &mut show).map(|_| ())
        }
        SetAlwaysAudition(on) => {
            show.preferences.general.always_audition = on;
            show.mark_modified();
            let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
            Ok(())
        }
        AuditionGo | AuditionGoToCue(..) => {
            if let Some((list_id, id)) = audition_target(&show, &action) {
                drop(show);
                audition_network_cue(app_handle, list_id, id);
            }
            Ok(())
        }
        Stop | HardStop => stop_locked(state, app_handle, &mut show, action == HardStop),
        Panic => {
            let result = stop_locked(state, app_handle, &mut show, false);
            broadcast_bare_event(&show, "panicAll");
            broadcast_msc(&show, crate::cue::msc::MscCommand::AllOff, "");
            result
        }
        Reset => reset_workspace_locked(state, app_handle, &mut show),
        SetPaused(paused) => pause_all_lists(state, app_handle, &mut show, paused),
        TogglePaused => {
            let any_running = show.cue_lists.iter().any(|list| list.cues.iter().any(|cue| cue.is_running()));
            pause_all_lists(state, app_handle, &mut show, !any_running)
        }
        Step { forward, sequence } => {
            let Some(list) = show.active_cue_list_mut() else { return Ok(()) };
            if list.step_standby(forward, sequence).map_err(|error| error.to_string())? {
                emit_playhead(app_handle, list.playhead_cue_id);
            }
            Ok(())
        }
        ClearStandby => {
            let Some(list) = show.active_cue_list_mut().filter(|list| !list.independent_pads()) else { return Ok(()) };
            super::cue_cmds::set_playhead_in_list(list, None, app_handle)
        }
        // Handled before the workspace lock is taken, above.
        Save | Undo | Redo => Ok(()),
    }
}

/// QLab `/auditionGo`: preview a cue on the dedicated audition output.
///
/// Spawned off the OSC receive thread; the workspace lock is held only to
/// resolve the cue, build the monitor clips and snapshot the patches.  Once
/// the audition voice is playing, `auditionGo` is broadcast with the cue's
/// number/name/uniqueID/type.  The playhead is not moved.
pub(crate) fn audition_network_cue(app_handle: &tauri::AppHandle, list_id: Uuid, cue_id: CueId) {
    let handle = app_handle.clone();
    tauri::async_runtime::spawn(async move {
        if let Err(error) = run_audition_go(&handle, list_id, cue_id).await {
            log::warn!("Network /auditionGo: {error}");
        }
    });
}

async fn run_audition_go(app_handle: &tauri::AppHandle, list_id: Uuid, cue_id: CueId) -> Result<(), String> {
    let state = app_handle.state::<AppState>();
    let device = crate::machine_config::load().audition_device_id.ok_or(
        "Set a dedicated audition output in Preferences → Audio (or the cue monitor) before using /auditionGo.",
    )?;
    let request_id = Uuid::new_v4();
    let (clips, patches) = {
        let ws = state.workspace.lock().map_err(|e| e.to_string())?;
        let owner = super::cue_cmds::GroupTimelineOwner { workspace_id: ws.instance_id(), list_id };
        let cue = super::cue_cmds::scoped_audition_cue(&ws, owner, cue_id)?;
        let clips = super::cue_cmds::audition_clips(cue, None, None, 0.0, true)?;
        let patches = ws.output_patches.iter().map(|p| p.device_id.clone()).collect::<Vec<_>>();
        (clips, patches)
    };
    state.audio_engine.reserve_audition(request_id)?;
    let target = device.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::engine::device_manager::run_bounded(std::time::Duration::from_secs(1), move || {
            crate::engine::device_manager::check_audition_output(&target)
        }).unwrap_or_else(|| Err("The PipeWire output check timed out. Refresh outputs and retry.".into()))
    }).await.map_err(|e| e.to_string())??;
    let result = if clips.len() == 1 {
        let (clip, effects) = clips.into_iter().next().unwrap();
        state.audio_engine.start_audition_with_effects(request_id, &device, patches.iter().map(String::as_str), clip, effects)
    } else {
        let mixed = crate::engine::audition::mix_clips(&clips)?;
        state.audio_engine.start_audition(request_id, &device, patches.iter().map(String::as_str), mixed)
    };
    result.map(|_| ())?;
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if let Some(cue) = ws.cue_list_by_id(list_id).and_then(|list| list.get_recursive(&cue_id)) {
        broadcast_current_cue_event("auditionGo", cue);
    }
    Ok(())
}

/// Pause or resume every cue list in the workspace (QLab `/pause`, `/resume`,
/// `/togglePause` at workspace level).
fn pause_all_lists(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    show: &mut Workspace,
    paused: bool,
) -> Result<(), String> {
    let context = workspace_context(state, show);
    let mut first_error = None;
    for list in &mut show.cue_lists {
        if let Err(error) = crate::show::cue_list_window::set_list_paused(list, &context, paused) {
            first_error.get_or_insert(error);
        }
    }
    let _ = app_handle.emit("cue-list-refresh", ());
    broadcast_bare_event(show, if paused { "pauseAll" } else { "resumeAll" });
    first_error.map_or(Ok(()), |error| Err(error.to_string()))
}

/// QLab show-control broadcast helpers (`/listen` clients).
fn broadcast_current_cue_event(event: &str, cue: &dyn crate::cue::traits::Cue) {
    if crate::engine::osc_broadcast::has_listeners() {
        crate::engine::osc_broadcast::broadcast_current(event, Some(crate::engine::osc_broadcast::CueEventArgs {
            number: cue.number().unwrap_or(""),
            name: cue.name(),
            unique_id: cue.id(),
            cue_type: cue.cue_type().qlab_name(),
        }));
    }
}

fn broadcast_bare_event(ws: &Workspace, event: &str) {
    if crate::engine::osc_broadcast::has_listeners() {
        crate::engine::osc_broadcast::broadcast(ws.instance_id, event, None);
    }
}

/// QLab MSC show-control broadcast (workspace MIDI → MSC Broadcast destinations).
fn broadcast_msc(ws: &Workspace, command: crate::cue::msc::MscCommand, q_number: &str) {
    crate::engine::msc_broadcast::broadcast(&ws.preferences.midi, command, q_number);
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

    let start_args = cue_list.get_recursive(&id).map(|cue| (
        cue.number().unwrap_or("").to_string(),
        cue.name().to_string(),
        cue.id(),
        cue.cue_type().qlab_name().to_string(),
    ));

    let result = if preserve_playhead {
        transport.start_independent(cue_list, id)
    } else {
        transport.go_by_id(cue_list, &id)
    }.map_err(|e| e.to_string())?;

    if let Some((number, name, cue_id, cue_type)) = start_args {
        if crate::engine::osc_broadcast::has_listeners() {
            crate::engine::osc_broadcast::broadcast_current("cue/start", Some(crate::engine::osc_broadcast::CueEventArgs {
                number: &number,
                name: &name,
                unique_id: cue_id,
                cue_type: &cue_type,
            }));
        }
    }

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

#[tauri::command]
pub fn reset_workspace_transport(
    workspace_id: uuid::Uuid,
    window: tauri::WebviewWindow,
    app_handle: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Use Reset Workspace on the main desk.".into());
    }
    let mut show = state.workspace.lock().map_err(|error| error.to_string())?;
    if show.instance_id() != workspace_id {
        return Err("The workspace changed before it could be reset.".into());
    }
    reset_workspace_locked(&state, &app_handle, &mut show)
}

fn reset_workspace_locked(
    state: &AppState, app_handle: &tauri::AppHandle, show: &mut Workspace,
) -> Result<(), String> {
    let mut transport = Transport::new(workspace_context(state, show));
    transport.reset_workspace(&mut show.cue_lists).map_err(|error| error.to_string())?;
    let standby = show.active_cue_list().and_then(|list| list.playhead_cue_id);
    emit_playhead(app_handle, standby);
    broadcast_bare_event(show, "resetAll");
    broadcast_msc(show, crate::cue::msc::MscCommand::Reset, "");
    clear_all_panicking();
    Ok(())
}

fn emit_playhead(app_handle: &tauri::AppHandle, standby: Option<CueId>) {
    let _ = app_handle.emit("playhead-moved", serde_json::json!({ "cue_id": standby }));
    let _ = app_handle.emit("cue-list-refresh", ());
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
    broadcast_bare_event(ws, if hard { "hardStopAll" } else { "stopAll" });
    // QLab MSC broadcast: Stop All and HardStop All both send ALL_OFF.
    broadcast_msc(ws, crate::cue::msc::MscCommand::AllOff, "");
    clear_all_panicking();
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

/// Cue IDs currently fading out from a QLab `/cue/{n}/panic`: a second panic
/// while the fade runs hard-stops immediately (QLab behavior).
static PANICKING: Mutex<Option<HashSet<CueId>>> = Mutex::new(None);

fn panicking_contains(id: &CueId) -> bool {
    match PANICKING.lock() {
        Ok(guard) => guard.as_ref().is_some_and(|set| set.contains(id)),
        Err(_) => false,
    }
}

fn mark_panicking(id: CueId) {
    if let Ok(mut guard) = PANICKING.lock() {
        guard.get_or_insert_with(HashSet::new).insert(id);
    }
}

fn clear_panicking(id: &CueId) {
    if let Ok(mut guard) = PANICKING.lock() {
        if let Some(set) = guard.as_mut() {
            set.remove(id);
        }
    }
}

fn clear_all_panicking() {
    if let Ok(mut guard) = PANICKING.lock() {
        *guard = None;
    }
}

/// QLab `/cue/{n}/panic [seconds]`: fade the cue out and stop it. `Some(0)` —
/// or a second panic while the fade is still running — hard-stops immediately.
fn panic_cue_in_list(
    transport: &mut Transport,
    cue_list: &mut crate::show::cue_list::CueList,
    id: uuid::Uuid,
    duration: Option<f64>,
    app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    if panicking_contains(&id) || duration == Some(0.0) {
        clear_panicking(&id);
        let result = transport.hard_stop_cue(cue_list, &id).map_err(|error| error.to_string());
        let _ = app_handle.emit(
            "cue-state-changed",
            serde_json::json!({
                "cue_id": id.to_string(),
                "old_state": "running",
                "new_state": "standby",
            }),
        );
        return result;
    }
    let was_running = cue_list
        .get_recursive(&id)
        .is_some_and(|cue| cue.is_running() || cue.is_paused());
    let result = stop_cue_in_list(transport, cue_list, id, app_handle);
    if was_running {
        mark_panicking(id);
    }
    result
}

fn stop_cue_in_list(
    transport: &mut Transport,
    cue_list: &mut crate::show::cue_list::CueList,
    id: uuid::Uuid,
    app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    clear_panicking(&id);
    if let Some(cue) = cue_list.get_recursive(&id) {
        broadcast_current_cue_event("cue/stop", cue);
    }
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

/// Set or clear the next entrance of one idle File Audio or Video cue on the local desk.
#[tauri::command]
pub fn load_media_to_time(
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
        return Err("Open the cue's original workspace and cue list.".into());
    }
    let context = workspace_context(&state, &ws);
    let cue = ws.active_cue_list_mut().and_then(|list| list.get_mut(&cue_id))
        .filter(|cue| matches!(cue.cue_type(), CueType::Audio | CueType::Video))
        .ok_or("Load to Time is available for a top-level File Audio or Video cue.")?;
    if let Some(offset) = offset_ms {
        stage_media_load_time(cue, offset).map_err(|error| error.to_string())?;
        context.request_cue_refresh();
    } else if cue.cancel_loaded_time() {
        context.request_cue_refresh();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{memo_cue::MemoCue, types::ContinueMode, wait_cue::WaitCue};

    #[test]
    fn network_cue_writes_edit_the_addressed_cue_and_mark_the_workspace() {
        let mut workspace = Workspace::new("OSC writes");
        assert!(!workspace.is_modified);
        let mut cue = WaitCue::new();
        cue.set_number(Some("7".into()));
        let id = cue.id();
        let list_index = 0;
        workspace.active_cue_list_mut().unwrap().push(Box::new(cue));

        assert!(edit_cue_field(&mut workspace, list_index, id, |cue| {
            cue.set_pre_wait(std::time::Duration::from_secs(2));
            cue.set_continue_mode(ContinueMode::AutoFollow);
            cue.set_disabled(true);
            cue.set_name("renamed".into());
        }));
        workspace.mark_modified();
        let cue = workspace.cue_lists[list_index].get_recursive(&id).unwrap();
        assert_eq!(cue.pre_wait(), std::time::Duration::from_secs(2));
        assert_eq!(cue.continue_mode(), ContinueMode::AutoFollow);
        assert!(cue.is_disabled());
        assert_eq!(cue.name(), "renamed");
        assert!(workspace.is_modified);
        assert!(!edit_cue_field(&mut workspace, list_index, uuid::Uuid::new_v4(), |_| {}));
    }

    #[test]
    fn notes_color_and_flagged_writes_apply_to_the_addressed_cue() {
        use crate::cue::types::CueColor;
        let mut workspace = Workspace::new("OSC property writes");
        let mut cue = MemoCue::new();
        cue.set_number(Some("9".into()));
        let id = cue.id();
        let list_index = 0;
        workspace.active_cue_list_mut().unwrap().push(Box::new(cue));

        // Notes and color go through the cue edit mapping.
        let notes_action = NetworkCueAction::SetNotes("check mic 3".into());
        let edit = write_edit(&notes_action).unwrap();
        assert!(edit_cue_field(&mut workspace, list_index, id, edit));
        let color_action = NetworkCueAction::SetColor(CueColor::from_osc_name("Hot Pink").unwrap());
        let edit = write_edit(&color_action).unwrap();
        assert!(edit_cue_field(&mut workspace, list_index, id, edit));
        let cue = workspace.cue_lists[list_index].get_recursive(&id).unwrap();
        assert_eq!(cue.notes(), "check mic 3");
        assert_eq!(cue.color(), CueColor::HotPink);

        // Flagged lives on the list and toggles when the write carries None.
        assert!(!workspace.cue_lists[list_index].flagged_cues.contains(&id));
        assert!(apply_flagged_write(&mut workspace, list_index, id, Some(true)));
        assert!(workspace.cue_lists[list_index].flagged_cues.contains(&id));
        assert!(apply_flagged_write(&mut workspace, list_index, id, None));
        assert!(!workspace.cue_lists[list_index].flagged_cues.contains(&id));
        assert!(apply_flagged_write(&mut workspace, list_index, id, None));
        assert!(workspace.cue_lists[list_index].flagged_cues.contains(&id));
        assert!(apply_flagged_write(&mut workspace, list_index, id, Some(false)));
        assert!(!workspace.cue_lists[list_index].flagged_cues.contains(&id));
        assert!(!apply_flagged_write(&mut workspace, list_index, uuid::Uuid::new_v4(), Some(true)));

        assert!(NetworkCueAction::SetNotes(String::new()).is_write());
        assert!(NetworkCueAction::SetColor(CueColor::Red).is_write());
        assert!(NetworkCueAction::SetFlagged(None).is_write());
        assert!(!NetworkCueAction::Go.is_write());
    }

    #[test]
    fn qlab_increment_decrement_writes_adjust_waits_and_cycle_continue_mode() {
        use crate::cue::types::ContinueMode;
        let mut workspace = Workspace::new("OSC +/- writes");
        let mut cue = WaitCue::new();
        cue.set_number(Some("3".into()));
        cue.set_pre_wait(std::time::Duration::from_secs(2));
        cue.set_post_wait(std::time::Duration::from_secs(1));
        cue.set_continue_mode(ContinueMode::DoNotContinue);
        let id = cue.id();
        workspace.active_cue_list_mut().unwrap().push(Box::new(cue));

        let apply = |workspace: &mut Workspace, action: &NetworkCueAction| {
            let edit = write_edit(action).unwrap();
            assert!(edit_cue_field(workspace, 0, id, edit));
        };
        apply(&mut workspace, &NetworkCueAction::AdjustPreWait(1.5));
        apply(&mut workspace, &NetworkCueAction::AdjustPostWait(-0.5));
        apply(&mut workspace, &NetworkCueAction::AdjustPreWait(-99.0));
        let cue = workspace.cue_lists[0].get_recursive(&id).unwrap();
        assert_eq!(cue.pre_wait(), std::time::Duration::from_secs_f64(0.0), "waits clamp at zero");
        assert_eq!(cue.post_wait(), std::time::Duration::from_secs_f64(0.5));

        // continueMode cycles with rollover in both directions.
        assert_eq!(cue.continue_mode(), ContinueMode::DoNotContinue);
        apply(&mut workspace, &NetworkCueAction::AdjustContinueMode(1));
        assert_eq!(workspace.cue_lists[0].get_recursive(&id).unwrap().continue_mode(), ContinueMode::AutoContinue);
        apply(&mut workspace, &NetworkCueAction::AdjustContinueMode(1));
        assert_eq!(workspace.cue_lists[0].get_recursive(&id).unwrap().continue_mode(), ContinueMode::AutoFollow);
        apply(&mut workspace, &NetworkCueAction::AdjustContinueMode(1));
        assert_eq!(workspace.cue_lists[0].get_recursive(&id).unwrap().continue_mode(), ContinueMode::DoNotContinue, "rolls over");
        apply(&mut workspace, &NetworkCueAction::AdjustContinueMode(-1));
        assert_eq!(workspace.cue_lists[0].get_recursive(&id).unwrap().continue_mode(), ContinueMode::AutoFollow, "negative steps wrap");
        assert!(NetworkCueAction::AdjustPreWait(1.0).is_write());
        assert!(NetworkCueAction::AdjustContinueMode(1).is_write());
    }

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

        assert_eq!(first.unique_numbered_cue("1.5").map(Cue::id), Some(first_id));
        assert_eq!(second.unique_numbered_cue("1.5").map(Cue::id), Some(second_id));
        assert!(first.unique_numbered_cue("1.50").is_none());

        let mut duplicate = MemoCue::new();
        duplicate.set_number(Some("1.5".into()));
        first.push(Box::new(duplicate));
        assert!(first.unique_numbered_cue("1.5").is_none());
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
        let address = list.unique_numbered_cue("102").unwrap().id();
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
