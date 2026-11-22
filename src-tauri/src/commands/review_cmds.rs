//! Cue note, flag and name collaboration. The HTTP listener checks the credential and scope.

use crate::{
    cue::traits::Cue,
    show::{undo_stack::UndoHistory, Workspace},
    state::AppState,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tauri::{Emitter, State};
use uuid::Uuid;

const MAX_NOTE_BYTES: usize = 2_000;
const MAX_NAME_BYTES: usize = 2_000;
const MAX_ROWS: usize = 4_096;

#[derive(Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CueNoteEdit {
    Saved { note: String, changed: bool },
    Conflict { note: String },
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum ReviewChange {
    #[default]
    Note,
    FlagSet,
    FlagCleared,
    Name,
}

fn record_review(ws: &mut Workspace, list_id: Uuid, cue_id: Uuid, change: ReviewChange) {
    ws.programming_history.push(json!({
        "time": chrono::Utc::now(),
        "cue_review": { "list_id": list_id, "cue_id": cue_id, "change": change }
    }));
}

fn edit_note(
    ws: &mut Workspace,
    list_id: Uuid,
    cue_id: Uuid,
    expected_note: &str,
    note: String,
    history: Option<&mut UndoHistory>,
) -> Result<CueNoteEdit, String> {
    let list = ws.cue_list_by_id_mut(list_id).ok_or("Unknown cue list")?;
    let cue = list.get_mut_recursive(&cue_id).ok_or("Unknown cue in this list")?;
    if cue.notes() != expected_note {
        return Ok(CueNoteEdit::Conflict { note: cue.notes().to_owned() });
    }
    if note == expected_note {
        return Ok(CueNoteEdit::Saved { note, changed: false });
    }

    let history = history.ok_or("The desk undo history is unavailable")?;
    let before = cue.notes().to_owned();
    cue.set_notes(note.clone());
    history.push_note_edit(list_id, cue_id, before);
    ws.mark_modified();
    Ok(CueNoteEdit::Saved { note, changed: true })
}

/// Apply a list-scoped flag edit. Review clients must compare the current
/// value before changing it, while the local Inspector supplies an intent.
pub(super) fn edit_flag(
    ws: &mut Workspace,
    list_id: Uuid,
    cue_id: Uuid,
    expected: Option<bool>,
    flagged: bool,
    history: Option<&mut UndoHistory>,
) -> Result<bool, String> {
    let list = ws.cue_list_by_id_mut(list_id).ok_or("Unknown cue list")?;
    list.get_recursive(&cue_id).ok_or("Unknown cue in this list")?;
    let previous = list.flagged_cues.contains(&cue_id);
    if expected.is_some_and(|expected| previous != expected) {
        return Err("The cue flag changed. Read it again before editing.".into());
    }
    if previous == flagged {
        return Ok(false);
    }

    let history = history.ok_or("The desk undo history is unavailable")?;
    history.push_flag_edit(list_id, cue_id, previous);
    if flagged {
        list.flagged_cues.insert(cue_id);
    } else {
        list.flagged_cues.remove(&cue_id);
    }
    ws.mark_modified();
    Ok(true)
}

/// Save the inspector's note against the version and show the operator edited.
#[tauri::command]
pub fn set_cue_notes(
    workspace_id: Uuid,
    list_id: Uuid,
    cue_id: Uuid,
    expected_note: String,
    note: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<CueNoteEdit, String> {
    let mut ws = state.workspace.try_lock().map_err(|_| "Desk busy, retry the save")?;
    if ws.instance_id() != workspace_id {
        return Err("The workspace changed. Open the cue again.".into());
    }
    let mut history = state.undo_stack.try_lock().map_err(|_| "Desk busy, retry the save")?;
    let result = edit_note(&mut ws, list_id, cue_id, &expected_note, note, Some(&mut history))?;
    drop(history);
    drop(ws);
    if matches!(&result, CueNoteEdit::Saved { changed: true, .. }) {
        let _ = app_handle.emit("workspace-modified", ());
    }
    Ok(result)
}

#[derive(Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub(super) enum ReviewRequest {
    Lists {},
    Cues {
        workspace_id: Uuid,
        list_id: Uuid,
    },
    Note {
        workspace_id: Uuid,
        list_id: Uuid,
        cue_id: Uuid,
    },
    SetNote {
        workspace_id: Uuid,
        list_id: Uuid,
        cue_id: Uuid,
        expected_note: String,
        note: String,
    },
    Flag {
        workspace_id: Uuid,
        list_id: Uuid,
        cue_id: Uuid,
    },
    SetFlag {
        workspace_id: Uuid,
        list_id: Uuid,
        cue_id: Uuid,
        expected_flagged: bool,
        flagged: bool,
    },
    SetName {
        workspace_id: Uuid,
        list_id: Uuid,
        cue_id: Uuid,
        expected_name: String,
        name: String,
    },
}

impl ReviewRequest {
    fn owner(&self) -> Option<Uuid> {
        match self {
            Self::Lists {} => None,
            Self::Cues { workspace_id, .. }
            | Self::Note { workspace_id, .. }
            | Self::SetNote { workspace_id, .. }
            | Self::Flag { workspace_id, .. }
            | Self::SetFlag { workspace_id, .. }
            | Self::SetName { workspace_id, .. } => Some(*workspace_id),
        }
    }

    pub(super) fn is_write(&self) -> bool {
        matches!(self, Self::SetNote { .. } | Self::SetFlag { .. } | Self::SetName { .. })
    }

    pub(super) fn requires_rename_access(&self) -> bool {
        matches!(self, Self::SetName { .. })
    }
}

fn review_rows(
    cues: &[Box<dyn Cue>],
    parent: Option<Uuid>,
    rows: &mut Vec<Value>,
) -> Result<(), String> {
    for cue in cues {
        if rows.len() >= MAX_ROWS {
            return Err("This cue list exceeds the review limit of 4,096 cues".into());
        }
        rows.push(json!({
            "id": cue.id(), "parent_id": parent, "number": cue.number(), "name": cue.name()
        }));
        if let Some(children) = cue.child_cues() {
            review_rows(children, Some(cue.id()), rows)?;
        }
    }
    Ok(())
}

pub(super) fn apply_review(
    ws: &mut Workspace,
    request: ReviewRequest,
    history: Option<&mut UndoHistory>,
) -> Result<(Value, bool), String> {
    if request.owner().is_some_and(|id| id != ws.instance_id()) {
        return Err("The workspace changed. Reconnect from the desk.".into());
    }
    match request {
        ReviewRequest::Lists {} => {
            if ws.cue_lists.len() > MAX_ROWS {
                return Err("This workspace exceeds the review limit of 4,096 lists".into());
            }
            let lists: Vec<_> = ws
                .cue_lists
                .iter()
                .map(|list| json!({"id":list.id,"name":list.name}))
                .collect();
            Ok((
                json!({
                    "workspace_id":ws.instance_id(),"workspace_name":ws.metadata.name,"lists":lists
                }),
                false,
            ))
        }
        ReviewRequest::Cues { list_id, .. } => {
            let list = ws.cue_list_by_id(list_id).ok_or("Unknown cue list")?;
            let mut rows = Vec::new();
            review_rows(&list.cues, None, &mut rows)?;
            Ok((json!({"cues":rows}), false))
        }
        ReviewRequest::Note {
            list_id, cue_id, ..
        } => {
            let cue = ws
                .cue_list_by_id(list_id)
                .ok_or("Unknown cue list")?
                .get_recursive(&cue_id)
                .ok_or("Unknown cue in this list")?;
            Ok((json!({"note":cue.notes()}), false))
        }
        ReviewRequest::SetNote {
            list_id,
            cue_id,
            expected_note,
            note,
            ..
        } => {
            if note.len() > MAX_NOTE_BYTES || expected_note.len() > MAX_NOTE_BYTES {
                return Err(
                    "The current and replacement cue notes must each fit in 2,000 UTF-8 bytes"
                        .into(),
                );
            }
            match edit_note(ws, list_id, cue_id, &expected_note, note, history)? {
                CueNoteEdit::Saved { note, changed } => {
                    if changed {
                        record_review(ws, list_id, cue_id, ReviewChange::Note);
                    }
                    Ok((json!({"changed":changed,"note": note}), changed))
                }
                CueNoteEdit::Conflict { .. } =>
                    Err("The cue note changed. Read it again and reconcile your edit.".into()),
            }
        }
        ReviewRequest::Flag { list_id, cue_id, .. } => {
            let list = ws.cue_list_by_id(list_id).ok_or("Unknown cue list")?;
            list.get_recursive(&cue_id).ok_or("Unknown cue in this list")?;
            Ok((json!({ "flagged": list.flagged_cues.contains(&cue_id) }), false))
        }
        ReviewRequest::SetFlag { list_id, cue_id, expected_flagged, flagged, .. } => {
            let changed = edit_flag(ws, list_id, cue_id, Some(expected_flagged), flagged, history)?;
            if changed {
                let change = if flagged { ReviewChange::FlagSet } else { ReviewChange::FlagCleared };
                record_review(ws, list_id, cue_id, change);
            }
            Ok((json!({ "changed": changed, "flagged": flagged }), changed))
        }
        ReviewRequest::SetName { list_id, cue_id, expected_name, name, .. } => {
            if name.len() > MAX_NAME_BYTES || expected_name.len() > MAX_NAME_BYTES {
                return Err("The current and replacement cue names must each fit in 2,000 UTF-8 bytes".into());
            }
            let list = ws.cue_list_by_id_mut(list_id).ok_or("Unknown cue list")?;
            let cue = list.get_mut_recursive(&cue_id).ok_or("Unknown cue in this list")?;
            if cue.name() != expected_name {
                return Err("The cue name changed. Read the cue list again before renaming it.".into());
            }
            let changed = name != expected_name;
            if changed {
                let history = history.ok_or("The desk undo history is unavailable")?;
                history.push_name_edit(list_id, cue_id, expected_name);
                cue.set_name(name.clone());
                record_review(ws, list_id, cue_id, ReviewChange::Name);
                ws.mark_modified();
            }
            Ok((json!({ "changed": changed, "name": name }), changed))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{memo_cue::MemoCue, registry::CueRegistry};
    use crate::show::undo_stack::HistoryDirection;

    #[test]
    fn notes_are_compare_and_set_on_the_exact_open_instance_and_undoable() {
        let mut ws = Workspace::new("Rehearsal");
        let owner = ws.instance_id();
        let list = ws.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let mut cue = MemoCue::new();
        let cue_id = cue.id();
        cue.set_notes("Before".into());
        list.push(Box::new(cue));
        let standby = MemoCue::new();
        let standby_id = standby.id();
        list.push(Box::new(standby));
        list.set_playhead(Some(cue_id)).unwrap();
        let mut history = UndoHistory::new();
        let edit = |id, before: &str, after: &str| ReviewRequest::SetNote {
            workspace_id: id,
            list_id,
            cue_id,
            expected_note: before.into(),
            note: after.into(),
        };
        let original_revision = ws.revision;
        assert!(apply_review(
            &mut ws,
            edit(Uuid::new_v4(), "Before", "Wrong workspace"),
            Some(&mut history)
        )
        .is_err());
        assert!(apply_review(
            &mut ws,
            edit(owner, "Before", &"é".repeat(1_001)),
            Some(&mut history)
        )
        .is_err());
        assert!(
            !apply_review(&mut ws, edit(owner, "Before", "Before"), Some(&mut history))
                .unwrap()
                .1
        );
        assert_eq!(ws.revision, original_revision);
        assert!(!history.can_undo(list_id));
        assert!(
            apply_review(
                &mut ws,
                edit(owner, "Before", "The chorus enters"),
                Some(&mut history)
            )
            .unwrap()
            .1
        );
        assert!(apply_review(
            &mut ws,
            edit(owner, "Before", "Stale overwrite"),
            Some(&mut history)
        )
        .is_err());
        assert_eq!(ws.revision, original_revision + 1);
        assert_eq!(ws.programming_history.len(), 1);
        let request = || ReviewRequest::Note {
            workspace_id: owner,
            list_id,
            cue_id,
        };
        assert_eq!(
            apply_review(&mut ws, request(), None).unwrap().0["note"],
            "The chorus enters"
        );
        ws.active_cue_list_mut().unwrap().set_playhead(Some(standby_id)).unwrap();
        let registry = CueRegistry::new();
        for (direction, note) in [
            (HistoryDirection::Undo, "Before"),
            (HistoryDirection::Redo, "The chorus enters"),
        ] {
            assert!(super::super::undo_cmds::apply_history(
                &mut ws, &mut history, &registry, direction,
            ).unwrap());
            assert_eq!(apply_review(&mut ws, request(), None).unwrap().0["note"], note);
            assert_eq!(ws.active_cue_list().unwrap().playhead_cue_id, Some(standby_id));
        }
    }

    #[test]
    fn review_flags_are_scope_checked_conflict_safe_audited_and_undoable() {
        let mut workspace = Workspace::new("Rehearsal");
        let owner = workspace.instance_id();
        let list = workspace.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let mut cue = MemoCue::new();
        cue.set_notes("Keep the cue note".into());
        let cue_id = cue.id();
        list.push(Box::new(cue));
        let revision = workspace.revision;

        let read = || ReviewRequest::Flag { workspace_id: owner, list_id, cue_id };
        let edit = |workspace_id, expected_flagged, flagged| ReviewRequest::SetFlag {
            workspace_id, list_id, cue_id, expected_flagged, flagged,
        };
        let mut history = UndoHistory::new();
        assert_eq!(apply_review(&mut workspace, read(), None).unwrap(), (json!({ "flagged": false }), false));
        assert!(apply_review(&mut workspace, edit(Uuid::new_v4(), false, true), Some(&mut history)).is_err());
        assert!(apply_review(&mut workspace, edit(owner, false, true), None).is_err());
        assert_eq!(apply_review(&mut workspace, edit(owner, false, false), Some(&mut history)).unwrap(),
            (json!({ "flagged": false, "changed": false }), false));
        assert_eq!(workspace.revision, revision);
        assert!(!history.can_undo(list_id));

        assert_eq!(apply_review(&mut workspace, edit(owner, false, true), Some(&mut history)).unwrap(),
            (json!({ "flagged": true, "changed": true }), true));
        assert!(apply_review(&mut workspace, edit(owner, false, false), Some(&mut history)).is_err());
        assert_eq!(workspace.programming_history[0]["cue_review"]["change"], "flag_set");

        assert!(apply_review(&mut workspace, edit(owner, true, false), Some(&mut history)).unwrap().1);
        assert_eq!(workspace.programming_history[1]["cue_review"]["change"], "flag_cleared");
        assert_eq!(workspace.revision, revision + 2);
        let mut registry = CueRegistry::new();
        registry.register(crate::cue::types::CueType::Memo, Box::new(crate::cue::memo_cue::MemoCueFactory));
        assert!(super::super::undo_cmds::apply_history(&mut workspace, &mut history, &registry, HistoryDirection::Undo).unwrap());
        assert_eq!(apply_review(&mut workspace, read(), None).unwrap().0["flagged"], true);
        let cue = workspace.active_cue_list().unwrap().get_recursive(&cue_id).unwrap();
        assert_eq!(cue.notes(), "Keep the cue note");
        assert!(!cue.is_disabled());
    }

    #[test]
    fn review_names_compare_exact_labels_and_undo_without_rebuilding_the_list() {
        let mut workspace = Workspace::new("Rehearsal");
        let owner = workspace.instance_id();
        let list = workspace.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let mut cue = MemoCue::new();
        cue.set_name("Before ".into());
        cue.set_notes("Keep notes".into());
        let id = cue.id();
        list.push(Box::new(cue));
        list.playhead_cue_id = Some(id);
        let revision = workspace.revision;
        let mut history = UndoHistory::new();
        let rename = |expected: &str, name: &str| ReviewRequest::SetName {
            workspace_id: owner, list_id, cue_id: id,
            expected_name: expected.into(), name: name.into(),
        };
        assert!(apply_review(&mut workspace, rename("Before", "After"), Some(&mut history)).is_err());
        assert!(!apply_review(&mut workspace, rename("Before ", "Before "), None).unwrap().1);
        assert_eq!(workspace.revision, revision);
        assert!(!history.can_undo(list_id));
        assert!(apply_review(&mut workspace, rename("Before ", "After"), Some(&mut history)).unwrap().1);
        assert_eq!(workspace.programming_history[0]["cue_review"]["change"], "name");
        assert!(apply_review(&mut workspace, rename("Before ", "Stale"), Some(&mut history)).is_err());
        let registry = CueRegistry::new();
        for (direction, expected) in [(HistoryDirection::Undo, "Before "), (HistoryDirection::Redo, "After")] {
            assert!(super::super::undo_cmds::apply_history(&mut workspace, &mut history, &registry, direction).unwrap());
            let list = workspace.active_cue_list().unwrap();
            let cue = list.get_recursive(&id).unwrap();
            assert_eq!(cue.name(), expected);
            assert_eq!(cue.notes(), "Keep notes");
            assert_eq!(list.playhead_cue_id, Some(id));
        }
    }

    #[test]
    fn local_note_conflicts_return_exact_text_and_long_local_notes_remain_editable() {
        let mut ws = Workspace::new("Rehearsal");
        let list = ws.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let mut cue = MemoCue::new();
        let cue_id = cue.id();
        let saved = format!("{}\r\n ", "é".repeat(1_200));
        cue.set_notes(saved.clone());
        list.push(Box::new(cue));
        let mut history = UndoHistory::new();
        let revision = ws.revision;
        let stale = saved.trim_end().to_owned();
        assert_eq!(
            edit_note(&mut ws, list_id, cue_id, &stale, "Draft".into(), Some(&mut history)).unwrap(),
            CueNoteEdit::Conflict { note: saved.clone() },
        );
        assert!(!history.can_undo(list_id));
        assert_eq!(ws.revision, revision);

        let replacement = format!("{saved}rehearsal");
        assert_eq!(
            edit_note(&mut ws, list_id, cue_id, &saved, replacement.clone(), Some(&mut history)).unwrap(),
            CueNoteEdit::Saved { note: replacement, changed: true },
        );
        assert!(history.can_undo(list_id));
        assert_eq!(ws.revision, revision + 1);
        assert!(ws.programming_history.is_empty());
    }

    #[test]
    fn protocol_excludes_transport_and_generic_cue_properties() {
        assert!(serde_json::from_value::<ReviewRequest>(
            json!({"command":"transport","action":"go"})
        )
        .is_err());
        for command in ["lists", "note"] {
            assert!(serde_json::from_value::<ReviewRequest>(json!({
                "command":command,"volume_db":0,"script":"replacement"
            }))
            .is_err());
        }
    }
}
