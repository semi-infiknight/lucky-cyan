//! Tauri commands for undo, redo, copy and paste.
//!
//! Structural edits save the full cue list state (serialised JSON and decoded
//! audio Arcs). Flag, note and Timeline entrance edits record one field. Undo and redo apply the entry
//! while holding the registry, workspace and undo_stack locks in that order.
//!
//! Lock ordering (always respected to prevent deadlocks):
//!   registry → workspace → undo_stack → clipboard

use std::collections::{BTreeMap, BTreeSet, HashMap};

use tauri::{Emitter, State};
use uuid::Uuid;

use super::preflight_cmds::spawn_media_preload;

use crate::{
    cue::{
        decoded_audio::{
            apply_decoded_audio, apply_decoded_audio_to_copy, assign_fresh_ids,
            collect_decoded_audio, media_cues_needing_decode,
        },
        group_cue::GroupCue,
        registry::CueRegistry,
        types::CueColor,
    },
    show::{
        cue_list::CueList,
        undo_stack::{CueSnapshot, HistoryDirection, HistoryEntry, Snapshot, UndoHistory},
        Workspace,
    },
    state::{AppState, ClipboardCue},
};

// ---------------------------------------------------------------------------
// Snapshot helpers
// ---------------------------------------------------------------------------

/// Serialise the current cue list into a [`Snapshot`].
///
/// The decoded audio `Arc` for each cue is cloned — this is a reference-count
/// bump, not a data copy, so it is O(n_cues) not O(total_samples).
pub fn take_snapshot(cue_list: &CueList) -> Snapshot {
    Snapshot {
        cart_layout: cue_list.cart_layout.clone(),
        hotkeys: cue_list.hotkeys.clone(),
        flagged_cues: cue_list.flagged_cues.clone(),
        second_colors: cue_list.second_colors.clone(),
        wall_clock_triggers: cue_list.wall_clock_triggers.clone(),
        cues: cue_list
            .cues
            .iter()
            .map(|c| CueSnapshot {
                json: c.serialize(),
                decoded: collect_decoded_audio(c.as_ref()),
            })
            .collect(),
        playhead_id: cue_list.playhead_cue_id,
        playhead_child_id: cue_list.enter_standby_child_id(),
    }
}

/// Flags follow the fresh identities of duplicated cues and their descendants.
pub(super) fn remap_copied_flags(source: &BTreeSet<Uuid>, new_to_old: &HashMap<Uuid, Uuid>) -> Vec<Uuid> {
    new_to_old.iter().filter_map(|(&new, old)| source.contains(old).then_some(new)).collect()
}

pub(super) fn remap_copied_colors(source: &BTreeMap<Uuid, CueColor>, new_to_old: &HashMap<Uuid, Uuid>) -> Vec<(Uuid, CueColor)> {
    new_to_old.iter().filter_map(|(&new, old)| source.get(old).map(|&color| (new, color))).collect()
}

/// Restore a [`Snapshot`] into `cue_list`, rebuilding every cue via the
/// registry and re-injecting decoded audio so there is no re-decode round-trip.
fn restore_snapshot(
    snapshot: &Snapshot,
    cue_list: &mut CueList,
    registry: &CueRegistry,
) -> anyhow::Result<()> {
    let mut restored = Vec::with_capacity(snapshot.cues.len());
    for cs in &snapshot.cues {
        let mut cue = registry.from_json(cs.json.clone())?;
        apply_decoded_audio(cue.as_mut(), &cs.decoded);
        restored.push(cue);
    }
    cue_list.cues = restored;
    cue_list.cart_layout = snapshot.cart_layout.clone();
    cue_list.hotkeys = snapshot.hotkeys.clone();
    cue_list.flagged_cues = snapshot.flagged_cues.clone();
    cue_list.second_colors = snapshot.second_colors.clone();
    cue_list.wall_clock_triggers = snapshot.wall_clock_triggers.clone();
    cue_list.wall_clock_session.disarm_all();
    cue_list.independent_runs.clear();
    cue_list.cancel_enter_follow();
    // Restore playhead; clear it if the referenced cue no longer exists.
    cue_list.playhead_cue_id = snapshot
        .playhead_id
        .filter(|id| !cue_list.independent_pads() && cue_list.cues.iter().any(|c| c.id() == *id));
    cue_list.restore_enter_standby_child(snapshot.playhead_child_id);
    Ok(())
}

// ---------------------------------------------------------------------------
// Undo / Redo
// ---------------------------------------------------------------------------

/// A native snapshot can restore cue programming but cannot restore the
/// engine handles that stop a running voice or resume a loaded presentation.
pub(super) fn playback_is_active(ws: &Workspace) -> bool {
    fn active(cue: &dyn crate::cue::traits::Cue) -> bool {
        cue.is_running() || cue.is_paused() || cue.playing_voice_id().is_some()
            || cue.child_cues().is_some_and(|children| children.iter().any(|child| active(child.as_ref())))
    }
    ws.cue_lists.iter().any(|list| list.cues.iter().any(|cue| active(cue.as_ref())))
}

fn history_available(state: &AppState, direction: HistoryDirection) -> Result<bool, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let list = ws.active_cue_list().ok_or("No active cue list")?;
    let stack = state.undo_stack.lock().map_err(|e| e.to_string())?;
    Ok(!playback_is_active(&ws) && stack.candidate(list.id, direction).is_some())
}

#[tauri::command]
pub fn can_undo(state: State<'_, AppState>) -> Result<bool, String> {
    history_available(&state, HistoryDirection::Undo)
}

#[tauri::command]
pub fn can_redo(state: State<'_, AppState>) -> Result<bool, String> {
    history_available(&state, HistoryDirection::Redo)
}

/// Caller holds the workspace and history locks through the check and restore.
pub fn apply_history(
    ws: &mut Workspace,
    stack: &mut UndoHistory,
    registry: &CueRegistry,
    direction: HistoryDirection,
) -> Result<bool, String> {
    if playback_is_active(ws) {
        return Err("Finish or stop all running and paused cues before using Undo or Redo.".into());
    }
    let list = ws.active_cue_list_mut().ok_or("No active cue list")?;
    let Some(candidate) = stack.candidate(list.id, direction) else { return Ok(false); };
    let inverse = match candidate {
        HistoryEntry::Snapshot(snapshot) => {
            let current = take_snapshot(list);
            restore_snapshot(snapshot, list, registry).map_err(|e| e.to_string())?;
            HistoryEntry::Snapshot(current)
        }
        HistoryEntry::CueFlag { cue_id, flagged } => {
            if *flagged && list.get_recursive(cue_id).is_some() {
                list.flagged_cues.insert(*cue_id);
            } else {
                list.flagged_cues.remove(cue_id);
            }
            HistoryEntry::CueFlag {
                cue_id: *cue_id,
                flagged: !*flagged,
            }
        }
        HistoryEntry::CueNote { cue_id, note } => {
            let previous = if let Some(cue) = list.get_mut_recursive(cue_id) {
                let previous = cue.notes().to_owned();
                cue.set_notes(note.clone());
                previous
            } else {
                note.clone()
            };
            HistoryEntry::CueNote { cue_id: *cue_id, note: previous }
        }
        HistoryEntry::CuePreWait { cue_id, pre_wait } => {
            let cue = list.get_mut_recursive(cue_id).ok_or("The Timeline cue no longer exists.")?;
            if cue.loaded_time_ms().is_some() && !pre_wait.is_zero() {
                return Err("Clear the child's loaded time before undoing or redoing its entrance.".into());
            }
            let previous = cue.pre_wait();
            cue.set_pre_wait(*pre_wait);
            HistoryEntry::CuePreWait { cue_id: *cue_id, pre_wait: previous }
        }
        HistoryEntry::CueHotkey { cue_id, key } => {
            let previous = list.replace_hotkey(*cue_id, key.clone()).map_err(|error| error.to_string())?;
            HistoryEntry::CueHotkey { cue_id: *cue_id, key: previous }
        }
        HistoryEntry::PlaylistTrigger { group_id, trigger } => {
            let group = list.get_mut(group_id)
                .and_then(|cue| (cue as &mut dyn std::any::Any).downcast_mut::<GroupCue>())
                .ok_or("The Playlist group no longer exists.")?;
            let previous = group.playlist_second_trigger().ok_or("The group no longer advances as a Playlist.")?;
            group.set_playlist_second_trigger(*trigger);
            HistoryEntry::PlaylistTrigger { group_id: *group_id, trigger: previous }
        }
        HistoryEntry::PlaylistShuffle { group_id, enabled } => {
            let group = list.get_mut(group_id)
                .and_then(|cue| (cue as &mut dyn std::any::Any).downcast_mut::<GroupCue>())
                .ok_or("The Playlist no longer exists.")?;
            let previous = group.playlist_auto_shuffle().ok_or("The group no longer advances as a Playlist.")?;
            group.set_playlist_auto_shuffle(*enabled);
            HistoryEntry::PlaylistShuffle { group_id: *group_id, enabled: previous }
        }
    };
    stack.advance(list.id, direction, inverse);
    ws.mark_modified();
    Ok(true)
}

fn change_history(
    state: &AppState,
    handle: &tauri::AppHandle,
    direction: HistoryDirection,
) -> Result<(), String> {
    let playhead = {
        let registry = state.registry.lock().map_err(|e| e.to_string())?;
        let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
        let mut stack = state.undo_stack.lock().map_err(|e| e.to_string())?;
        let restores_playhead = matches!(
            stack.candidate(ws.active_cue_list_id, direction),
            Some(HistoryEntry::Snapshot(_))
        );
        if !apply_history(&mut ws, &mut stack, &registry, direction)? {
            return Ok(());
        }
        restores_playhead.then(|| ws.active_cue_list().and_then(|cl| cl.playhead_cue_id))
    };
    let _ = handle.emit("workspace-modified", serde_json::json!({}));
    if let Some(playhead) = playhead {
        let _ = handle.emit("playhead-moved", serde_json::json!({ "cue_id": playhead }));
    }
    Ok(())
}

#[tauri::command]
pub fn undo(state: State<'_, AppState>, app_handle: tauri::AppHandle) -> Result<(), String> {
    change_history(&state, &app_handle, HistoryDirection::Undo)
}

#[tauri::command]
pub fn redo(state: State<'_, AppState>, app_handle: tauri::AppHandle) -> Result<(), String> {
    change_history(&state, &app_handle, HistoryDirection::Redo)
}

// ---------------------------------------------------------------------------
// Copy / Paste
// ---------------------------------------------------------------------------

/// Copy a cue into the in-app clipboard by serialising it to JSON.
///
/// The clipboard is internal to Inkue — it does not interact with the OS
/// clipboard.  Only one cue is stored at a time; copying a new one replaces
/// the previous entry.  The cue's decoded audio (and its children's, for a
/// Group) travels with it, so pasting into another cue list stays playable.
#[tauri::command]
pub fn copy_cue(
    cue_id: String,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let id: Uuid = cue_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let cue_list = ws.active_cue_list().ok_or("No active cue list")?;
    let cue = cue_list.get_recursive(&id).ok_or("Cue not found")?;
    let json = cue.serialize();
    let matrix_origin = if json["level_matrix"].is_array() {
        super::cue_properties_cmds::matrix_paste_patch(&ws, cue)
    } else {
        None
    };
    let entry = ClipboardCue {
        revision: Uuid::new_v4(),
        matrix_origin,
        json,
        flagged_cues: cue_list.flagged_cues.clone(),
        second_colors: cue_list.second_colors.clone(),
        audio: collect_decoded_audio(cue),
    };
    drop(ws);
    *state.clipboard.lock().map_err(|e| e.to_string())? = Some(entry);
    Ok(())
}

/// Paste the clipboard cue as a new cue inserted after `after_cue_id`.
///
/// - If `after_cue_id` is `Some`, the new cue is inserted immediately after
///   the specified cue.
/// - If `after_cue_id` is `None`, the new cue is appended at the end.
///
/// The pasted cue gets a fresh UUID so it is independent of the original.
/// Returns the new cue's ID string.
#[tauri::command]
pub fn paste_cue(
    after_cue_id: Option<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<String, String> {
    let after_id = after_cue_id
        .as_deref()
        .map(str::parse::<Uuid>)
        .transpose()
        .map_err(|e| e.to_string())?;

    // 1. Take a copy of the clipboard entry (brief clipboard lock).
    let (mut new_json, source_audio, source_flags, source_colors) = {
        let clip = state.clipboard.lock().map_err(|e| e.to_string())?;
        let entry = clip.as_ref().ok_or("Clipboard is empty — copy a cue first")?;
        (entry.json.clone(), entry.audio.clone(), entry.flagged_cues.clone(), entry.second_colors.clone())
    };

    // 2. Fresh ids for the cue and its children (a Group's children would
    //    otherwise share ids with the original), then rebuild via the registry
    //    and re-attach the decoded audio of the whole subtree.
    let new_to_old = assign_fresh_ids(&mut new_json);
    let mut new_cue = {
        let registry = state.registry.lock().map_err(|e| e.to_string())?;
        registry.from_json(new_json).map_err(|e| e.to_string())?
    };
    apply_decoded_audio_to_copy(new_cue.as_mut(), &source_audio, &new_to_old);
    let new_id = new_cue.id().to_string();
    let pending_decodes = media_cues_needing_decode(new_cue.as_ref());

    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let cue_list = ws.active_cue_list_mut().ok_or("No active cue list")?;
    let insert_idx = after_id
        .and_then(|id| cue_list.index_of(&id))
        .map(|i| i + 1)
        .unwrap_or(cue_list.cues.len());
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    let before = take_snapshot(cue_list);
    cue_list.ensure_unique_number(new_cue.as_mut());
    cue_list.insert(insert_idx, new_cue);
    cue_list.flagged_cues.extend(remap_copied_flags(&source_flags, &new_to_old));
    cue_list.second_colors.extend(remap_copied_colors(&source_colors, &new_to_old));
    history.push_action(cue_list.id, before);
    ws.mark_modified();
    let instance = ws.instance_id;
    drop(history);
    drop(ws);
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));

    // Anything whose audio could not be carried over (the source was never
    //    decoded, e.g. a file that was missing at copy time) decodes in the
    //    background so the pasted cue still becomes playable.
    for (cue_id, cue_type, path) in pending_decodes {
        spawn_media_preload(state.inner(), &app_handle, instance, cue_id, cue_type, path);
    }

    Ok(new_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::control_cue::{ControlAction, ControlCue};
    use crate::cue::{group_cue::GroupCueFactory, memo_cue::{MemoCue, MemoCueFactory}, traits::Cue, types::CueType};
    use crate::show::wall_clock::{Observation, WallClockTrigger};

    #[test]
    fn cue_flag_history_preserves_standby_scheduled_arming_and_prepared_target() {
        let mut ws = Workspace::new("Practice");
        let list = ws.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let flagged_cue = MemoCue::new();
        let flagged_id = flagged_cue.id();
        list.push(Box::new(flagged_cue));
        let start = ControlCue::new(ControlAction::Start);
        let start_id = start.id();
        list.push(Box::new(start));
        list.set_playhead(Some(flagged_id)).unwrap();
        assert!(list.can_start_independently(flagged_id));
        list.wall_clock_triggers.insert(
            flagged_id,
            WallClockTrigger {
                time: "14:00:00".parse().unwrap(),
                weekdays: 0b1111111,
            },
        );

        let mut history = UndoHistory::new();
        assert!(super::super::review_cmds::edit_flag(
            &mut ws,
            list_id,
            flagged_id,
            Some(false),
            true,
            Some(&mut history),
        ).unwrap());
        let list = ws.active_cue_list_mut().unwrap();
        list.set_playhead(Some(start_id)).unwrap();
        list.wall_clock_session.set_armed(flagged_id, true, Observation::now());
        list.get_mut_recursive(&start_id).unwrap().set_temporary_target(Some(flagged_id));

        // Flag history needs no cue factories and leaves cue instances in place.
        let registry = CueRegistry::new();
        for (direction, flagged) in [(HistoryDirection::Undo, false), (HistoryDirection::Redo, true)] {
            assert!(apply_history(&mut ws, &mut history, &registry, direction).unwrap());
            let list = ws.active_cue_list().unwrap();
            assert_eq!(list.flagged_cues.contains(&flagged_id), flagged);
            assert_eq!(list.playhead_cue_id, Some(start_id));
            assert!(list.wall_clock_session.is_armed(flagged_id));
            assert_eq!(list.get_recursive(&start_id).unwrap().temporary_target(), Some(flagged_id));
        }
    }

    #[test]
    fn snapshots_and_copy_keep_nested_review_flags_by_identity() {
        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        let mut list = CueList::new("Review");
        let cue = MemoCue::new();
        let child = cue.id();
        list.push(Box::new(cue));
        let group = list.group_cues(&[child]).unwrap();
        list.flagged_cues.insert(child);

        let mut copied = list.get_recursive(&group).unwrap().serialize();
        let new_to_old = assign_fresh_ids(&mut copied);
        let remapped = remap_copied_flags(&list.flagged_cues, &new_to_old);
        assert_eq!(remapped.len(), 1);
        assert_eq!(new_to_old[&remapped[0]], child);
        assert_ne!(remapped[0], child);

        let before = take_snapshot(&list);
        list.remove(&group).unwrap();
        let after = take_snapshot(&list);
        assert!(list.flagged_cues.is_empty());
        restore_snapshot(&before, &mut list, &registry).unwrap();
        assert_eq!(list.flagged_cues, BTreeSet::from([child]));
        assert!(list.get_recursive(&child).is_some());
        restore_snapshot(&after, &mut list, &registry).unwrap();
        assert!(list.flagged_cues.is_empty());
        assert!(list.get_recursive(&child).is_none());
    }

    #[test]
    fn structural_undo_and_redo_restore_the_exact_enter_standby_child() {
        use crate::cue::{group_cue::GroupCue, types::GroupMode};

        let mut registry = CueRegistry::new();
        registry.register(CueType::Memo, Box::new(MemoCueFactory));
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        let mut ws = Workspace::new("Standby rehearsal");
        let list = ws.active_cue_list_mut().unwrap();
        let mut group = GroupCue::new();
        group.mode = GroupMode::StartFirstAndEnter;
        let first = MemoCue::new();
        let second = MemoCue::new();
        let (first_id, second_id, group_id) = (first.id(), second.id(), group.id());
        group.children = vec![Box::new(first), Box::new(second)];
        let outside = MemoCue::new();
        let outside_id = outside.id();
        list.push(Box::new(group));
        list.push(Box::new(outside));
        list.set_playhead(Some(second_id)).unwrap();
        let mut history = UndoHistory::new();
        history.push_action(list.id, take_snapshot(list));
        list.move_before(&[outside_id], Some(group_id)).unwrap();
        list.set_playhead(Some(first_id)).unwrap();

        for (direction, child, first_row) in [
            (HistoryDirection::Undo, second_id, group_id),
            (HistoryDirection::Redo, first_id, outside_id),
        ] {
            assert!(apply_history(&mut ws, &mut history, &registry, direction).unwrap());
            let list = ws.active_cue_list().unwrap();
            assert_eq!(list.cues[0].id(), first_row);
            assert_eq!(list.playhead_cue_id, Some(group_id));
            assert_eq!(list.enter_standby_child_id(), Some(child));
        }
    }
}
