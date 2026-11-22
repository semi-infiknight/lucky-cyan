//! Tauri commands for cue CRUD operations.

use std::{
    collections::HashSet,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use uuid::Uuid;

use crate::{
    cue::{
        audio_cue::AudioCue,
        group_cue::GroupCue,
        decoded_audio::{
            apply_decoded_audio_to_copy, assign_fresh_ids, assign_fresh_ids_to_copies, collect_decoded_audio,
        },
        traits::Cue,
        types::{ContinueMode, CueColor, CueState, CueType, GroupMode, PlaylistSecondTrigger, SliceList, PLAY_COUNT_INFINITE},
        video_cue::VideoCue,
    },
    engine::audition::AuditionClip,
    show::{
        cue_colors::CueStarts,
        cue_list::{CueList, CueListMode, CueNumberChange},
        undo_stack::UndoHistory,
        Workspace,
    },
    state::{AppState, LoadingCues},
};

// ---------------------------------------------------------------------------
// DTO types
// ---------------------------------------------------------------------------

/// Compact summary of a cue, used to populate the cue list table in the UI.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CueSummary {
    #[serde(default)]
    pub flagged: bool,
    #[serde(default)]
    pub has_flagged_child: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hotkey: Option<String>,
    /// Exact workspace and list that supplied this cue row.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cue_list_owner: Option<GroupTimelineOwner>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wall_clock_trigger: Option<crate::show::wall_clock::WallClockTrigger>,
    #[serde(default)]
    pub wall_clock_armed: bool,
    pub id: String,
    pub cue_type: CueType,
    pub name: String,
    pub number: Option<String>,
    /// Free-form notes visible in the Notes column and inspector.
    pub notes: String,
    pub state: CueState,
    pub continue_mode: ContinueMode,
    pub color: CueColor,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub second_color: Option<CueColor>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub display_color: Option<CueColor>,
    pub pre_wait_ms: u64,
    #[serde(default)]
    pub supports_timeline_entrance: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_slices: Option<TimelineSliceGuides>,
    /// Owner captured with the Simultaneous Group and its direct child rows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeline_owner: Option<GroupTimelineOwner>,
    pub post_wait_ms: u64,
    pub duration_ms: Option<u64>,
    /// File path for audio cues, None for others.
    pub file_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub still_rotation_target: Option<bool>,
    /// Identity of the active picture run displayed by this summary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub picture_voice_id: Option<Uuid>,
    /// Memo Cue only: the note shown in the Target column, where a media cue
    /// shows its filename.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memo_text: Option<String>,
    /// True while the audio file is being decoded in a background thread.
    pub is_loading: bool,
    /// True when this cue is disabled — skipped by the transport on GO.
    pub is_disabled: bool,
    /// True when this cue's media file was assigned but is now missing from disk.
    pub is_broken: bool,
    /// True for non-critical problems (no file assigned, zero duration, empty group).
    pub is_warning: bool,
    /// True when the cue can be scrubbed while it plays (`seek_cue` moves it).
    pub seekable: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loaded_time_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub is_prepared: Option<bool>,
    /// Duration of one pass before trim and repetition, or the period of an
    /// authored loop. `None` when the cue has no known period or duration.
    pub file_duration_ms: Option<u64>,
    /// Human-readable description of the warning condition, when `is_warning` is true.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning_message: Option<String>,
    /// Name of the Output Patch this cue plays through — explicit patch, or
    /// the workspace default for audio-producing cues with none assigned.
    /// `None` for cue types with no audio output (Output column stays empty).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_patch_name: Option<String>,
    /// For Group cues: their direct children summaries (recursive).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<CueSummary>>,
    /// For Group cues: the playback mode.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_mode: Option<GroupMode>,
    /// For Playlist Group cues: whether the playlist loops (wraps last → first).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub playlist_loop: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_advance_playhead: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_second_trigger: Option<PlaylistTriggerSetting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_auto_shuffle: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub playlist_run_order: Option<Vec<Uuid>>,
    /// For running Sequential Group cues: ID of the currently active child
    /// (running right now or next to fire on GO after a DoNotContinue pause).
    /// `None` for Simultaneous groups and non-Group cues.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_child_id: Option<String>,
}

/// Returns `true` when a media cue's file was assigned but is missing from disk.
fn check_broken(cue: &dyn Cue, workspace_dir: Option<&std::path::Path>, missing: &mut dyn FnMut(&std::path::Path) -> bool) -> bool {
    match cue.cue_type() {
        CueType::Audio | CueType::Video | CueType::Image | CueType::MidiFile => {
            match cue.media_file_path() {
                None => false,
                Some(p) if p.as_os_str().is_empty() => false,
                Some(p) => {
                    if p.is_absolute() {
                        missing(p)
                    } else {
                        workspace_dir.map(|d| missing(&d.join(p))).unwrap_or(true)
                    }
                }
            }
        }
        _ => false,
    }
}

/// Returns a warning message for non-critical problems, or `None` if the cue is healthy.
fn check_warning(cue: &dyn Cue) -> Option<String> {
    match cue.cue_type() {
        CueType::Audio | CueType::Video | CueType::Image | CueType::MidiFile => {
            match cue.media_file_path() {
                None => Some("No file assigned".to_string()),
                Some(p) if p.as_os_str().is_empty() => Some("No file assigned".to_string()),
                _ => None,
            }
        }
        CueType::Wait => {
            if cue.duration() == Some(std::time::Duration::ZERO) {
                Some("Duration is zero".to_string())
            } else {
                None
            }
        }
        CueType::Group => {
            if cue.child_cues().map(|c| c.is_empty()).unwrap_or(false) {
                Some("Group is empty".to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Resolution table for the Output column: the workspace patch list plus the
/// default patch id, borrowed for the duration of one summary pass.
struct PatchTable<'a> {
    patches: &'a [crate::engine::device_manager::OutputPatch],
    default_id: Option<Uuid>,
}

impl PatchTable<'_> {
    /// The patch name a cue plays through, per the Output-column rules.
    fn name_for(&self, cue: &dyn Cue) -> Option<String> {
        let name_of = |id: Uuid| {
            self.patches
                .iter()
                .find(|p| p.id == id)
                .map(|p| p.name.clone())
                .unwrap_or_else(|| "(missing)".to_string())
        };
        match cue.output_patch_id() {
            Some(id) => Some(name_of(id)),
            // No explicit patch: audio-producing cues use the default patch.
            None => match cue.cue_type() {
                CueType::Audio | CueType::Video | CueType::Mic => {
                    self.default_id.map(name_of)
                }
                _ => None,
            },
        }
    }
}

fn supports_timeline_entrance(kind: CueType) -> bool {
    // These direct Group children delay their own action. Cue-targeted Fades
    // also need cue-list routing that direct Group GO does not provide.
    matches!(kind, CueType::Wait | CueType::Audio | CueType::Video | CueType::Image
        | CueType::Camera | CueType::Text | CueType::Mic
        | CueType::Midi | CueType::MidiFile | CueType::Timecode | CueType::Group)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimelineSliceGuides {
    offsets_ms: Vec<u64>,
    duration_ms: Option<u64>,
}

fn project_timeline_slices(slices: &SliceList, start_ms: u64, end_ms: u64, rate: f64) -> Option<TimelineSliceGuides> {
    if !rate.is_finite() || rate <= 0.0 { return None; }
    let segments = slices.segments(start_ms, end_ms);
    if segments.is_empty() { return None; }
    let mut offsets_ms = Vec::with_capacity(segments.len() - 1);
    let mut played_ms = 0u64;
    let wall_time = |file_ms: u64| (file_ms as f64 / rate).round() as u64;
    for (index, &(start, end, count)) in segments.iter().enumerate() {
        if count == PLAY_COUNT_INFINITE {
            return Some(TimelineSliceGuides { offsets_ms, duration_ms: None });
        }
        played_ms = played_ms.saturating_add((end - start).saturating_mul(u64::from(count)));
        if index + 1 < segments.len() {
            let offset = wall_time(played_ms);
            if offset != 0 && offsets_ms.last() != Some(&offset) { offsets_ms.push(offset); }
        }
    }
    Some(TimelineSliceGuides { offsets_ms, duration_ms: Some(wall_time(played_ms)) })
}

fn cue_timeline_slices(cue: &dyn Cue) -> Option<TimelineSliceGuides> {
    let data = cue as &dyn std::any::Any;
    let (slices, start, end, rate) = if let Some(audio) = data.downcast_ref::<AudioCue>() {
        (&audio.slices, audio.start_time, audio.end_time, audio.rate)
    } else {
        let video = data.downcast_ref::<VideoCue>()?;
        (&video.slices, video.start_time, video.end_time, 1.0)
    };
    if slices.is_empty() { return None; }
    let end = match (cue.file_duration(), end) {
        (Some(file), Some(trim)) => file.min(trim),
        (length, trim) => length.or(trim)?,
    };
    project_timeline_slices(slices, start.unwrap_or_default().as_millis() as u64, end.as_millis() as u64, rate)
}

#[cfg(test)]
#[test]
fn timeline_slice_guides_follow_trim_rate_repeats_and_indefinite_vamps() {
    let mut slices = SliceList {
        markers: vec![500, 1_000, 3_000, 5_000],
        play_counts: vec![1, 2, 3, 1, 1],
    };
    let projected = project_timeline_slices(&slices, 500, 4_000, 2.0).unwrap();
    assert_eq!(projected.offsets_ms, [500, 3_500]);
    assert_eq!(projected.duration_ms, Some(4_000));
    slices.play_counts[2] = PLAY_COUNT_INFINITE;
    let vamped = project_timeline_slices(&slices, 500, 4_000, 2.0).unwrap();
    assert_eq!(vamped.offsets_ms, [500]);
    assert_eq!(vamped.duration_ms, None);
    assert!(project_timeline_slices(&slices, 5_500, 6_000, 1.0).is_none());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupTimelineOwner {
    workspace_id: Uuid,
    list_id: Uuid,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaylistTriggerSetting {
    workspace_id: Uuid,
    list_id: Uuid,
    trigger: PlaylistSecondTrigger,
}

fn summarise(
    cue: &dyn Cue,
    list: &CueList,
    owner: GroupTimelineOwner,
    workspace_dir: Option<&Path>,
    patches: &PatchTable,
    starts: Option<&CueStarts>,
    missing: &mut dyn FnMut(&Path) -> bool,
) -> CueSummary {
    let warning_message = check_warning(cue);
    let advancing_playlist = (cue.playlist_advance_playhead() == Some(true))
        .then(|| (cue as &dyn std::any::Any).downcast_ref::<GroupCue>()).flatten();
    let second_color = list.second_colors.get(&cue.id()).copied();
    let has_timeline = cue.group_mode() == Some(GroupMode::Simultaneous);
    let children: Option<Vec<CueSummary>> = cue.child_cues().map(|children| {
        children.iter().map(|child| {
            let mut summary = summarise(child.as_ref(), list, owner, workspace_dir, patches, starts, missing);
            if has_timeline { summary.timeline_slices = cue_timeline_slices(child.as_ref()); }
            summary
        }).collect()
    });
    CueSummary {
        flagged: list.flagged_cues.contains(&cue.id()),
        has_flagged_child: children.as_ref().is_some_and(|children| {
            children.iter().any(|child| child.flagged || child.has_flagged_child)
        }),
        hotkey: list.hotkeys.get(&cue.id()).cloned(),
        cue_list_owner: Some(owner),
        wall_clock_trigger: list.wall_clock_triggers.get(&cue.id()).copied(),
        wall_clock_armed: list.wall_clock_session.is_armed(cue.id()),
        output_patch_name: patches.name_for(cue),
        id: cue.id().to_string(),
        cue_type: cue.cue_type(),
        name: cue.name().to_string(),
        number: cue.number().map(|s| s.to_string()),
        notes: cue.notes().to_string(),
        state: cue.state(),
        continue_mode: cue.continue_mode(),
        color: cue.color(),
        second_color,
        display_color: second_color.filter(|_| starts.is_some_and(|starts| starts.contains(&cue.id()))),
        pre_wait_ms: cue.pre_wait().as_millis() as u64,
        supports_timeline_entrance: supports_timeline_entrance(cue.cue_type()),
        timeline_slices: None,
        timeline_owner: has_timeline.then_some(owner),
        post_wait_ms: cue.post_wait().as_millis() as u64,
        duration_ms: cue.duration().map(|d| d.as_millis() as u64),
        file_path: cue.media_file_path().map(|p| p.to_string_lossy().into_owned()),
        still_rotation_target: cue.is_still_rotation_target().then_some(true),
        picture_voice_id: if cue.is_running() && matches!(cue.cue_type(), CueType::Video | CueType::Image | CueType::Camera) {
            cue.playing_voice_id()
        } else { None },
        memo_text: cue.memo_text().map(str::to_string),
        is_loading: false,
        is_disabled: cue.is_disabled(),
        is_broken: check_broken(cue, workspace_dir, missing),
        is_warning: warning_message.is_some(),
        seekable: cue.supports_seek(),
        loaded_time_ms: cue.loaded_time_ms(),
        is_prepared: cue.has_prepared_media().then_some(true),
        warning_message,
        file_duration_ms: cue.file_duration().map(|d| d.as_millis() as u64),
        children,
        group_mode: cue.group_mode(),
        playlist_loop: cue.playlist_loop(),
        playlist_advance_playhead: cue.playlist_advance_playhead(),
        playlist_second_trigger: advancing_playlist
            .filter(|_| list.mode == CueListMode::Sequential && list.index_of(&cue.id()).is_some())
            .and_then(GroupCue::playlist_second_trigger)
            .map(|trigger| PlaylistTriggerSetting {
                workspace_id: owner.workspace_id, list_id: owner.list_id, trigger,
            }),
        playlist_auto_shuffle: advancing_playlist.and_then(GroupCue::playlist_auto_shuffle),
        playlist_run_order: advancing_playlist.and_then(GroupCue::playlist_run_order),
        active_child_id: cue.active_child_id().map(|id| id.to_string()),
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

/// Return a compact summary of every cue in the active cue list.
#[tauri::command]
pub fn get_all_cues(state: State<'_, AppState>, app: tauri::AppHandle) -> Result<Vec<CueSummary>, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let loading = state.loading_cues.lock().map_err(|e| e.to_string())?;
    let cue_list = ws.active_cue_list().ok_or("No active cue list")?;
    Ok(state.media_presence.collect(
        |missing| list_summaries(&ws, cue_list, &loading, missing),
        move || { let _ = app.emit("cue-list-refresh", ()); },
    ))
}

pub(super) fn list_summaries(
    ws: &crate::show::Workspace,
    cue_list: &crate::show::cue_list::CueList,
    loading: &LoadingCues,
    missing: &mut dyn FnMut(&std::path::Path) -> bool,
) -> Vec<CueSummary> {
    let ws_dir = ws.file_path.as_ref().and_then(|p| p.parent()).map(|p| p.to_owned());
    let ws_dir_ref = ws_dir.as_deref();
    let patch_table = PatchTable {
        patches: &ws.output_patches,
        default_id: ws.default_output_patch_id,
    };
    let starts = ws.cue_starts.lock().ok();
    let owner = GroupTimelineOwner { workspace_id: ws.instance_id(), list_id: cue_list.id };

    cue_list
        .cues
        .iter()
        .map(|c| {
            let mut s = summarise(c.as_ref(), cue_list, owner, ws_dir_ref, &patch_table, starts.as_deref(), missing);
            s.is_loading = loading.contains(&c.id());
            s
        })
        .collect()
}

/// Retake an eligible live movie's speed without changing its saved programming.
#[tauri::command]
pub fn reset_silent_speed(workspace_id: Uuid, cue_id: Uuid, state: State<'_, AppState>) -> Result<(), String> {
    let workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    if workspace.instance_id != workspace_id {
        return Err("The open workspace changed.".into());
    }
    let (cue, armed) = workspace.active_cue_list()
        .and_then(|list| list.cue_with_arming(&cue_id)).ok_or("Cue not found")?;
    let voice = cue.playing_voice_id().filter(|_| armed && cue.supports_silent_speed())
        .ok_or("Start the armed silent Video first.")?;
    let owner = Uuid::new_v4();
    if state.output_engine.claim_silent_speed(voice, owner).is_none()
        || !state.output_engine.set_silent_speed(voice, owner, 1.0)
    {
        return Err("This playback is unavailable for silent movie speed control.".into());
    }
    Ok(())
}

#[tauri::command]
pub fn revert_picture_fade(
    workspace_id: Uuid,
    list_id: Uuid,
    cue_id: Uuid,
    state: State<'_, AppState>,
) -> Result<crate::cue::fade_cue::PictureFadeRevertResult, String> {
    let mut workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    if workspace.instance_id() != workspace_id || workspace.active_cue_list_id != list_id {
        return Err("Open the Fade's original workspace and cue list.".into());
    }
    let list = workspace.active_cue_list_mut().ok_or("No active cue list.")?;
    crate::show::crossfade::revert_picture_fade(list, state.output_engine.as_ref(), cue_id)
        .map_err(|error| error.to_string())
}

/// The programmed Fade and live playback the operator selected for preview.
#[derive(Deserialize)]
pub struct PictureSaturationPreviewRequest {
    workspace_id: Uuid,
    list_id: Uuid,
    fade_id: Uuid,
    target_id: Uuid,
    voice_id: Uuid,
    saturation: f64,
}

/// Briefly display the Fade's destination saturation without running the Fade.
#[tauri::command]
pub fn preview_picture_saturation(
    request: PictureSaturationPreviewRequest, window: tauri::WebviewWindow, state: State<'_, AppState>,
) -> Result<(Uuid, u64), String> {
    if window.label() != "main" {
        return Err("Preview saturation from the main desk.".into());
    }
    let workspace = state.workspace.try_lock().map_err(|_| "Desk busy. Try preview again.")?;
    if workspace.instance_id() != request.workspace_id || workspace.active_cue_list_id != request.list_id {
        return Err("The workspace or cue list changed. Select the Fade again.".into());
    }
    let list = workspace.active_cue_list().ok_or("No active cue list.")?;
    let saturation = crate::show::crossfade::resolve_picture_saturation_preview(
        list, request.fade_id, request.target_id, request.voice_id, request.saturation,
    ).map_err(|error| error.to_string())?;
    let token = state.output_engine.preview_voice_saturation(request.voice_id, saturation)
        .ok_or("Wait for the target picture to appear on its output before previewing.")?;
    Ok((token, crate::engine::output_engine::PICTURE_SATURATION_PREVIEW_MS))
}

/// Dismiss the identified disposable preview. A later preview has its own token.
#[tauri::command]
pub fn cancel_picture_saturation_preview(
    voice_id: Uuid, token: Uuid, window: tauri::WebviewWindow, state: State<'_, AppState>,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Preview saturation from the main desk.".into());
    }
    state.output_engine.cancel_voice_saturation_preview(voice_id, token);
    Ok(())
}

/// Return the full serialised JSON for a single cue.
#[tauri::command]
pub fn get_cue(cue_id: String, state: State<'_, AppState>) -> Result<serde_json::Value, String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let cue_list = ws.active_cue_list().ok_or("No active cue list")?;
    let cue = cue_list.get_recursive(&id).ok_or("Cue not found")?;
    let mut data = cue.serialize();
    data["flagged"] = serde_json::json!(cue_list.flagged_cues.contains(&id));
    data["second_color"] = serde_json::json!(cue_list.second_colors.get(&id));
    if let Some(id) = cue.temporary_target() { data["temporary_target_id"] = serde_json::json!(id); }
    Ok(data)
}

/// Change a review flag on the exact cue list the operator is editing.
#[tauri::command]
pub fn set_cue_flag(
    workspace_id: Uuid,
    list_id: Uuid,
    cue_id: Uuid,
    flagged: bool,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    if workspace.instance_id != workspace_id || workspace.active_cue_list_id != list_id {
        return Err("The active workspace or cue list changed".into());
    }
    let mut history = state.undo_stack.lock().map_err(|error| error.to_string())?;
    let changed = super::review_cmds::edit_flag(&mut workspace, list_id, cue_id, None, flagged, Some(&mut history))?;
    drop(history);
    drop(workspace);
    if changed {
        let _ = app_handle.emit("workspace-modified", ());
    }
    Ok(())
}

/// Set the alternate display color for a cue in the list being edited.
#[tauri::command]
pub fn set_second_cue_color(
    workspace_id: Uuid,
    list_id: Uuid,
    cue_id: Uuid,
    second_color: Option<CueColor>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    if workspace.instance_id != workspace_id || workspace.active_cue_list_id != list_id {
        return Err("The active workspace or cue list changed".into());
    }
    let list = workspace.active_cue_list_mut().ok_or("No active cue list")?;
    if list.get_recursive(&cue_id).is_none() {
        return Err("The cue no longer exists".into());
    }
    if list.second_colors.get(&cue_id).copied() == second_color {
        return Ok(());
    }
    state.undo_stack.lock().map_err(|error| error.to_string())?
        .push_action(list_id, super::undo_cmds::take_snapshot(list));
    if let Some(color) = second_color {
        list.second_colors.insert(cue_id, color);
    } else {
        list.second_colors.remove(&cue_id);
    }
    workspace.mark_modified();
    drop(workspace);
    let _ = app_handle.emit("workspace-modified", ());
    Ok(())
}

/// Clear the displayed played colors across the workspace.
#[tauri::command]
pub fn reset_cue_colors(
    workspace_id: Uuid,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    if workspace.instance_id != workspace_id {
        return Err("The open workspace changed.".into());
    }
    workspace.cue_starts.lock().map_err(|error| error.to_string())?.clear();
    Ok(())
}

/// Add a new cue of the given type at the given position (index).
/// Pass `position = -1` to append at the end.
#[derive(serde::Deserialize)]
pub struct CartDrop {
    list_id: uuid::Uuid,
    cell: crate::show::cart_layout::CartCell,
}

#[tauri::command]
pub fn add_cue(
    cue_type: CueType,
    position: i64,
    cart_drop: Option<CartDrop>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let registry = state.registry.lock().map_err(|e| e.to_string())?;
    let cue = registry.create(&cue_type).map_err(|e| e.to_string())?;
    let cue_id = cue.id();
    drop(registry);

    let mut cue = cue;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let cue_list = ws.active_cue_list_mut().ok_or("No active cue list")?;
    if let Some(drop) = &cart_drop {
        let grid = cue_list.cart_layout.as_ref().filter(|_| cue_list.id == drop.list_id &&
            cue_list.mode == crate::show::cue_list::CueListMode::Cart).ok_or("The active cart changed")?;
        if !(1..=grid.rows).contains(&drop.cell.row) || !(1..=grid.columns).contains(&drop.cell.column)
            || grid.pads.values().any(|pos| *pos == Some(drop.cell))
        {
            return Err("Drop the new cue on an empty visible cell".into());
        }
    }
    state.undo_stack.lock().map_err(|e| e.to_string())?
        .push_action(cue_list.id, super::undo_cmds::take_snapshot(cue_list));

    // With auto-renumber off, push/insert won't number the new cue — assign the
    // next free number so it isn't blank (auto-renumber on: the resequence does it).
    if !cue_list.auto_renumber {
        cue.set_number(Some(cue_list.next_available_number()));
    }

    if position < 0 || position as usize >= cue_list.cues.len() {
        cue_list.push(cue);
    } else {
        cue_list.insert(position as usize, cue);
    }

    if let (Some(drop), Some(grid)) = (cart_drop, &mut cue_list.cart_layout) {
        grid.pads.insert(cue_id, Some(drop.cell));
    }
    ws.mark_modified();
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(cue_id.to_string())
}

/// Remove a cue by ID.
#[tauri::command]
pub fn remove_cue(
    workspace_id: Uuid,
    list_id: Uuid,
    cue_id: Uuid,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    remove_owned_cues(workspace_id, list_id, &[cue_id], None, &state, &app_handle)
}

/// Move a cue to a new position.
#[tauri::command]
pub fn move_cue(
    cue_id: String,
    new_position: usize,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    edit_cue_structure(&mut ws, &mut history, |list| list.move_cue(&id, new_position))?;
    drop(history);
    drop(ws);
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Remove multiple cues in one atomic operation.
#[tauri::command]
pub fn remove_cues(
    workspace_id: Uuid,
    list_id: Uuid,
    ids: Vec<Uuid>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    remove_owned_cues(workspace_id, list_id, &ids, None, &state, &app_handle)
}

fn remove_owned_cues(
    workspace_id: Uuid,
    list_id: Uuid,
    ids: &[Uuid],
    parent: Option<Uuid>,
    state: &AppState,
    app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let stop_fade_ms = ws.preferences.audio.default_fade_out_ms;
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    let context = super::transport_cmds::stop_context(state, stop_fade_ms);
    let changed = apply_cue_removal(&mut ws, &mut history, workspace_id, list_id, ids, parent, |cue| {
        stop_removed_subtree(cue, &context);
    })?;
    drop(history);
    drop(ws);
    if changed {
        let _ = app_handle.emit("workspace-modified", ());
    }
    Ok(())
}

/// Validate the selected membership before submitting any Stop. Keep cue
/// ownership through a successful removal so engine voices can be stopped.
fn apply_cue_removal(
    ws: &mut Workspace,
    history: &mut UndoHistory,
    workspace_id: Uuid,
    list_id: Uuid,
    ids: &[Uuid],
    parent: Option<Uuid>,
    mut stop: impl FnMut(&mut dyn Cue),
) -> Result<bool, String> {
    if ws.instance_id != workspace_id || ws.active_cue_list_id != list_id {
        return Err("The workspace or cue list changed. Select the cues again.".into());
    }
    if ids.is_empty() {
        return Ok(false);
    }
    if ids.iter().collect::<HashSet<_>>().len() != ids.len() {
        return Err("Select each cue to remove only once.".into());
    }
    let list = ws.active_cue_list_mut().ok_or("No active cue list")?;
    let before = super::undo_cmds::take_snapshot(list);
    if let Some(parent) = parent {
        list.remove_many_from_group(&parent, ids).map_err(|error| error.to_string())?;
        for id in ids {
            if let Some(cue) = list.get_mut(id) {
                stop(cue);
            }
        }
    } else {
        for mut cue in list.remove_many_anywhere(ids).map_err(|error| error.to_string())? {
            stop(cue.as_mut());
        }
    }
    history.push_action(list_id, before);
    ws.mark_modified();
    Ok(true)
}

fn stop_removed_subtree(cue: &mut dyn Cue, context: &crate::cue::context::CueContext) {
    if let Some(children) = cue.child_cues_mut() {
        for child in children {
            stop_removed_subtree(child.as_mut(), context);
        }
    }
    if cue.is_running() || cue.is_paused() || cue.playing_voice_id().is_some() {
        let _ = cue.stop(context);
    }
}

/// Move a group of cues immediately before `before_id`, or to the end if `None`.
#[tauri::command]
pub fn move_cues(
    ids: Vec<String>,
    before_id: Option<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    if ids.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = ids
        .iter()
        .map(|s| s.parse::<Uuid>().map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let before_id: Option<Uuid> = before_id
        .as_deref()
        .map(|s| s.parse::<Uuid>().map_err(|e| e.to_string()))
        .transpose()?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    edit_cue_structure(&mut ws, &mut history, |list| list.move_before(&ids, before_id))?;
    drop(history);
    drop(ws);
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Capture the exact list prior to a structural edit that either succeeds
/// completely or leaves it untouched. The caller holds workspace and history
/// in that order through the edit, so a review write cannot split the pair.
fn edit_cue_structure<T>(
    workspace: &mut Workspace,
    history: &mut UndoHistory,
    edit: impl FnOnce(&mut CueList) -> anyhow::Result<T>,
) -> Result<T, String> {
    edit_cue_list(workspace, history, |list| edit(list).map(|result| (result, true)))
}

/// Apply one valid edit and record history only when it changed the list.
fn edit_cue_list<T>(
    workspace: &mut Workspace,
    history: &mut UndoHistory,
    edit: impl FnOnce(&mut CueList) -> anyhow::Result<(T, bool)>,
) -> Result<T, String> {
    let list = workspace.active_cue_list_mut().ok_or("No active cue list")?;
    let before = super::undo_cmds::take_snapshot(list);
    let (result, changed) = edit(list).map_err(|error| error.to_string())?;
    if changed {
        history.push_action(list.id, before);
        workspace.mark_modified();
    }
    Ok(result)
}

fn edit_cue_metadata(
    state: &AppState,
    app_handle: &tauri::AppHandle,
    edit: impl FnOnce(&mut CueList) -> anyhow::Result<bool>,
) -> Result<(), String> {
    let mut workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    let mut history = state.undo_stack.lock().map_err(|error| error.to_string())?;
    let changed = edit_cue_list(&mut workspace, &mut history, |list| {
        edit(list).map(|changed| (changed, changed))
    })?;
    drop(history);
    drop(workspace);
    if changed {
        let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    }
    Ok(())
}

/// Explicitly resequence every cue number in the active list (1, 2, 3…; nested
/// 3.1…). This is the on-demand "Renumber All Cues" action; it runs regardless
/// of the auto-renumber preference and is undoable.
#[tauri::command]
pub fn renumber_cues(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    edit_cue_metadata(&state, &app_handle, |list| Ok(list.renumber_all()))
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct RenumberSelectionPreview {
    workspace_id: Uuid,
    cue_list_id: Uuid,
    start: f64,
    increment: f64,
    changes: Vec<CueNumberChange>,
}

fn plan_cue_numbering(
    workspace: &Workspace,
    workspace_id: Uuid,
    cue_list_id: Uuid,
    ids: &[Uuid],
    start: f64,
    increment: f64,
) -> Result<RenumberSelectionPreview, String> {
    if workspace.instance_id() != workspace_id || workspace.active_cue_list_id != cue_list_id {
        return Err("The original workspace and cue list must be open to renumber this selection.".into());
    }
    let list = workspace.active_cue_list().ok_or("No active cue list")?;
    let other_lists = workspace.cue_lists.iter().filter(|other| other.id != list.id);
    let changes = list.plan_renumber_selected(ids, start, increment, other_lists)
        .map_err(|error| error.to_string())?;
    Ok(RenumberSelectionPreview { workspace_id, cue_list_id, start, increment, changes })
}

#[tauri::command]
pub fn preview_renumber_selected_cues(
    workspace_id: Uuid,
    cue_list_id: Uuid,
    ids: Vec<Uuid>,
    start: f64,
    increment: f64,
    state: State<'_, AppState>,
) -> Result<RenumberSelectionPreview, String> {
    let workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    plan_cue_numbering(&workspace, workspace_id, cue_list_id, &ids, start, increment)
}

/// Apply the displayed numbers as one edit after checking the current workspace.
#[tauri::command]
pub fn renumber_selected_cues(
    preview: RenumberSelectionPreview,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    let mut history = state.undo_stack.lock().map_err(|error| error.to_string())?;
    let changed = apply_renumber_preview(&mut workspace, &mut history, preview)?;
    drop(history);
    drop(workspace);
    if changed {
        let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    }
    Ok(())
}

fn apply_renumber_preview(
    workspace: &mut Workspace,
    history: &mut UndoHistory,
    preview: RenumberSelectionPreview,
) -> Result<bool, String> {
    let ids: Vec<_> = preview.changes.iter().map(|change| change.cue_id).collect();
    let current = plan_cue_numbering(
        workspace, preview.workspace_id, preview.cue_list_id, &ids, preview.start, preview.increment,
    )?;
    if current != preview {
        return Err("The cue numbers changed. Review the refreshed preview before applying.".into());
    }
    if current.changes.iter().all(|change| change.previous.as_deref() == Some(change.number.as_str())) {
        return Ok(false);
    }

    let list = workspace.active_cue_list_mut().ok_or("No active cue list")?;
    history.push_action(list.id, super::undo_cmds::take_snapshot(list));
    list.apply_renumber_plan(&current.changes);
    workspace.mark_modified();
    Ok(true)
}

/// Clear every cue number in the active list.
#[tauri::command]
pub fn clear_cue_numbers(
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    edit_cue_metadata(&state, &app_handle, |list| Ok(list.clear_all_numbers()))
}

/// Duplicate a cue (creates a copy with a new ID, inserted immediately after).
#[tauri::command]
pub fn duplicate_cue(
    cue_id: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let registry = state.registry.lock().map_err(|e| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;

    let (json, source_audio, new_to_old, flags, colors) = {
        let cue_list = ws.active_cue_list().ok_or("No active cue list")?;
        let cue = cue_list.get_recursive(&id).ok_or("Cue not found")?;
        let mut j = cue.serialize();
        // Fresh ids for the copy and (for a Group) every child.  The decoded
        // audio travels with it so the copy is playable immediately, without a
        // background re-decode.
        let new_to_old = assign_fresh_ids(&mut j);
        let flags = super::undo_cmds::remap_copied_flags(&cue_list.flagged_cues, &new_to_old);
        let colors = super::undo_cmds::remap_copied_colors(&cue_list.second_colors, &new_to_old);
        (j, collect_decoded_audio(cue), new_to_old, flags, colors)
    };

    let mut new_cue = registry.from_json(json).map_err(|e| e.to_string())?;
    apply_decoded_audio_to_copy(new_cue.as_mut(), &source_audio, &new_to_old);
    let new_id = new_cue.id().to_string();
    drop(registry);

    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    edit_cue_structure(&mut ws, &mut history, |list| {
        if list.index_of(&id).is_some() {
            list.ensure_unique_number(new_cue.as_mut());
        }
        // A copy of a nested cue stays adjacent in the same group.
        list.insert_after_anywhere(&id, new_cue)?;
        list.flagged_cues.extend(flags);
        list.second_colors.extend(colors);
        Ok(())
    })?;
    drop(history);
    drop(ws);
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(new_id)
}

/// Duplicate multiple cues, inserting each copy immediately after its own
/// source (so a copy of a cue nested in a group stays in that group).
#[tauri::command]
pub fn duplicate_cues(
    ids: Vec<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<String>, String> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let ids: Vec<Uuid> = ids
        .iter()
        .map(|s| s.parse::<Uuid>().map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;

    let registry = state.registry.lock().map_err(|e| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;

    // Collect serialised copies (paired with their source ID) and preserved
    // audio for each source cue.  Recursive lookup so group children duplicate.
    let (mut jsons, sources): (Vec<_>, Vec<_>) = {
        let cue_list = ws.active_cue_list().ok_or("No active cue list")?;
        ids.iter()
            .map(|id| {
                let cue = cue_list.get_recursive(id).ok_or_else(|| format!("Cue {id:?} not found"))?;
                Ok((cue.serialize(), (*id, collect_decoded_audio(cue))))
            })
            .collect::<Result<Vec<_>, String>>()?
            .into_iter()
            .unzip()
    };
    let maps = assign_fresh_ids_to_copies(&mut jsons);
    let source_list = ws.active_cue_list().ok_or("No active cue list")?;
    let mut copies = Vec::with_capacity(jsons.len());
    for ((json, (src_id, source_audio)), new_to_old) in jsons.into_iter().zip(sources).zip(maps) {
        let mut new_cue = registry.from_json(json).map_err(|e| e.to_string())?;
        apply_decoded_audio_to_copy(new_cue.as_mut(), &source_audio, &new_to_old);
        let flags = super::undo_cmds::remap_copied_flags(&source_list.flagged_cues, &new_to_old);
        let colors = super::undo_cmds::remap_copied_colors(&source_list.second_colors, &new_to_old);
        copies.push((src_id, new_cue, flags, colors));
    }

    drop(registry);
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    let new_ids = edit_cue_structure(&mut ws, &mut history, |list| {
        let mut new_ids = Vec::with_capacity(copies.len());
        // Every source was found above, and inserts leave the sources in place.
        for (src_id, mut new_cue, flags, colors) in copies {
            if list.index_of(&src_id).is_some() {
                list.ensure_unique_number(new_cue.as_mut());
            }
            new_ids.push(new_cue.id().to_string());
            list.insert_after_anywhere(&src_id, new_cue)?;
            list.flagged_cues.extend(flags);
            list.second_colors.extend(colors);
        }
        Ok(new_ids)
    })?;
    drop(history);
    drop(ws);
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(new_ids)
}

/// Update cue properties from a partial JSON object.
///
/// All fields present in `properties` are merged into the cue's serialised
/// form and the cue is rebuilt via the [`CueRegistry`].  This correctly
/// handles both generic trait fields (name, number, …) and type-specific
/// fields (volume_db, pan, fade_in_ms, …) without any unsafe downcasting.
#[tauri::command]
pub fn update_cue(
    cue_id: String,
    properties: serde_json::Value,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;

    // Lock order: registry first, then workspace (matches duplicate_cue).
    let registry = state.registry.lock().map_err(|e| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let list = ws.active_cue_list().ok_or("No active cue list")?;
    let list_id = list.id;
    let snapshot = super::undo_cmds::take_snapshot(list);
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    if !apply_cue_properties(
        state.audio_engine.as_ref(), state.output_engine.as_ref(), &registry,
        &mut ws, id, &properties, MergePolicy::AllKeys,
    )? {
        return Ok(());
    }
    history.push_action(list_id, snapshot);
    ws.mark_modified();
    drop(history);
    drop(ws);

    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Which keys of a property patch [`apply_cue_properties`] may write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergePolicy {
    /// Every key is merged — the inspector knows the cue it edits.
    AllKeys,
    /// Only keys the cue already serialises are merged, so one patch can be
    /// sent to a mixed selection and land only where it means something.
    KnownKeysOnly,
}

/// Merge `properties` into a cue's serialised form under `policy`.
///
/// Returns `true` when at least one key was written.
pub fn merge_properties(
    target: &mut serde_json::Value,
    properties: &serde_json::Value,
    policy: MergePolicy,
) -> bool {
    let (Some(target), Some(source)) = (target.as_object_mut(), properties.as_object()) else {
        return false;
    };
    let mut merged = false;
    for (key, value) in source {
        if key == "layer_style_patch" {
            if let (Some(style), Some(edits)) = (
                target.get_mut("layer_style").and_then(serde_json::Value::as_object_mut),
                value.as_object(),
            ) {
                style.extend(edits.clone());
                merged |= !edits.is_empty();
            }
            continue;
        }
        if policy == MergePolicy::KnownKeysOnly && !target.contains_key(key) {
            continue;
        }
        target.insert(key.clone(), value.clone());
        merged = true;
    }
    merged
}

/// Merge `properties` into one cue and rebuild it through the registry,
/// pushing level, geometry and compositing edits to its playing voice.
///
/// The caller holds both locks and captures Undo before applying the edit.
/// Returns `false` when `policy` left nothing to write (the cue is untouched).
pub fn apply_cue_properties(
    audio: &dyn crate::engine::engine_traits::AudioEngineApi,
    output: &dyn crate::engine::engine_traits::OutputEngineApi,
    registry: &crate::cue::registry::CueRegistry,
    ws: &mut crate::show::workspace::Workspace,
    id: Uuid,
    properties: &serde_json::Value,
    policy: MergePolicy,
) -> Result<bool, String> {
    if properties.get("playlist_auto_shuffle").is_some() {
        return Err("Change Auto-shuffle in the Playlist inspector.".into());
    }
    if let Some(source_names) = properties.get("source_channel_names")
        .filter(|_| properties.as_object().is_some_and(|fields| fields.len() == 1))
    {
        let cue = ws.active_cue_list_mut().ok_or("No active cue list")?
            .get_mut_recursive(&id).ok_or("Cue not found")?;
        let Some(names) = cue.source_channel_names_mut() else { return Ok(false) };
        *names = serde_json::from_value(source_names.clone()).map_err(|e| e.to_string())?;
        return Ok(true);
    }

    // Snapshot the patch table before borrowing the cue list: a live matrix
    // edit needs the same channel mapping the GO path used.
    let patch_channels_by_id: Vec<(Uuid, Vec<u16>)> = ws
        .output_patches
        .iter()
        .map(|p| (p.id, p.channels.clone()))
        .collect();
    let default_patch_id = ws.default_output_patch_id;
    let cue_list = ws.active_cue_list().ok_or("No active cue list")?;

    // Serialise → merge → rebuild, working recursively so child cues inside
    // groups can also be updated from the inspector.
    let old = cue_list.get_recursive(&id).ok_or("Cue not found")?;
    if (old.is_running() || old.is_paused())
        && (old as &dyn std::any::Any).downcast_ref::<GroupCue>()
            .is_some_and(GroupCue::has_shuffled_playlist_run)
        && ["children", "group_mode", "playlist_advance_playhead"]
            .iter().any(|key| properties.get(*key).is_some())
    {
        return Err("Stop the shuffled Playlist before changing its mode or children.".into());
    }
    let mut json = old.serialize();
    let previous_visual_output = json.get("output_id").cloned();
    if !merge_properties(&mut json, properties, policy) && policy == MergePolicy::KnownKeysOnly {
        return Ok(false);
    }
    if let Some(map) = properties
        .get("point_mix").and_then(|mix| mix.get("map"))
        .filter(|map| json["type"] == "audio" && !map.is_null())
    {
        let requested = serde_json::from_value(map.clone()).map_err(|err| err.to_string())?;
        let workspace_id = properties["audio_map_workspace"].as_str().and_then(|id| id.parse().ok());
        super::audio_map_cmds::checked_map(ws, workspace_id, point_patch(&json, default_patch_id), &requested)?;
    }
    apply_point_routing(&mut json, properties, default_patch_id, &patch_channels_by_id)?;
    let visual_output_changed = previous_visual_output.as_ref() != json.get("output_id");
    let mut edited = registry.from_json(json).map_err(|e| e.to_string())?;
    let matrix_changed = ["level_matrix", "point_mix", "matrix_mirror_mono", "output_patch_id"]
        .iter().any(|key| properties.get(*key).is_some());
    if matrix_changed {
        if let Some(voice) = old.playing_voice_id() {
            let patch = edited.output_patch_id().or(default_patch_id)
                .and_then(|id| ws.output_patches.iter().find(|patch| patch.id == id));
            check_live_matrix_patch(audio, voice, patch)?;
        }
    }
    let mut output_trim_update = None;
    if properties.get("output_trim").is_some() {
        use crate::cue::audio_cue::AudioCue;
        if let Some(audio_cue) = (edited.as_ref() as &dyn std::any::Any).downcast_ref::<AudioCue>() {
            let owner = properties["output_trim_workspace"].as_str().and_then(|id| id.parse().ok());
            if owner != Some(ws.instance_id) {
                return Err("The workspace changed. Reopen its Trim tab before editing.".into());
            }
            let patch = edited.output_patch_id().or(default_patch_id)
                .and_then(|id| ws.output_patches.iter().find(|patch| patch.id == id));
            if let Some(trim) = &audio_cue.output_trim {
                let route = trim.route(patch)?;
                output_trim_update = old.playing_voice_id().map(|voice| (voice, route, trim.linear_levels()));
            } else if let (Some(voice), Some(previous)) = (old.playing_voice_id(),
                (old as &dyn std::any::Any).downcast_ref::<AudioCue>().and_then(|cue| cue.output_trim.as_ref()))
            {
                let route = previous.route(patch)
                    .map_err(|_| "Stop or unload this cue before clearing trim on a reassigned output.")?;
                output_trim_update = Some((voice, route, Vec::new()));
            }
        }
    }
    let cue_list = ws.active_cue_list_mut().ok_or("No active cue list")?;
    let old = cue_list.get_mut_recursive(&id).ok_or("Cue not found")?;
    if edited.id() != old.id() || edited.cue_type() != old.cue_type() {
        return Err("A property edit must keep the cue's identity and type.".into());
    }
    if old.playing_voice_id().is_some()
        && (old.output_patch_id() != edited.output_patch_id()
            || visual_output_changed)
    {
        return Err("Stop or unload this cue before assigning a different output.".into());
    }
    if old.uses_silent_speed_clock() && !edited.supports_silent_speed() {
        return Err("Stop or unload this Video before changing its silent speed settings.".into());
    }
    if let Some((voice, route, gains)) = output_trim_update {
        if !audio.set_voice_output_trims(voice, &route, &gains) {
            return Err("The playing voice uses a different output assignment. Stop or unload it before editing trim.".into());
        }
    }
    let previous_cutoffs = properties.get("audio_effects").map(|_| old.audio_filter_cutoffs());
    let previous_eq_gains = properties.get("audio_effects").map(|_| old.audio_eq_gains());
    inherit_playback_tree(edited.as_mut(), old);

    // A completed Fade can leave the voice at a different gain or pan from
    // its saved starting level. Publish only the parameters the operator edited.
    let gain_changed = properties.get("volume_db").is_some();
    let pan_changed = properties.get("pan").is_some();
    if gain_changed || pan_changed || matrix_changed {
        if let Some(params) = edited.live_audio_params() {
            let voice = output.video_audio_voice(params.voice_id).unwrap_or(params.voice_id);
            if gain_changed {
                let _ = audio.set_voice_gain(voice, params.gain);
            }
            if pan_changed {
                let _ = audio.set_voice_pan(voice, params.pan);
            }
            if matrix_changed {
                let channels = edited.output_patch_id().or(default_patch_id)
                    .and_then(|id| patch_channels_by_id.iter().find(|(pid, _)| *pid == id))
                    .map(|(_, channels)| channels.as_slice()).unwrap_or_default();
                let matrix = params.level_matrix.as_ref().and_then(|spec|
                    crate::cue::audio_cue::build_level_matrix(spec, channels, params.matrix_mirror_mono));
                let _ = audio.set_voice_level_matrix(voice, matrix.as_ref());
            }
        }
    }
    if edited.cue_type() == CueType::Audio {
        if let Some(voice) = edited.playing_voice_id() {
            if let Some(db) = properties.get("trim_db").and_then(serde_json::Value::as_f64) {
                audio.set_voice_trim(voice, crate::cue::types::db_to_linear(db) as f32);
            }
            if let Some(previous) = previous_cutoffs {
                for (id, cutoff) in edited.audio_filter_cutoffs() {
                    if previous.iter().any(|(old_id, old_cutoff)| *old_id == id && *old_cutoff != cutoff) {
                        if let Some(fader) = audio.voice_cutoff_fader(voice, id) {
                            fader.manual(cutoff);
                        }
                    }
                }
            }
            if let Some(previous) = previous_eq_gains {
                for (id, gain) in edited.audio_eq_gains() {
                    if previous.iter().any(|(old_id, old_gain)| *old_id == id && *old_gain != gain) {
                        if let Some(fader) = audio.voice_eq_gain_fader(voice, id) {
                            fader.manual(gain);
                        }
                    }
                }
            }
            // Only an explicit Audio Rate edit takes control from a rate Fade.
            if let Some(rate) = properties.get("rate").and_then(serde_json::Value::as_f64) {
                if let Some(fader) = audio.voice_rate_fader(voice) {
                    fader.manual(crate::engine::rate_fader::normalize_rate(rate) as f32);
                }
            }
            if properties.get("fade_out_ms").is_some() || properties.get("fade_out_curve").is_some() {
                let (duration, curve) = edited.natural_fade_out()
                    .unwrap_or((0, crate::engine::ring_command::FadeCurve::Linear));
                audio.set_voice_natural_fade_out(voice, duration, curve);
            }
        }
    }
    if let Some(voice) = edited.playing_voice_id() {
        if properties.get("geometry").is_some() {
            if let Some(geometry) = edited.visual_geometry() {
                output.apply_geometry(voice, &geometry);
            }
        }
        let replace_style = properties.get("layer_style").is_some();
        let style_edits = properties.get("layer_style_patch").and_then(serde_json::Value::as_object);
        if replace_style || style_edits.is_some() {
            if let Some(style) = edited.layer_style() {
                let edit_saturation = replace_style || style_edits.is_some_and(|edits| edits.contains_key("saturation"));
                output.set_layer_props(voice, &style, edit_saturation);
            }
        }
    }
    cue_list.replace_cue_recursive(&id, edited);
    Ok(true)
}

/// Restore the run of every matching group descendant. A changed media file
/// keeps its new decoded data; a metadata edit reuses the original samples.
fn inherit_playback_tree(edited: &mut dyn Cue, old: &mut dyn Cue) {
    if old.media_file_path() == edited.media_file_path() {
        if let Some((samples, channels, rate, duration)) = old.extract_decoded_audio() {
            edited.accept_preloaded_audio(samples, channels, rate, duration);
        }
    }
    edited.inherit_playback(old);
    if edited.is_disabled() {
        edited.cancel_loaded_time();
    }
    if let (Some(children), Some(previous)) = (edited.child_cues_mut(), old.child_cues_mut()) {
        let mut previous: std::collections::HashMap<_, _> =
            previous.iter_mut().map(|cue| (cue.id(), cue)).collect();
        for child in children {
            if let Some(old) = previous.remove(&child.id()) {
                if child.cue_type() == old.cue_type() {
                    inherit_playback_tree(child.as_mut(), old.as_mut());
                }
            }
        }
    }
}

pub(super) fn check_live_matrix_patch(
    audio: &dyn crate::engine::engine_traits::AudioEngineApi,
    voice: crate::engine::ring_command::VoiceId,
    patch: Option<&crate::engine::device_manager::OutputPatch>,
) -> Result<(), String> {
    if let Some(route) = audio.named_voice_output(voice) {
        if !patch.is_some_and(|patch| route.matches_patch(patch)) {
            return Err("The playing cue uses a different output assignment. Stop or unload it before changing the matrix.".into());
        }
    }
    Ok(())
}

/// Set the Playhead to a specific cue.
#[tauri::command]
pub fn set_playhead(
    cue_id: Option<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Option<Uuid> = cue_id
        .as_deref()
        .map(|s| s.parse::<Uuid>().map_err(|e| e.to_string()))
        .transpose()?;

    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let cue_list = ws.active_cue_list_mut().ok_or("No active cue list")?;

    set_playhead_in_list(cue_list, id, &app_handle)
}

/// Park standby by exact number in the workspace and list that opened the dialog.
#[tauri::command]
pub fn go_to_cue_number(
    workspace_id: Uuid,
    list_id: Uuid,
    number: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Uuid, String> {
    let mut workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    if workspace.instance_id() != workspace_id
        || workspace.active_cue_list().map(|list| list.id) != Some(list_id)
    {
        return Err("The workspace or active cue list changed. Reopen Go to cue number.".into());
    }
    let list = workspace.active_cue_list_mut().ok_or("No active cue list")?;
    let id = list.goto_number_target(number.trim()).map_err(|error| error.to_string())?;
    set_playhead_in_list(list, Some(id), &app_handle)?;
    Ok(id)
}

pub(super) fn set_playhead_in_list(
    cue_list: &mut crate::show::cue_list::CueList,
    id: Option<Uuid>,
    app_handle: &tauri::AppHandle,
) -> Result<(), String> {
    cue_list.set_playhead(id).map_err(|e| e.to_string())?;
    // The resulting outer Playhead may differ from `id` (a group child parks the
    // Playhead on its ancestor group), so emit the actual outer Playhead.
    let outer_playhead = cue_list.playhead_cue_id;

    let _ = app_handle.emit(
        "playhead-moved",
        serde_json::json!({ "cue_id": outer_playhead.map(|u| u.to_string()) }),
    );
    // Only when the target was a nested cue (resolved to a different outer
    // Playhead) refresh cues so the group's inner playhead (active_child_id)
    // updates — top-level clicks don't need the extra round-trip.
    if id != outer_playhead {
        let _ = app_handle.emit("cue-list-refresh", serde_json::json!({}));
    }
    Ok(())
}

/// Return the current Playhead cue ID (or null).
#[tauri::command]
pub fn get_playhead(state: State<'_, AppState>) -> Result<Option<String>, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let cue_list = ws.active_cue_list().ok_or("No active cue list")?;
    Ok(cue_list.playhead_cue_id.map(|u| u.to_string()))
}

// ---------------------------------------------------------------------------
// Waveform
// ---------------------------------------------------------------------------

/// Downsampled peak data returned by [`get_waveform_peaks`].
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WaveformData {
    /// Peak values (0.0 – 1.0), one per bin.
    pub peaks: Vec<f32>,
    /// RMS values (0.0 – 1.0), one per bin — the "body" of the sound, drawn
    /// inside the peak envelope for a two-tone DAW-style waveform.
    pub rms: Vec<f32>,
    /// Full file duration in seconds (ignoring start/end markers).
    pub file_duration_s: f64,
}

/// Downsample interleaved samples into per-bin peak + RMS values.
fn compute_waveform_bins(samples: &[f32], channels: usize, bins: usize) -> (Vec<f32>, Vec<f32>) {
    let total_frames = samples.len().checked_div(channels.max(1)).unwrap_or(0);
    if bins == 0 || total_frames == 0 {
        return (vec![], vec![]);
    }
    let mut peaks = Vec::with_capacity(bins);
    let mut rms = Vec::with_capacity(bins);
    for i in 0..bins {
        let start = (i * total_frames) / bins;
        let end = (((i + 1) * total_frames) / bins).max(start + 1).min(total_frames);
        let mut peak = 0.0f32;
        let mut sum_sq = 0.0f64;
        let mut count = 0usize;
        for frame in start..end {
            for ch in 0..channels {
                let v = samples[frame * channels + ch];
                let a = v.abs();
                if a > peak {
                    peak = a;
                }
                sum_sq += (v as f64) * (v as f64);
                count += 1;
            }
        }
        peaks.push(peak);
        rms.push(if count > 0 { (sum_sq / count as f64).sqrt() as f32 } else { 0.0 });
    }
    (peaks, rms)
}

/// Return waveform peak data for an audio cue.
///
/// `bins` controls the number of columns (typically 400–800 for UI use).
/// Returns an error if the cue has not been decoded yet.
#[tauri::command]
pub fn get_waveform_peaks(
    cue_id: String,
    bins: usize,
    state: State<'_, AppState>,
) -> Result<WaveformData, String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;

    // Hold the workspace lock only long enough to clone the decoded audio Arc.
    // Peak computation can be expensive (large files) so we do it AFTER releasing
    // the lock so concurrent GO commands are never blocked.
    let (samples, channels, _sample_rate, file_duration) = {
        let ws = state.workspace.lock().map_err(|e| e.to_string())?;
        let cue_list = ws.active_cue_list().ok_or("No active cue list")?;
        let cue = cue_list.get_recursive(&id).ok_or("Cue not found")?;
        cue.extract_decoded_audio()
            .ok_or("Audio not loaded yet — assign a file first")?
        // workspace lock dropped here
    };

    // Compute peaks + RMS outside the lock.
    let (peaks, rms) = compute_waveform_bins(&samples, channels as usize, bins);

    Ok(WaveformData {
        peaks,
        rms,
        file_duration_s: file_duration.as_secs_f64(),
    })
}

/// Push a level change straight to a playing cue's voice, **without** touching
/// the workspace or the undo stack.
///
/// This is the drag path: an inspector slider calls it continuously while it
/// moves, then persists once on release via `update_cue`. Routing every drag
/// step through `update_cue` instead would re-serialise the cue and push an
/// undo snapshot per pixel.
///
/// A no-op when the cue is not playing.
#[tauri::command]
pub fn set_live_level(
    cue_id: String,
    volume_db: Option<f64>,
    pan: Option<f32>,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let Some(cue_list) = ws.active_cue_list() else { return Ok(()) };
    let Some(cue) = cue_list.get_recursive(&id) else { return Ok(()) };
    let Some(params) = cue.live_audio_params() else { return Ok(()) };

    // A Video Cue reports its visual voice; the sound is on the paired one.
    let voice = state
        .output_engine
        .video_audio_voice(params.voice_id)
        .unwrap_or(params.voice_id);
    if let Some(db) = volume_db {
        let _ = state.audio_engine.set_voice_gain(voice, crate::cue::types::db_to_linear(db) as f32);
    }
    if let Some(pan) = pan {
        let _ = state.audio_engine.set_voice_pan(voice, pan);
    }
    Ok(())
}

/// Push one crosspoint of a playing cue's level matrix, live and without an
/// undo snapshot — the matrix-grid equivalent of [`set_live_level`].
///
/// Sends a single cell rather than the whole matrix: dragging one cell would
/// otherwise flood the real-time ring buffer with 32 commands per step.
#[tauri::command]
pub fn set_live_crosspoint(
    cue_id: String,
    input: u8,
    output: u8,
    gain_db: f64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let Some(cue_list) = ws.active_cue_list() else { return Ok(()) };
    let Some(cue) = cue_list.get_recursive(&id) else { return Ok(()) };
    let Some(params) = cue.live_audio_params() else { return Ok(()) };

    // Resolve the column onto the patch channel, exactly as the GO path does.
    let patch = cue
        .output_patch_id()
        .or(ws.default_output_patch_id)
        .and_then(|pid| ws.output_patches.iter().find(|p| p.id == pid));
    check_live_matrix_patch(state.audio_engine.as_ref(), params.voice_id, patch)?;
    let patch_channels = patch.map(|patch| patch.channels.as_slice()).unwrap_or_default();
    let physical = if patch_channels.is_empty() {
        u16::from(output)
    } else {
        *patch_channels.get(usize::from(output)).ok_or("Output is outside the assigned patch")?
    };
    if usize::from(physical) >= crate::engine::voice::MATRIX_OUTPUTS { return Err("Patch output exceeds 128 channels".into()); }
    let device = physical as u8;
    let mut gain = crate::cue::types::db_to_linear(gain_db) as f32;
    if let Some(row) = params.level_matrix.as_ref().and_then(|m| m.get(usize::from(input))) {
        for (column, &route) in patch_channels.iter().enumerate() {
            if route == physical && column != usize::from(output) {
                if let Some(&db) = row.get(column) { gain += crate::cue::types::db_to_linear(db) as f32; }
            }
        }
    }
    let voice = state
        .output_engine
        .video_audio_voice(params.voice_id)
        .unwrap_or(params.voice_id);
    state
        .audio_engine
        .set_voice_crosspoint(voice, input, device, gain, params.matrix_mirror_mono)
        .map_err(|e| e.to_string())
}

/// Compute the `volume_db` that normalises this audio cue's peak to 0 dBFS.
///
/// Reads the already-decoded samples (non-destructively via `Arc::clone`),
/// finds the absolute peak, and returns `20 × log10(1 / peak)` — the gain
/// the fader must be set to so the loudest sample plays at exactly 0 dBFS.
///
/// Errors if the audio has not been decoded yet or if the file is silent.
#[tauri::command]
pub fn get_normalize_db(
    cue_id: String,
    state: State<'_, AppState>,
) -> Result<f64, String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;

    let samples = {
        let ws = state.workspace.lock().map_err(|e| e.to_string())?;
        let cue_list = ws.active_cue_list().ok_or("No active cue list")?;
        let cue = cue_list.get_recursive(&id).ok_or("Cue not found")?;
        cue.extract_decoded_audio()
            .ok_or("Audio not loaded yet — open the file first")?
            .0
        // workspace lock released here
    };

    let peak: f32 = samples.iter().map(|s| s.abs()).fold(0.0_f32, f32::max);

    if peak < 1e-6 {
        return Err("File is silent — cannot normalize".into());
    }

    let normalize_db = (1.0_f64 / peak as f64).log10() * 20.0;
    Ok(normalize_db.clamp(-60.0, 12.0))
}

/// Stop `cue` first when it is currently running or paused.
///
/// The file setters replace the cue object wholesale (serialise → rebuild), so
/// a live cue would otherwise lose its `active_voice_id` and its engine voice
/// would keep playing with no owner — unreachable by Stop or even Hard Stop
/// (the rebuilt cue reports Standby, so the transport skips it).  Uses the
/// soft stop so the operator hears the normal fade-out; the engine completes
/// the fade autonomously after the cue object is replaced.
pub(super) fn stop_if_live(cue: &mut dyn Cue, state: &AppState, stop_fade_ms: u32) {
    if cue.is_running() || cue.is_paused() {
        let context = super::transport_cmds::stop_context(state, stop_fade_ms);
        let _ = cue.stop(&context);
    }
}

/// Swap `file_path` into a serialized cue and, when the path actually
/// changed, reset the clip window — start/end times and slice markers
/// describe positions in the *old* media and break playback on a shorter
/// file (start time past EOF = silent audio; ab-loop segments past the end
/// = video looping with Loop unchecked).  The cached duration is cleared
/// too so a stale length never outlives the file it measured.
fn set_file_path_resetting_clip(json: &mut serde_json::Value, file_path: &str) {
    let Some(obj) = json.as_object_mut() else { return };
    let changed = obj.get("file_path").and_then(|v| v.as_str()) != Some(file_path);
    obj.insert("file_path".to_string(), serde_json::json!(file_path));
    if changed {
        obj.insert("start_time_ms".to_string(), serde_json::Value::Null);
        obj.insert("end_time_ms".to_string(), serde_json::Value::Null);
        obj.insert(
            "slices".to_string(),
            serde_json::json!({ "markers": [], "play_counts": [1] }),
        );
        obj.insert("cached_duration_ms".to_string(), serde_json::Value::Null);
    }
}

#[derive(Clone, Copy)]
struct FileAssignment {
    workspace_id: Uuid,
    cue_id: Uuid,
    request_id: Uuid,
}

fn replace_cue_file(
    state: &AppState,
    id: Uuid,
    cue_type: CueType,
    file_path: &str,
) -> Result<FileAssignment, String> {
    let registry = state.registry.lock().map_err(|e| e.to_string())?;
    let mut workspace = state.workspace.lock().map_err(|e| e.to_string())?;
    let assignment = FileAssignment {
        workspace_id: workspace.instance_id,
        cue_id: id,
        request_id: Uuid::new_v4(),
    };
    let stop_fade_ms = workspace.preferences.audio.default_fade_out_ms;
    let list = workspace.active_cue_list_mut().ok_or("No active cue list")?;
    let cue = list.get_recursive(&id).ok_or("Cue not found")?;
    if cue.cue_type() != cue_type {
        return Err(format!("The selected cue is not a {cue_type} cue."));
    }

    let mut json = cue.serialize();
    let needs_preload = matches!(cue_type, CueType::Audio | CueType::Video);
    if needs_preload {
        set_file_path_resetting_clip(&mut json, file_path);
    } else if let Some(fields) = json.as_object_mut() {
        fields.insert("file_path".to_owned(), serde_json::json!(file_path));
    }
    let replacement = registry.from_json(json).map_err(|e| e.to_string())?;
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    let mut loading = needs_preload
        .then(|| state.loading_cues.lock().map_err(|e| e.to_string()))
        .transpose()?;
    let before = super::undo_cmds::take_snapshot(list);
    let cue = list.get_mut_recursive(&id).ok_or("Cue not found")?;
    stop_if_live(cue, state, stop_fade_ms);
    list.replace_cue_recursive(&id, replacement);
    if let Some(loading) = &mut loading {
        loading.begin(id, assignment.request_id);
    }
    history.push_action(list.id, before);
    workspace.mark_modified();
    Ok(assignment)
}

/// Only the latest direct assignment to this cue in this open workspace can
/// retire its loading marker or publish decoded data and errors.
fn finish_file_assignment(
    workspace: &Mutex<Workspace>,
    loading_cues: &Mutex<LoadingCues>,
    assignment: FileAssignment,
    path: &Path,
    expected_type: CueType,
    apply: impl FnOnce(&mut dyn Cue),
) -> bool {
    let Ok(mut workspace) = workspace.lock() else { return false };
    if workspace.instance_id != assignment.workspace_id {
        return false;
    }
    let Ok(mut loading) = loading_cues.lock() else { return false };
    if !loading.finish(assignment.cue_id, assignment.request_id) {
        return false;
    }
    // A cue can move to another list while decoding. A replacement by another
    // editor must still match the assigned path before accepting this result.
    let cue = workspace.cue_lists.iter_mut()
        .find_map(|list| list.get_mut_recursive(&assignment.cue_id));
    if let Some(cue) = cue.filter(|cue| {
        cue.cue_type() == expected_type && super::cue_file_path_still(&**cue, path)
    }) {
        apply(cue);
    }
    true
}

/// Set the file path of an audio cue.
/// Uses the same JSON-merge-and-rebuild strategy as [`update_cue`].
#[tauri::command]
pub fn set_audio_file(
    cue_id: String,
    file_path: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let assignment = replace_cue_file(&state, id, CueType::Audio, &file_path)?;

    // Notify the frontend: filename appears in Target column, loading indicator shows.
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));

    // Spawn a background thread to decode the audio file and push the result
    // back via accept_preloaded_audio.  The workspace mutex is only held for
    // the brief moment needed to store the decoded samples — never during I/O.
    let workspace = state.workspace.clone();
    let loading_cues = state.loading_cues.clone();
    let app_handle2 = app_handle.clone();
    let file_path_buf = std::path::PathBuf::from(&file_path);

    let worker = std::thread::Builder::new()
        .name("inkue-preload".into())
        .spawn(move || {
            // Run at below-normal OS priority so decoding never starves the
            // audio callback thread or causes fan spin-up on the host machine.
            // The decode is I/O + CPU bound; at BELOW_NORMAL it finishes in the
            // same wall-clock time when the system is otherwise idle, but yields
            // automatically under load.
            #[cfg(windows)]
            // SAFETY: only changes the scheduling priority of this thread.
            unsafe {
                use std::os::raw::c_void;
                extern "system" {
                    fn GetCurrentThread() -> *mut c_void;
                    fn SetThreadPriority(h_thread: *mut c_void, n_priority: i32) -> i32;
                }
                SetThreadPriority(GetCurrentThread(), -1); // THREAD_PRIORITY_BELOW_NORMAL
            }

            let decoded = crate::cue::audio_cue::AudioCue::decode_file(&file_path_buf);
            let retired = finish_file_assignment(
                &workspace, &loading_cues, assignment, &file_path_buf, CueType::Audio,
                |cue| match decoded {
                    Ok((samples, channels, sample_rate)) => {
                        let duration = std::time::Duration::from_secs_f64(
                            samples.len() as f64 / channels.max(1) as f64 / sample_rate.max(1) as f64,
                        );
                        cue.accept_preloaded_audio(Arc::new(samples), channels, sample_rate, duration);
                        super::clear_decode_failure(id);
                    }
                    Err(error) => {
                        log::warn!("Background preload failed for {:?}: {error}", file_path_buf);
                        super::surface_decode_failure(id, &file_path_buf);
                        let _ = app_handle2.emit("cue-load-error", serde_json::json!({
                            "cue_id": id.to_string(),
                            "error": error.to_string(),
                        }));
                    }
                },
            );
            if retired {
                let _ = app_handle2.emit("workspace-modified", ());
            }
        });

    if let Err(error) = worker {
        let retired = finish_file_assignment(
            &state.workspace, &state.loading_cues, assignment, Path::new(&file_path), CueType::Audio,
            |_| super::surface_decode_failure(id, Path::new(&file_path)),
        );
        if retired {
            let _ = app_handle.emit("workspace-modified", ());
        }
        return Err(format!("The file was assigned, but its audio loader could not start: {error}"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Waveform preview (plays a temporary voice without touching cue state)
// ---------------------------------------------------------------------------

fn scoped_audition_cue(
    ws: &crate::show::Workspace, owner: GroupTimelineOwner, cue_id: Uuid,
) -> Result<&dyn Cue, String> {
    if ws.instance_id() != owner.workspace_id {
        return Err("The workspace changed before audition started.".into());
    }
    let cue = ws.cue_list_by_id(owner.list_id).and_then(|list| list.get_recursive(&cue_id))
        .ok_or("The cue is no longer in the selected list.")?;
    if cue.cue_type() != CueType::Audio {
        return Err("Choose an Audio cue for clip audition.".into());
    }
    Ok(cue)
}

/// Monitor a trimmed audio clip independently of cue execution and the live mixer.
#[tauri::command]
pub async fn preview_cue(
    cue_id: String, request_id: Uuid, device_id: String, monitor_db: f32,
    start_ms: Option<u64>, end_ms: Option<u64>, audio_fx: Option<bool>, owner: Option<GroupTimelineOwner>,
    state: State<'_, AppState>,
) -> Result<String, String> {
    if !cfg!(target_os = "linux") {
        return Err("Dedicated clip audition currently requires Linux PipeWire.".into());
    }
    if !monitor_db.is_finite() || !(-60.0..=0.0).contains(&monitor_db) {
        return Err("Choose a headphone level between −60 and 0 dB.".into());
    }
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let owner = {
        let ws = state.workspace.lock().map_err(|e| e.to_string())?;
        let owner = owner.unwrap_or(GroupTimelineOwner { workspace_id: ws.instance_id(), list_id: ws.active_cue_list_id });
        scoped_audition_cue(&ws, owner, id)?;
        owner
    };
    state.audio_engine.reserve_audition(request_id)?;
    let target = device_id.clone();
    tauri::async_runtime::spawn_blocking(move || {
        crate::engine::device_manager::run_bounded(std::time::Duration::from_secs(1), move || {
            crate::engine::device_manager::check_audition_output(&target)
        }).unwrap_or_else(|| Err("The PipeWire output check timed out. Refresh outputs and retry.".into()))
    }).await.map_err(|e| e.to_string())??;

    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let cue = scoped_audition_cue(&ws, owner, id)?;
    let audio = (cue as &dyn std::any::Any).downcast_ref::<AudioCue>()
        .ok_or("Choose an Audio cue for clip audition.")?;
    let (samples, channels, sample_rate, _) = cue.extract_decoded_audio().ok_or("The audio file has not finished loading.")?;
    let json = cue.serialize();
    let volume_db = json.get("volume_db").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let clip = AuditionClip {
        samples, channels: channels.into(), sample_rate,
        start_ms: start_ms.or_else(|| json["start_time_ms"].as_u64()).unwrap_or(0),
        end_ms: end_ms.or_else(|| json["end_time_ms"].as_u64()),
        gain: (crate::cue::types::db_to_linear(volume_db)
            * crate::cue::types::db_to_linear(f64::from(monitor_db))) as f32,
        pan: json["pan"].as_f64().unwrap_or(0.0) as f32,
    };
    let patches = ws.output_patches.iter().map(|p| p.device_id.as_str());
    let result = if audio_fx.unwrap_or(false) {
        state.audio_engine.start_audition_with_effects(request_id, &device_id, patches, clip, audio.effects().to_vec())
    } else {
        state.audio_engine.start_audition(request_id, &device_id, patches, clip)
    };
    result.map(|id| id.to_string())
}

#[tauri::command]
pub fn stop_preview(voice_id: Uuid, state: State<'_, AppState>) {
    state.audio_engine.stop_audition(Some(voice_id));
}

#[tauri::command]
pub fn audition_running(voice_id: Uuid, state: State<'_, AppState>) -> Result<bool, String> {
    state.audio_engine.audition_running(voice_id)
}

// ---------------------------------------------------------------------------
// Video file management
// ---------------------------------------------------------------------------

/// Set the file path of a Video Cue.
///
/// The picture streams at GO. Probe its duration and prepare its audio track
/// in the background so both are available to the cue.
#[tauri::command]
pub fn set_video_file(
    cue_id: String,
    file_path: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let assignment = replace_cue_file(&state, id, CueType::Video, &file_path)?;
    let _ = app_handle.emit("workspace-modified", ());

    // Probe the video duration (mpv) and decode the audio track (symphonia) so
    // the cue shows its length and has synced audio ready before the first GO.
    let path = std::path::PathBuf::from(&file_path);
    let output_engine = Arc::clone(&state.output_engine);
    let workspace = Arc::clone(&state.workspace);
    let loading_cues = state.loading_cues.clone();
    let handle = app_handle.clone();
    let worker = std::thread::Builder::new()
        .name("inkue-video-load".into())
        .spawn(move || {
            let duration = output_engine
                .try_mpv_lib()
                .and_then(|lib| crate::engine::OutputEngine::probe_duration(lib, &path));
            let audio = crate::cue::media_decode::decode_audio_track(&path);
            let retired = finish_file_assignment(
                &workspace, &loading_cues, assignment, &path, CueType::Video,
                |cue| {
                    if let Some(duration) = duration {
                        cue.set_runtime_duration(duration);
                    }
                    match audio {
                        Ok(Some((samples, channels, sample_rate))) => {
                            let duration = std::time::Duration::from_secs_f64(
                                samples.len() as f64 / channels.max(1) as f64 / sample_rate.max(1) as f64,
                            );
                            cue.accept_preloaded_audio(Arc::new(samples), channels, sample_rate, duration);
                        }
                        Ok(None) => VideoCue::mark_silent_track(cue),
                        Err(error) => log::warn!("Video audio decode failed for {path:?}: {error}"),
                    }
                },
            );
            if retired {
                let _ = handle.emit("workspace-modified", ());
            }
        });
    if let Err(error) = worker {
        let retired = finish_file_assignment(
            &state.workspace, &state.loading_cues, assignment, Path::new(&file_path), CueType::Video,
            |_| {},
        );
        if retired {
            let _ = app_handle.emit("workspace-modified", ());
        }
        return Err(format!("The file was assigned, but its video loader could not start: {error}"));
    }
    Ok(())
}

/// Return the list of connected monitors for the Screen selector in the inspector.
#[tauri::command]
pub fn list_video_screens(
    state: tauri::State<crate::state::AppState>,
) -> Vec<crate::engine::output_engine::ScreenInfo> {
    state.output_engine.list_screens()
}

/// Enumerate connected cameras / capture devices for the Camera Cue inspector.
///
/// Async + `spawn_blocking`: device enumeration can stall on a flaky driver,
/// and Tauri runs sync commands on the main thread (same rule as
/// `list_audio_devices` — see 1.1.5).
#[tauri::command]
pub async fn list_camera_devices(
) -> Result<Vec<crate::engine::camera_enum::CameraDeviceInfo>, String> {
    tauri::async_runtime::spawn_blocking(crate::engine::camera_enum::list_camera_devices)
        .await
        .map_err(|e| e.to_string())
}

/// Monotonic token so a rapid second Identify cancels the first one's cleanup.
static IDENTIFY_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Flash a big identification label on `screen`, using an output's window
/// (`output_id` absent = the main output), so the operator can verify before a
/// show that "Screen 2" really is the projector.  Cleans up after ~2.5 s: the
/// label is removed and the window goes back to where the show wants it (or is
/// hidden again if it was hidden before).
#[tauri::command]
pub fn identify_output_screen(
    screen_index: Option<u32>,
    output_id: Option<String>,
    state: tauri::State<crate::state::AppState>,
) -> Result<(), String> {
    use std::sync::atomic::Ordering;

    let output = super::output_cmds::parse_output_id(output_id.as_deref())?;
    let engine = std::sync::Arc::clone(&state.output_engine);
    let was_visible = engine.is_output_visible();
    let generation = IDENTIFY_GENERATION.fetch_add(1, Ordering::Relaxed) + 1;

    let screen_label = match screen_index {
        Some(idx) => format!("SCREEN {}", idx + 1),
        None => "OUTPUT WINDOW".to_string(),
    };
    let ass = format!(
        "{{\\an5\\fs140\\bord6\\1c&H00FFFFFF&\\3c&H00000000&}}{screen_label}\\N{{\\fs42}}{}",
        engine.output_name(output),
    );
    engine.identify_screen(output, screen_index, &ass).map_err(|e| e.to_string())?;

    std::thread::Builder::new()
        .name("inkue-identify-screen".into())
        .spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(2500));
            // A newer Identify owns the overlay now — let its cleanup handle it.
            if IDENTIFY_GENERATION.load(Ordering::Relaxed) != generation {
                return;
            }
            engine.end_identify(output, was_visible);
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Image file management
// ---------------------------------------------------------------------------

/// Set the file path of an Image Cue.
///
/// Unlike [`set_audio_file`], no background decoding is needed — the image is
/// passed to the OutputEngine at GO time via mpv loadfile.
#[tauri::command]
pub fn set_image_file(
    cue_id: String,
    file_path: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    replace_cue_file(&state, id, CueType::Image, &file_path)?;
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

// ---------------------------------------------------------------------------
// MIDI file management
// ---------------------------------------------------------------------------

/// Set the file path of a MIDI File Cue.
///
/// The rebuild parses the new file, so the cue's duration is correct by the
/// time the command returns — MIDI files are small enough that no background
/// decode is warranted.
#[tauri::command]
pub fn set_midi_file(
    cue_id: String,
    file_path: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    replace_cue_file(&state, id, CueType::MidiFile, &file_path)?;
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Return a `data:` URL thumbnail for a media file (image or video), for the
/// inspector's Media preview. Generated headlessly via libmpv (`vo=image`) and
/// cached on disk, so only the first request per file decodes anything.
///
/// `seek_into` picks a frame ~15 % in (videos — frame 0 is often black).
/// Async + `spawn_blocking`: decode can take a few hundred ms and sync
/// commands run on the main thread.
#[tauri::command]
pub async fn get_media_thumbnail(
    path: String,
    seek_into: bool,
    state: State<'_, AppState>,
) -> Result<String, String> {
    let lib = state
        .output_engine
        .try_mpv_lib_arc()
        .ok_or_else(|| crate::engine::output_engine::NO_VIDEO_OUTPUT.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        crate::engine::thumbnails::media_thumbnail(&lib, std::path::Path::new(&path), seek_into)
            .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Return a filmstrip for the video trimmer: `tiles` JPEG `data:` URLs evenly
/// spread across the file. Same headless-libmpv path and disk cache as
/// [`get_media_thumbnail`].
#[tauri::command]
pub async fn get_video_filmstrip(
    path: String,
    tiles: usize,
    tile_width: u32,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let lib = state
        .output_engine
        .try_mpv_lib_arc()
        .ok_or_else(|| crate::engine::output_engine::NO_VIDEO_OUTPUT.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        crate::engine::thumbnails::video_filmstrip(
            &lib,
            std::path::Path::new(&path),
            tiles,
            tile_width,
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Filmstrip over a time range (zoomed clip editor): `tiles` frames spread
/// across `[start_s, end_s]`.  Same cache/headless-mpv path as the full strip.
#[tauri::command]
pub async fn get_video_filmstrip_range(
    path: String,
    start_s: f64,
    end_s: f64,
    tiles: usize,
    tile_width: u32,
    state: State<'_, AppState>,
) -> Result<Vec<String>, String> {
    let lib = state
        .output_engine
        .try_mpv_lib_arc()
        .ok_or_else(|| crate::engine::output_engine::NO_VIDEO_OUTPUT.to_string())?;
    tauri::async_runtime::spawn_blocking(move || {
        crate::engine::thumbnails::video_filmstrip_range(
            &lib,
            std::path::Path::new(&path),
            start_s,
            end_s,
            tiles,
            tile_width,
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Toggle the output window visibility (F9 / View menu).
#[tauri::command]
pub fn toggle_output_window(state: State<'_, AppState>) {
    state.output_engine.toggle_visibility();
}

/// Return whether the output window is currently visible.
#[tauri::command]
pub fn get_output_window_visible(state: State<'_, AppState>) -> bool {
    state.output_engine.is_visible()
}

// ---------------------------------------------------------------------------
// Group Cue commands
// ---------------------------------------------------------------------------

/// Wrap the given cues in a new Group Cue inserted at the first selected position.
/// Returns the new Group's ID.
#[tauri::command]
pub fn group_cues(
    ids: Vec<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let ids: Vec<Uuid> = ids
        .iter()
        .map(|s| s.parse::<Uuid>().map_err(|e| e.to_string()))
        .collect::<Result<_, _>>()?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    let group_id = edit_cue_structure(&mut ws, &mut history, |list| {
        anyhow::ensure!(ids.iter().all(|id| list.index_of(id).is_some()),
            "Only selected top-level cues can be grouped together");
        list.group_cues(&ids)
    })?;
    drop(history);
    drop(ws);
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(group_id.to_string())
}

/// Dissolve a Group: move its children into the parent list and remove the Group.
#[tauri::command]
pub fn ungroup(
    group_id: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = group_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let mut workspace = state.workspace.lock().map_err(|e| e.to_string())?;
    let cue_list = workspace.active_cue_list_mut().ok_or("No active cue list")?;
    let before = super::undo_cmds::take_snapshot(cue_list);
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    cue_list.ungroup(&id).map_err(|e| e.to_string())?;
    history.push_action(cue_list.id, before);
    workspace.mark_modified();
    drop(history);
    drop(workspace);
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Change the playback mode of a Group Cue.
#[tauri::command]
pub fn set_group_mode(
    group_id: String,
    mode: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = group_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let group_mode: GroupMode = serde_json::from_value(serde_json::json!(mode))
        .map_err(|_| format!("Unknown group mode: {mode}"))?;
    edit_cue_metadata(&state, &app_handle, |list| {
        let cue = list.get_mut_recursive(&id).ok_or_else(|| anyhow::anyhow!("Group cue not found"))?;
        let current = cue.group_mode().ok_or_else(|| anyhow::anyhow!("Select a Group cue to set its mode"))?;
        if current == group_mode {
            return Ok(false);
        }
        if (matches!(current, GroupMode::StartFirst | GroupMode::StartFirstAndEnter)
            || matches!(group_mode, GroupMode::StartFirst | GroupMode::StartFirstAndEnter | GroupMode::Playlist)
            || cue.playlist_advance_playhead() == Some(true))
            && (cue.is_running() || cue.is_paused())
        {
            anyhow::bail!("Stop the group before changing its playback mode");
        }
        cue.set_group_mode(group_mode);
        Ok(true)
    })
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct GroupChildEntranceEdit {
    workspace_id: Uuid,
    list_id: Uuid,
    group_id: Uuid,
    child_id: Uuid,
    expected_pre_wait_ms: u64,
    pre_wait_ms: u64,
}

fn edit_group_child_entrance(
    workspace: &mut Workspace,
    history: &mut UndoHistory,
    edit: GroupChildEntranceEdit,
) -> Result<bool, String> {
    const MAX_PRE_WAIT_MS: u64 = 86_400_000;
    if workspace.instance_id() != edit.workspace_id
        || workspace.active_cue_list().map(|list| list.id) != Some(edit.list_id)
    {
        return Err("The workspace or active cue list changed. Reopen the Timeline.".into());
    }
    if edit.pre_wait_ms > MAX_PRE_WAIT_MS {
        return Err("Enter a Pre-Wait between 0 and 86,400 seconds.".into());
    }

    let list = workspace.active_cue_list_mut().ok_or("No active cue list")?;
    let group = list.get_mut_recursive(&edit.group_id)
        .ok_or("This Group moved or was removed. Reopen the Timeline.")?;
    if group.group_mode() != Some(GroupMode::Simultaneous) {
        return Err("The Group is no longer in Simultaneous mode.".into());
    }
    if group.is_running() || group.is_paused() {
        return Err("Stop the Group before editing entrances.".into());
    }
    let child = group.child_cues_mut().and_then(|children| {
        children.iter_mut().find(|child| child.id() == edit.child_id)
    }).ok_or("This cue is no longer a direct child of the Group.")?;
    if !supports_timeline_entrance(child.cue_type()) {
        return Err("This cue type has no timed entrance in a Simultaneous Group.".into());
    }
    if child.is_running() || child.is_paused() {
        return Err("Stop the child cue before editing its entrance.".into());
    }
    let previous = child.pre_wait();
    if previous != Duration::from_millis(edit.expected_pre_wait_ms) {
        return Err("The cue's Pre-Wait changed. Review the refreshed Timeline before editing.".into());
    }
    let new = Duration::from_millis(edit.pre_wait_ms);
    if previous == new {
        return Ok(false);
    }
    if child.loaded_time_ms().is_some() && !new.is_zero() {
        return Err("Clear the child's loaded time before changing its entrance.".into());
    }

    child.set_pre_wait(new);
    history.push_pre_wait_edit(list.id, edit.child_id, previous);
    workspace.mark_modified();
    Ok(true)
}

/// Commit one Timeline gesture or numeric edit against the cue the operator saw.
#[tauri::command]
pub fn set_group_child_entrance(
    edit: GroupChildEntranceEdit,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<bool, String> {
    let mut workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    let mut history = state.undo_stack.lock().map_err(|error| error.to_string())?;
    let changed = edit_group_child_entrance(&mut workspace, &mut history, edit)?;
    drop(history);
    drop(workspace);
    if changed {
        let _ = app_handle.emit("workspace-modified", ());
    }
    Ok(changed)
}

/// Enable/disable looping for a Playlist Group Cue (wrap last child → first).
#[tauri::command]
pub fn set_playlist_loop(
    group_id: String,
    loop_on: bool,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = group_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    edit_cue_metadata(&state, &app_handle, |list| {
        let cue = list.get_mut_recursive(&id).ok_or_else(|| anyhow::anyhow!("Group cue not found"))?;
        let current = cue.playlist_loop().ok_or_else(|| anyhow::anyhow!("Select a Group cue to set playlist looping"))?;
        if current == loop_on {
            return Ok(false);
        }
        cue.set_playlist_loop(loop_on);
        Ok(true)
    })
}

/// Set whether this Playlist leaves standby in the main cue list after GO.
#[tauri::command]
pub fn set_playlist_advance_playhead(
    group_id: Uuid,
    advance: bool,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    edit_cue_metadata(&state, &app_handle, |list| {
        let cue = list.get_mut_recursive(&group_id).ok_or_else(|| anyhow::anyhow!("Playlist Group not found"))?;
        let current = cue.playlist_advance_playhead().ok_or_else(|| anyhow::anyhow!("Select a Playlist Group"))?;
        if current == advance {
            return Ok(false);
        }
        anyhow::ensure!(!cue.is_running() && !cue.is_paused(),
            "Stop the Playlist before changing standby behavior");
        cue.set_playlist_advance_playhead(advance);
        Ok(true)
    })
}

fn owned_advancing_playlist(
    workspace: &mut Workspace,
    workspace_id: Uuid,
    list_id: Uuid,
    group_id: Uuid,
) -> Result<&mut GroupCue, String> {
    if workspace.instance_id() != workspace_id || workspace.active_cue_list_id != list_id {
        return Err("The workspace or cue list changed. Reopen the Playlist inspector.".into());
    }
    let list = workspace.active_cue_list_mut().ok_or("No active cue list")?;
    if list.mode != CueListMode::Sequential {
        return Err("Choose an advancing Playlist in a sequential cue list.".into());
    }
    let group = list.get_mut(&group_id)
        .and_then(|cue| (cue as &mut dyn std::any::Any).downcast_mut::<GroupCue>())
        .ok_or("Choose a top-level Playlist group.")?;
    if group.playlist_advance_playhead() != Some(true) {
        return Err("Enable Advance playhead after GO on the Playlist.".into());
    }
    Ok(group)
}

fn edit_playlist_second_trigger(
    workspace: &mut Workspace,
    history: &mut UndoHistory,
    group_id: Uuid,
    expected: PlaylistTriggerSetting,
    trigger: PlaylistSecondTrigger,
) -> Result<bool, String> {
    let group = owned_advancing_playlist(workspace, expected.workspace_id, expected.list_id, group_id)?;
    let current = group.playlist_second_trigger().ok_or("Choose an advancing Playlist.")?;
    if current != expected.trigger {
        return Err("The Second Trigger setting changed. Review the refreshed inspector.".into());
    }
    if current == trigger {
        return Ok(false);
    }
    if group.is_running() || group.is_paused() {
        return Err("Stop the Playlist before editing its Second Trigger.".into());
    }

    group.set_playlist_second_trigger(trigger);
    history.push_playlist_trigger_edit(expected.list_id, group_id, current);
    workspace.mark_modified();
    Ok(true)
}

#[tauri::command]
pub fn set_playlist_second_trigger(
    group_id: Uuid,
    expected: PlaylistTriggerSetting,
    trigger: PlaylistSecondTrigger,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    let mut history = state.undo_stack.lock().map_err(|error| error.to_string())?;
    let changed = edit_playlist_second_trigger(&mut workspace, &mut history, group_id, expected, trigger)?;
    drop(history);
    drop(workspace);
    if changed {
        let _ = app_handle.emit("workspace-modified", ());
    }
    Ok(())
}

#[derive(Clone, Copy, Deserialize)]
pub struct PlaylistShuffleEdit {
    workspace_id: Uuid,
    list_id: Uuid,
    group_id: Uuid,
    expected: bool,
    enabled: bool,
}

fn edit_playlist_auto_shuffle(
    workspace: &mut Workspace,
    history: &mut UndoHistory,
    edit: PlaylistShuffleEdit,
) -> Result<bool, String> {
    let group = owned_advancing_playlist(workspace, edit.workspace_id, edit.list_id, edit.group_id)?;
    let previous = group.playlist_auto_shuffle().ok_or("Choose an advancing Playlist.")?;
    if previous != edit.expected {
        return Err("Auto-shuffle changed. Review the refreshed Playlist inspector.".into());
    }
    if previous == edit.enabled {
        return Ok(false);
    }
    if group.is_running() || group.is_paused() {
        return Err("Stop the Playlist before changing Auto-shuffle.".into());
    }
    group.set_playlist_auto_shuffle(edit.enabled);
    history.push_playlist_shuffle_edit(edit.list_id, edit.group_id, previous);
    workspace.mark_modified();
    Ok(true)
}

#[tauri::command]
pub fn set_playlist_auto_shuffle(
    edit: PlaylistShuffleEdit,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    let mut history = state.undo_stack.lock().map_err(|error| error.to_string())?;
    let changed = edit_playlist_auto_shuffle(&mut workspace, &mut history, edit)?;
    drop(history);
    drop(workspace);
    if changed {
        let _ = app_handle.emit("workspace-modified", ());
    }
    Ok(())
}

/// Move a top-level cue into a Group's children (position = −1 for append).
#[tauri::command]
pub fn add_cue_to_group(
    cue_id: String,
    group_id: String,
    position: i32,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let cue_uuid: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let grp_uuid: Uuid = group_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let mut workspace = state.workspace.lock().map_err(|e| e.to_string())?;
    let cue_list = workspace.active_cue_list_mut().ok_or("No active cue list")?;
    let before = super::undo_cmds::take_snapshot(cue_list);
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    cue_list
        .add_to_group(&cue_uuid, &grp_uuid, position)
        .map_err(|e| e.to_string())?;
    history.push_action(cue_list.id, before);
    workspace.mark_modified();
    drop(history);
    drop(workspace);
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Move a cue from anywhere in the hierarchy to the top-level list, immediately
/// before `before_id` (or at the end if `before_id` is `null`).
#[tauri::command]
pub fn move_to_top_level(
    cue_id: String,
    before_id: Option<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let cue_uuid: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let before_uuid: Option<Uuid> = before_id
        .as_deref()
        .map(|s| s.parse::<Uuid>().map_err(|e| e.to_string()))
        .transpose()?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    edit_cue_structure(&mut ws, &mut history, |list| {
        list.move_to_top_level_before(&cue_uuid, before_uuid.as_ref())
    })?;
    drop(history);
    drop(ws);
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Move selected direct children after their Group in the main cue list.
#[tauri::command]
pub fn remove_cue_from_group(
    workspace_id: Uuid,
    list_id: Uuid,
    group_id: Uuid,
    cue_ids: Vec<Uuid>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    remove_owned_cues(workspace_id, list_id, &cue_ids, Some(group_id), &state, &app_handle)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::{compute_waveform_bins, set_file_path_resetting_clip};

    #[test]
    fn scoped_audition_uses_the_requested_list_and_open_workspace() {
        use super::{scoped_audition_cue, GroupTimelineOwner};
        use crate::{cue::{audio_cue::AudioCue, traits::Cue}, show::{cue_list::CueList, Workspace}};

        let mut ws = Workspace::new("Rehearsal");
        let active = ws.active_cue_list_id;
        let mut list = CueList::new("Standby checks");
        let cue = AudioCue::new();
        let id = cue.id();
        list.push(Box::new(cue));
        let owner = GroupTimelineOwner { workspace_id: ws.instance_id(), list_id: list.id };
        ws.cue_lists.push(list);
        assert_eq!(scoped_audition_cue(&ws, owner, id).unwrap().id(), id);
        assert_eq!(ws.active_cue_list_id, active);
        assert!(scoped_audition_cue(&ws, GroupTimelineOwner { list_id: active, ..owner }, id).is_err());
        let mut reopened = Workspace::new("Reopened");
        reopened.cue_lists.push(ws.cue_lists.pop().unwrap());
        assert!(scoped_audition_cue(&reopened, owner, id).is_err());
    }

    #[test]
    fn playlist_second_trigger_is_owner_checked_and_has_field_only_history() {
        use super::{edit_playlist_second_trigger, PlaylistTriggerSetting};
        use crate::{
            commands::undo_cmds::apply_history,
            cue::{group_cue::GroupCue, registry::CueRegistry, traits::Cue,
                types::{GroupMode, PlaylistSecondTrigger}, wait_cue::WaitCue},
            show::{undo_stack::{HistoryDirection, UndoHistory}, Workspace},
        };

        let mut ws = Workspace::new("Rehearsal");
        let owner = ws.instance_id();
        let list = ws.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let mut group = GroupCue::new();
        group.mode = GroupMode::Playlist;
        group.children.push(Box::new(WaitCue::new()));
        let group_id = group.id();
        list.push(Box::new(group));
        let outside = WaitCue::new();
        let outside_id = outside.id();
        list.push(Box::new(outside));
        let expected = PlaylistTriggerSetting { workspace_id: owner, list_id, trigger: PlaylistSecondTrigger::Ignore };
        let mut history = UndoHistory::new();

        let stale = PlaylistTriggerSetting { workspace_id: uuid::Uuid::new_v4(), ..expected };
        assert!(edit_playlist_second_trigger(&mut ws, &mut history, group_id, stale, PlaylistSecondTrigger::Previous).is_err());
        assert!(!edit_playlist_second_trigger(&mut ws, &mut history, group_id, expected, PlaylistSecondTrigger::Ignore).unwrap());
        assert!(!history.can_undo(list_id));
        assert!(edit_playlist_second_trigger(&mut ws, &mut history, group_id, expected, PlaylistSecondTrigger::Previous).unwrap());
        let list = ws.active_cue_list_mut().unwrap();
        list.set_playhead(Some(outside_id)).unwrap();
        let registry = CueRegistry::new();

        for (direction, trigger) in [
            (HistoryDirection::Undo, "ignore"),
            (HistoryDirection::Redo, "previous"),
        ] {
            assert!(apply_history(&mut ws, &mut history, &registry, direction).unwrap());
            let list = ws.active_cue_list().unwrap();
            assert_eq!(list.playhead_cue_id, Some(outside_id));
            assert_eq!(list.get(&group_id).unwrap().serialize()["playlist_second_trigger"], trigger);
        }
    }

    #[test]
    fn playlist_shuffle_checks_ownership_and_undo_preserves_standby() {
        use super::{edit_playlist_auto_shuffle, PlaylistShuffleEdit};
        use crate::{
            commands::undo_cmds::apply_history,
            cue::{group_cue::GroupCue, registry::CueRegistry, traits::Cue,
                types::GroupMode, wait_cue::WaitCue},
            show::{undo_stack::{HistoryDirection, UndoHistory}, Workspace},
        };

        let mut workspace = Workspace::new("Shuffle rehearsal");
        let workspace_id = workspace.instance_id();
        let list = workspace.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let mut group = GroupCue::new();
        group.mode = GroupMode::Playlist;
        group.children.push(Box::new(WaitCue::new()));
        let group_id = group.id();
        list.push(Box::new(group));
        let outside = WaitCue::new();
        let outside_id = outside.id();
        list.push(Box::new(outside));
        let edit = PlaylistShuffleEdit { workspace_id, list_id, group_id, expected: false, enabled: true };
        let mut history = UndoHistory::new();
        let stale = PlaylistShuffleEdit { workspace_id: uuid::Uuid::new_v4(), ..edit };

        assert!(edit_playlist_auto_shuffle(&mut workspace, &mut history, stale).is_err());
        assert!(!history.can_undo(list_id));
        assert!(edit_playlist_auto_shuffle(&mut workspace, &mut history, edit).unwrap());
        assert!(edit_playlist_auto_shuffle(&mut workspace, &mut history, edit).is_err());
        workspace.active_cue_list_mut().unwrap().set_playhead(Some(outside_id)).unwrap();

        let registry = CueRegistry::new();
        for (direction, enabled) in [
            (HistoryDirection::Undo, false),
            (HistoryDirection::Redo, true),
        ] {
            assert!(apply_history(&mut workspace, &mut history, &registry, direction).unwrap());
            let list = workspace.active_cue_list().unwrap();
            assert_eq!(list.playhead_cue_id, Some(outside_id));
            assert_eq!(list.get(&group_id).unwrap().serialize()["playlist_auto_shuffle"], enabled);
        }
    }

    #[test]
    fn timeline_entrance_checks_ownership_and_records_one_exact_undo() {
        use super::{edit_group_child_entrance, GroupChildEntranceEdit};
        use crate::{
            commands::undo_cmds::apply_history,
            cue::{
                audio_cue::{AudioCue, AudioCueFactory},
                fade_cue::{FadeCue, FadeCueFactory},
                group_cue::{GroupCue, GroupCueFactory},
                registry::CueRegistry,
                traits::Cue,
                types::{ContinueMode, CueType, GroupMode},
                wait_cue::{WaitCue, WaitCueFactory},
            },
            show::{
                undo_stack::{HistoryDirection, UndoHistory},
                wall_clock::{Observation, WallClockTrigger},
                Workspace,
            },
        };
        use std::{sync::Arc, time::Duration};
        use uuid::Uuid;

        let mut workspace = Workspace::new("Timeline rehearsal");
        let mut group = GroupCue::new();
        let first = WaitCue::new();
        let first_id = first.id();
        let mut second = WaitCue::new();
        let second_id = second.id();
        second.set_disabled(true);
        let mut outside = WaitCue::new();
        outside.set_continue_mode(ContinueMode::DoNotContinue);
        let outside_id = outside.id();
        let group_id = group.id();
        let fade = FadeCue::new();
        let fade_id = fade.id();
        let mut audio = AudioCue::new();
        let audio_id = audio.id();
        audio.accept_preloaded_audio(Arc::new(vec![0.; 48_000]), 1, 48_000, Duration::from_secs(1));
        audio.load_to_time(250).unwrap();
        group.children = vec![Box::new(first), Box::new(second), Box::new(fade), Box::new(audio)];
        let workspace_id = workspace.instance_id();
        let list = workspace.active_cue_list_mut().unwrap();
        let list_id = list.id;
        list.push(Box::new(group));
        list.push(Box::new(outside));
        let mut history = UndoHistory::new();
        let change = GroupChildEntranceEdit {
            workspace_id, list_id, group_id, child_id: second_id,
            expected_pre_wait_ms: 0, pre_wait_ms: 1250,
        };
        let revision = workspace.revision;
        for invalid in [
            GroupChildEntranceEdit { workspace_id: Uuid::new_v4(), ..change },
            GroupChildEntranceEdit { list_id: Uuid::new_v4(), ..change },
            GroupChildEntranceEdit { group_id: outside_id, ..change },
            GroupChildEntranceEdit { child_id: outside_id, ..change },
            GroupChildEntranceEdit { child_id: fade_id, ..change },
            GroupChildEntranceEdit { child_id: audio_id, ..change },
            GroupChildEntranceEdit { expected_pre_wait_ms: 1, ..change },
            GroupChildEntranceEdit { pre_wait_ms: 86_400_001, ..change },
        ] {
            assert!(edit_group_child_entrance(&mut workspace, &mut history, invalid).is_err());
        }
        assert!(!edit_group_child_entrance(&mut workspace, &mut history,
            GroupChildEntranceEdit { pre_wait_ms: 0, ..change }).unwrap());
        assert_eq!(workspace.revision, revision);
        assert!(!history.can_undo(list_id));

        assert!(edit_group_child_entrance(&mut workspace, &mut history, change).unwrap());
        let mut registry = CueRegistry::new();
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        registry.register(CueType::Wait, Box::new(WaitCueFactory));
        registry.register(CueType::Fade, Box::new(FadeCueFactory));
        registry.register(CueType::Audio, Box::new(AudioCueFactory));
        let saved = workspace.active_cue_list().unwrap().get(&group_id).unwrap().serialize();
        let reopened = registry.from_json(saved).unwrap();
        assert_eq!(reopened.child_cues().unwrap()[1].pre_wait(), Duration::from_millis(1250));
        assert!(reopened.child_cues().unwrap()[1].is_disabled());

        let (context, _, _) = crate::test_support::recording_context();
        let group = workspace.active_cue_list_mut().unwrap().get_mut(&group_id).unwrap();
        // Use only the two Wait cues for the playback check. Restage Audio after the stop.
        let children = group.child_cues_mut().unwrap();
        children[1].set_disabled(false);
        children[2].set_disabled(true);
        children[3].set_disabled(true);
        group.go(&context).unwrap();
        let list = workspace.active_cue_list().unwrap();
        assert!(list.get_recursive(&first_id).unwrap().is_action_started());
        assert!(!list.get_recursive(&second_id).unwrap().is_action_started());
        let later = GroupChildEntranceEdit { expected_pre_wait_ms: 1250, pre_wait_ms: 2000, ..change };
        assert!(edit_group_child_entrance(&mut workspace, &mut history, later).is_err());
        workspace.active_cue_list_mut().unwrap().get_mut(&group_id).unwrap().pause(&context).unwrap();
        assert!(edit_group_child_entrance(&mut workspace, &mut history, later).is_err());
        assert!(apply_history(&mut workspace, &mut history, &registry, HistoryDirection::Undo).is_err());
        assert!(workspace.active_cue_list().unwrap().get_recursive(&second_id).unwrap().is_paused());
        let group = workspace.active_cue_list_mut().unwrap().get_mut(&group_id).unwrap();
        group.stop(&context).unwrap();
        group.set_group_mode(GroupMode::StartFirst);
        assert!(edit_group_child_entrance(&mut workspace, &mut history, later).is_err());

        let list = workspace.active_cue_list_mut().unwrap();
        list.set_playhead(Some(outside_id)).unwrap();
        list.wall_clock_triggers.insert(outside_id, WallClockTrigger {
            time: "14:00:00".parse().unwrap(),
            weekdays: 0b1111111,
        });
        list.wall_clock_session.set_armed(outside_id, true, Observation::now());
        list.get_mut_recursive(&second_id).unwrap().set_disabled(true);
        let audio = (list.get_mut_recursive(&audio_id).unwrap() as &mut dyn std::any::Any)
            .downcast_mut::<AudioCue>().unwrap();
        audio.set_disabled(false);
        audio.load_to_time(250).unwrap();

        // No factories are needed for a Timeline undo or redo.
        let registry = CueRegistry::new();
        for (direction, expected) in [(HistoryDirection::Undo, 0), (HistoryDirection::Redo, 1250)] {
            assert!(apply_history(&mut workspace, &mut history, &registry, direction).unwrap());
            let list = workspace.active_cue_list().unwrap();
            assert_eq!(list.playhead_cue_id, Some(outside_id));
            assert!(list.wall_clock_session.is_armed(outside_id));
            assert_eq!(list.get_recursive(&audio_id).unwrap().loaded_time_ms(), Some(250));
            assert_eq!(list.get(&group_id).unwrap().group_mode(), Some(GroupMode::StartFirst));
            let child = list.get_recursive(&second_id).unwrap();
            assert_eq!(child.pre_wait(), Duration::from_millis(expected));
            assert!(child.is_disabled());
        }
        assert!(!history.can_redo(list_id));

        // A later Load to Time blocks restoring a nonzero Pre-Wait to that
        // same Audio cue until the operator clears its prepared offset.
        let list = workspace.active_cue_list_mut().unwrap();
        list.get_mut(&group_id).unwrap().set_group_mode(GroupMode::Simultaneous);
        let audio = list.get_mut_recursive(&audio_id).unwrap();
        assert!(audio.cancel_loaded_time());
        audio.set_pre_wait(Duration::from_millis(500));
        let clear = GroupChildEntranceEdit {
            child_id: audio_id, expected_pre_wait_ms: 500, pre_wait_ms: 0, ..change
        };
        assert!(edit_group_child_entrance(&mut workspace, &mut history, clear).unwrap());
        let audio = (workspace.active_cue_list_mut().unwrap().get_mut_recursive(&audio_id).unwrap()
            as &mut dyn std::any::Any).downcast_mut::<AudioCue>().unwrap();
        audio.load_to_time(250).unwrap();
        assert!(apply_history(&mut workspace, &mut history, &registry, HistoryDirection::Undo).is_err());
        let audio = workspace.active_cue_list_mut().unwrap().get_mut_recursive(&audio_id).unwrap();
        assert_eq!(audio.loaded_time_ms(), Some(250));
        assert_eq!(audio.pre_wait(), Duration::ZERO);
        assert!(audio.cancel_loaded_time());
        assert!(apply_history(&mut workspace, &mut history, &registry, HistoryDirection::Undo).unwrap());
        assert_eq!(
            workspace.active_cue_list().unwrap().get_recursive(&audio_id).unwrap().pre_wait(),
            Duration::from_millis(500),
        );
    }

    #[test]
    fn timeline_summary_identifies_the_open_instance_even_for_same_saved_ids() {
        use super::{list_summaries, GroupTimelineOwner, LoadingCues};
        use crate::{
            cue::{
                group_cue::{GroupCue, GroupCueFactory},
                registry::CueRegistry,
                traits::Cue,
                types::CueType,
                wait_cue::{WaitCue, WaitCueFactory},
            },
            show::Workspace,
        };

        let mut original = Workspace::new("Original");
        let mut group = GroupCue::new();
        let child = WaitCue::new();
        let (group_id, child_id) = (group.id().to_string(), child.id().to_string());
        group.children.push(Box::new(child));
        let saved = group.serialize();
        let list = original.active_cue_list_mut().unwrap();
        let list_id = list.id;
        list.push(Box::new(group));

        let mut registry = CueRegistry::new();
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        registry.register(CueType::Wait, Box::new(WaitCueFactory));
        let mut copy = Workspace::new("Collected copy");
        copy.active_cue_list_id = list_id;
        copy.cue_lists[0].id = list_id;
        copy.active_cue_list_mut().unwrap().push(registry.from_json(saved).unwrap());

        assert_ne!(original.instance_id(), copy.instance_id());
        for workspace in [&original, &copy] {
            let rows = list_summaries(
                workspace, workspace.active_cue_list().unwrap(), &LoadingCues::default(), &mut |_| false,
            );
            assert_eq!(rows[0].id, group_id);
            assert_eq!(rows[0].children.as_ref().unwrap()[0].id, child_id);
            assert_eq!(rows[0].timeline_owner, Some(GroupTimelineOwner {
                workspace_id: workspace.instance_id(),
                list_id,
            }));
            assert!(rows[0].children.as_ref().unwrap()[0].timeline_owner.is_none());
        }
    }

    #[test]
    fn cue_structure_history_undoes_a_move_before_undoing_a_committed_review_flag() {
        use crate::commands::{
            review_cmds::{apply_review, ReviewRequest},
            undo_cmds::apply_history,
        };
        use crate::cue::{memo_cue::{MemoCue, MemoCueFactory}, registry::CueRegistry, traits::Cue, types::CueType};
        use crate::show::{undo_stack::{HistoryDirection, UndoHistory}, Workspace};
        use super::edit_cue_structure;
        use uuid::Uuid;

        let mut workspace = Workspace::new("Concurrent review rehearsal");
        let workspace_id = workspace.instance_id();
        let list = workspace.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let (first, second) = (MemoCue::new(), MemoCue::new());
        let (first_id, second_id) = (first.id(), second.id());
        list.push(Box::new(first));
        list.push(Box::new(second));
        let mut history = UndoHistory::new();
        let change = ReviewRequest::SetFlag {
            workspace_id, list_id, cue_id: second_id, expected_flagged: false, flagged: true,
        };
        assert!(apply_review(&mut workspace, change, Some(&mut history)).unwrap().1);

        let revision = workspace.revision;
        assert!(edit_cue_structure(&mut workspace, &mut history, |list| {
            list.move_before(&[Uuid::new_v4()], Some(first_id))
        }).is_err());
        assert_eq!(workspace.revision, revision);
        edit_cue_structure(&mut workspace, &mut history, |list| {
            list.move_before(&[second_id], Some(first_id))
        }).unwrap();
        assert_eq!(workspace.active_cue_list().unwrap().cues[0].id(), second_id);

        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        assert!(apply_history(&mut workspace, &mut history, &registry, HistoryDirection::Undo).unwrap());
        let list = workspace.active_cue_list().unwrap();
        assert_eq!(list.cues[0].id(), first_id);
        assert!(list.flagged_cues.contains(&second_id));

        assert!(apply_history(&mut workspace, &mut history, &registry, HistoryDirection::Undo).unwrap());
        let list = workspace.active_cue_list().unwrap();
        assert_eq!(list.cues[0].id(), first_id);
        assert!(!list.flagged_cues.contains(&second_id));
        assert!(!history.can_undo(list_id));
    }

    #[test]
    fn repeated_cue_number_metadata_edits_leave_review_history_in_order() {
        use super::edit_cue_list;
        use crate::commands::{review_cmds::edit_flag, undo_cmds::apply_history};
        use crate::cue::{
            group_cue::{GroupCue, GroupCueFactory},
            memo_cue::{MemoCue, MemoCueFactory},
            registry::CueRegistry,
            traits::Cue,
            types::CueType,
        };
        use crate::show::{undo_stack::{HistoryDirection, UndoHistory}, Workspace};

        let mut workspace = Workspace::new("Cue numbering rehearsal");
        let list = workspace.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let child = MemoCue::new();
        let child_id = child.id();
        let mut group = GroupCue::new();
        group.children.push(Box::new(child));
        list.push(Box::new(group));
        list.renumber_all();
        assert_eq!(list.get_recursive(&child_id).unwrap().number(), Some("1.1"));

        let mut history = UndoHistory::new();
        assert!(edit_flag(&mut workspace, list_id, child_id, Some(false), true, Some(&mut history)).unwrap());
        let revision = workspace.revision;
        let renumber = |list: &mut super::CueList| {
            let changed = list.renumber_all();
            Ok((changed, changed))
        };
        let clear = |list: &mut super::CueList| {
            let changed = list.clear_all_numbers();
            Ok((changed, changed))
        };
        assert!(!edit_cue_list(&mut workspace, &mut history, renumber).unwrap());
        assert_eq!(workspace.revision, revision);
        assert!(edit_cue_list(&mut workspace, &mut history, clear).unwrap());
        assert!(!edit_cue_list(&mut workspace, &mut history, clear).unwrap());
        assert_eq!(workspace.revision, revision + 1);

        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        assert!(apply_history(&mut workspace, &mut history, &registry, HistoryDirection::Undo).unwrap());
        let list = workspace.active_cue_list().unwrap();
        assert_eq!(list.get_recursive(&child_id).unwrap().number(), Some("1.1"));
        assert!(list.flagged_cues.contains(&child_id));
        assert!(apply_history(&mut workspace, &mut history, &registry, HistoryDirection::Undo).unwrap());
        assert!(!workspace.active_cue_list().unwrap().flagged_cues.contains(&child_id));
        assert!(!history.can_undo(list_id));
    }

    #[test]
    fn rejected_cue_removal_leaves_live_cues_and_history_untouched() {
        use super::apply_cue_removal;
        use crate::cue::{group_cue::GroupCue, traits::Cue, wait_cue::WaitCue};
        use crate::show::{undo_stack::UndoHistory, Workspace};
        use uuid::Uuid;

        let mut workspace = Workspace::new("Removal practice");
        let workspace_id = workspace.instance_id();
        let revision = workspace.revision;
        let list = workspace.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let live = WaitCue::running_for_test();
        let live_id = live.id();
        list.push(Box::new(live));
        let mut group = GroupCue::new();
        let group_id = group.id();
        let child = WaitCue::running_for_test();
        let child_id = child.id();
        group.children.push(Box::new(child));
        list.push(Box::new(group));

        let mut history = UndoHistory::new();
        let missing = Uuid::new_v4();
        for (owner, parent, selection) in [
            (workspace_id, None, vec![live_id, missing]),
            (workspace_id, None, vec![live_id, live_id]),
            (Uuid::new_v4(), None, vec![live_id]),
            (workspace_id, Some(group_id), vec![child_id, live_id]),
        ] {
            let result = apply_cue_removal(
                &mut workspace, &mut history, owner, list_id, &selection, parent,
                |_| panic!("a rejected plan must never stop a cue"),
            );
            assert!(result.is_err());
            assert_eq!(workspace.revision, revision);
            assert!(!workspace.is_modified);
            assert!(!history.can_undo(list_id));
            let list = workspace.active_cue_list().unwrap();
            assert!(list.get(&live_id).unwrap().is_running());
            assert!(list.get_recursive(&child_id).unwrap().is_running());
            assert!(list.get(&child_id).is_none());
            assert_eq!(list.playhead_cue_id, Some(live_id));
        }
    }

    #[test]
    fn successful_group_extraction_preserves_child_order_and_records_one_undo() {
        use super::apply_cue_removal;
        use crate::commands::undo_cmds::apply_history;
        use crate::cue::{
            group_cue::{GroupCue, GroupCueFactory},
            memo_cue::{MemoCue, MemoCueFactory},
            registry::CueRegistry,
            traits::Cue,
            types::CueType,
        };
        use crate::show::{undo_stack::{HistoryDirection, UndoHistory}, Workspace};

        let mut workspace = Workspace::new("Removal practice");
        let workspace_id = workspace.instance_id();
        let list = workspace.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let mut group = GroupCue::new();
        let group_id = group.id();
        let first = MemoCue::new();
        let first_id = first.id();
        let second = MemoCue::new();
        let second_id = second.id();
        group.children.push(Box::new(first));
        group.children.push(Box::new(second));
        list.push(Box::new(group));

        let mut history = UndoHistory::new();
        let mut handled = Vec::new();
        assert!(apply_cue_removal(
            &mut workspace, &mut history, workspace_id, list_id, &[second_id, first_id],
            Some(group_id), |cue| handled.push(cue.id()),
        ).unwrap());
        let list = workspace.active_cue_list().unwrap();
        assert_eq!(list.cues.iter().map(|cue| cue.id()).collect::<Vec<_>>(), [group_id, first_id, second_id]);
        assert_eq!(handled, [second_id, first_id]);

        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        assert!(apply_history(&mut workspace, &mut history, &registry, HistoryDirection::Undo).unwrap());
        assert!(!history.can_undo(list_id));
        let list = workspace.active_cue_list().unwrap();
        assert_eq!(list.cues.len(), 1);
        assert_eq!(
            list.cues[0].child_cues().unwrap().iter().map(|cue| cue.id()).collect::<Vec<_>>(),
            [first_id, second_id],
        );
    }

    #[test]
    fn file_assignment_completion_rejects_stale_requests_even_at_the_same_path() {
        use super::{finish_file_assignment, FileAssignment};
        use crate::{
            cue::{audio_cue::AudioCue, traits::Cue, types::CueType},
            show::Workspace,
            state::LoadingCues,
        };
        use std::{path::Path, sync::{Arc, Mutex}, time::Duration};
        use uuid::Uuid;

        let mut workspace = Workspace::new("Practice");
        let mut cue = AudioCue::new();
        let cue_id = cue.id();
        let path = Path::new("A.wav");
        cue.file_path = Some(path.into());
        workspace.active_cue_list_mut().unwrap().push(Box::new(cue));
        let first = FileAssignment {
            workspace_id: workspace.instance_id,
            cue_id,
            request_id: Uuid::new_v4(),
        };
        let second = FileAssignment { request_id: Uuid::new_v4(), ..first };
        let newest = FileAssignment { request_id: Uuid::new_v4(), ..first };
        let prior_workspace = FileAssignment { workspace_id: Uuid::new_v4(), ..newest };
        let mut loading = LoadingCues::default();
        let earlier_open = Uuid::new_v4();
        loading.begin(cue_id, earlier_open);
        for assignment in [first, second, newest] {
            loading.begin(cue_id, assignment.request_id);
        }
        assert!(!loading.finish(cue_id, earlier_open));
        assert!(loading.contains(&cue_id));
        let workspace = Mutex::new(workspace);
        let loading = Mutex::new(loading);

        for (assignment, assigned_path) in [
            (first, path),
            (second, Path::new("B.wav")),
            (prior_workspace, path),
        ] {
            assert!(!finish_file_assignment(
                &workspace, &loading, assignment, assigned_path, CueType::Audio,
                |_| panic!("an obsolete result must never be published"),
            ));
            assert!(loading.lock().unwrap().contains(&cue_id));
        }
        assert!(finish_file_assignment(
            &workspace, &loading, newest, path, CueType::Audio,
            |cue| cue.accept_preloaded_audio(Arc::new(vec![0.5]), 1, 44_100, Duration::from_secs(1)),
        ));
        assert!(!loading.lock().unwrap().contains(&cue_id));
        let ws = workspace.lock().unwrap();
        let cue = ws.active_cue_list().unwrap().get_recursive(&cue_id).unwrap();
        assert_eq!(cue.extract_decoded_audio().unwrap().0.as_slice(), &[0.5]);
    }

    #[test]
    fn renumber_selected_skips_occupied_workspace_numbers_and_rejects_a_stale_preview() {
        use crate::cue::{
            group_cue::{GroupCue, GroupCueFactory},
            memo_cue::{MemoCue, MemoCueFactory},
            registry::CueRegistry,
            traits::Cue,
            types::CueType,
        };
        use crate::commands::undo_cmds::apply_history;
        use crate::show::{cue_list::CueList, undo_stack::{HistoryDirection, UndoHistory}, Workspace};
        use super::{apply_renumber_preview, plan_cue_numbering, RenumberSelectionPreview};

        fn numbered(number: &str) -> MemoCue {
            let mut cue = MemoCue::new();
            cue.set_number(Some(number.to_owned()));
            cue
        }

        let mut workspace = Workspace::new("Renumber practice");
        let workspace_id = workspace.instance_id();
        let list = workspace.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let first = numbered("10");
        let first_id = first.id();
        let reserved_here = numbered("11");
        let reserved_here_id = reserved_here.id();
        let child = numbered("26");
        let child_id = child.id();
        let mut group = GroupCue::new();
        group.children.push(Box::new(child));
        let last = numbered("27");
        let last_id = last.id();
        list.push(Box::new(first));
        list.push(Box::new(reserved_here));
        list.push(Box::new(group));
        list.push(Box::new(last));

        let mut other_list = CueList::new("Other cues");
        let reserved_elsewhere = numbered("13");
        let reserved_elsewhere_id = reserved_elsewhere.id();
        let mut other_group = GroupCue::new();
        other_group.children.push(Box::new(reserved_elsewhere));
        other_list.push(Box::new(other_group));
        workspace.cue_lists.push(other_list);

        let ids = [last_id, child_id, first_id];
        let preview = |workspace: &Workspace| plan_cue_numbering(workspace, workspace_id, list_id, &ids, 10.0, 1.0).unwrap();
        let numbers = |plan: &RenumberSelectionPreview| plan.changes.iter()
            .map(|change| change.number.clone()).collect::<Vec<_>>();
        let stale = preview(&workspace);
        assert_eq!(numbers(&stale), ["10", "12", "14"]);
        assert!(plan_cue_numbering(&workspace, workspace_id, list_id, &ids, 10.0, 1e-10).is_err());
        workspace.cue_lists[1].get_mut_recursive(&reserved_elsewhere_id).unwrap().set_number(Some("12".into()));
        let mut history = UndoHistory::new();
        assert!(apply_renumber_preview(&mut workspace, &mut history, stale).is_err());
        assert!(!history.can_undo(list_id));
        assert!(!workspace.is_modified);
        assert_eq!(workspace.active_cue_list().unwrap().get_recursive(&child_id).unwrap().number(), Some("26"));

        let ready = preview(&workspace);
        assert_eq!(numbers(&ready), ["10", "13", "14"]);
        assert!(apply_renumber_preview(&mut workspace, &mut history, ready).unwrap());
        assert!(workspace.is_modified);
        let list = workspace.active_cue_list().unwrap();
        assert_eq!(list.unique_numbered_cue("11").unwrap().id(), reserved_here_id);
        assert_eq!(list.get_recursive(&child_id).unwrap().number(), Some("13"));
        assert_eq!(list.get_recursive(&last_id).unwrap().number(), Some("14"));
        assert_eq!(workspace.cue_lists[1].get_recursive(&reserved_elsewhere_id).unwrap().number(), Some("12"));
        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        assert!(apply_history(&mut workspace, &mut history, &registry, HistoryDirection::Undo).unwrap());
        let restored = workspace.active_cue_list().unwrap();
        assert_eq!(restored.get_recursive(&last_id).unwrap().number(), Some("27"));
        assert!(!history.can_undo(list_id));
        assert!(apply_history(&mut workspace, &mut history, &registry, HistoryDirection::Redo).unwrap());
        assert_eq!(workspace.active_cue_list().unwrap().get_recursive(&last_id).unwrap().number(), Some("14"));
    }

    #[test]
    fn changing_the_file_resets_the_clip_window() {
        let mut json = serde_json::json!({
            "file_path": "old.wav",
            "start_time_ms": 81_314,
            "end_time_ms": 147_104,
            "slices": { "markers": [90_000, 120_000], "play_counts": [1, u32::MAX, 1] },
            "cached_duration_ms": 274_250,
            "volume_db": -6.0,
        });
        set_file_path_resetting_clip(&mut json, "new.wav");
        assert_eq!(json["file_path"], "new.wav");
        assert!(json["start_time_ms"].is_null());
        assert!(json["end_time_ms"].is_null());
        assert!(json["cached_duration_ms"].is_null());
        assert_eq!(json["slices"]["markers"].as_array().unwrap().len(), 0);
        assert_eq!(json["volume_db"], -6.0, "unrelated fields untouched");
    }

    #[test]
    fn repicking_the_same_file_keeps_the_clip_window() {
        let mut json = serde_json::json!({
            "file_path": "same.wav",
            "start_time_ms": 1_000,
            "end_time_ms": 2_000,
            "slices": { "markers": [1_500], "play_counts": [1, 1] },
        });
        set_file_path_resetting_clip(&mut json, "same.wav");
        assert_eq!(json["start_time_ms"], 1_000);
        assert_eq!(json["end_time_ms"], 2_000);
        assert_eq!(json["slices"]["markers"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn waveform_bins_empty_input() {
        assert_eq!(compute_waveform_bins(&[], 2, 10), (vec![], vec![]));
        assert_eq!(compute_waveform_bins(&[0.5, 0.5], 2, 0), (vec![], vec![]));
    }

    #[test]
    fn waveform_bins_rms_never_exceeds_peak() {
        let samples: Vec<f32> = (0..4800)
            .map(|i| (i as f32 * 0.13).sin() * 0.8)
            .collect();
        let (peaks, rms) = compute_waveform_bins(&samples, 2, 40);
        assert_eq!(peaks.len(), 40);
        assert_eq!(rms.len(), 40);
        for (p, r) in peaks.iter().zip(&rms) {
            assert!(r <= p, "rms {r} > peak {p}");
        }
    }

    #[test]
    fn waveform_bins_sine_rms_is_peak_over_sqrt2() {
        // Full-scale sine: peak ≈ 1.0, RMS ≈ 1/√2 ≈ 0.707.
        let samples: Vec<f32> = (0..48000)
            .map(|i| (i as f32 * std::f32::consts::TAU / 480.0).sin())
            .collect();
        let (peaks, rms) = compute_waveform_bins(&samples, 1, 4);
        for p in &peaks {
            assert!((p - 1.0).abs() < 0.01, "peak {p}");
        }
        for r in &rms {
            assert!((r - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.01, "rms {r}");
        }
    }

    #[test]
    fn waveform_bins_constant_signal_rms_equals_peak() {
        let samples = vec![0.5f32; 1000];
        let (peaks, rms) = compute_waveform_bins(&samples, 1, 5);
        for (p, r) in peaks.iter().zip(&rms) {
            assert!((p - 0.5).abs() < 1e-6);
            assert!((r - 0.5).abs() < 1e-4);
        }
    }
}


/// Bake only an explicit point edit. Other inspector edits leave point routing intact.
pub(crate) fn apply_point_routing(
    cue: &mut serde_json::Value,
    changes: &serde_json::Value,
    default_patch: Option<Uuid>,
    patches: &[(Uuid, Vec<u16>)],
) -> Result<(), String> {
    if cue["type"] != "audio" {
        return Ok(());
    }
    let Some(layout) = changes.get("point_mix").filter(|v| !v.is_null()) else {
        if changes.get("level_matrix").is_some() { cue["point_mix"] = serde_json::Value::Null; }
        return Ok(());
    };
    let point: crate::cue::point_mix::PointMix =
        serde_json::from_value(layout.clone()).map_err(|e| e.to_string())?;
    let patch = point_patch(cue, default_patch);
    let width = match patch {
        Some(id) => patches.iter().find(|(pid, _)| *pid == id)
            .map(|(_, channels)| channels.len().min(crate::engine::voice::MATRIX_OUTPUTS))
            .ok_or("Assign an available output patch first.")?,
        None => 2,
    };
    let mut matrix: Option<Vec<Vec<f64>>> =
        serde_json::from_value(cue["level_matrix"].clone()).map_err(|e| e.to_string())?;
    let from_pan = matrix.is_none();
    let rows = matrix.get_or_insert_with(Vec::new);
    point.apply(rows, width)?;
    cue["level_matrix"] = serde_json::json!(rows);
    if from_pan {
        // No legacy matrix routes exist. Use discrete file channel numbers.
        cue["matrix_mirror_mono"] = serde_json::json!(false);
    }
    Ok(())
}

fn point_patch(cue: &serde_json::Value, default_patch: Option<Uuid>) -> Option<Uuid> {
    cue["output_patch_id"].as_str().and_then(|id| id.parse().ok()).or(default_patch)
}

/// Calculate the draft operator readout without changing the cue or live voice.
#[tauri::command]
pub fn preview_point_mix(layout: crate::cue::point_mix::PointMix, outputs: usize) -> Result<Vec<f64>, String> {
    layout.levels(outputs)
}
