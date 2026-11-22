//! Read the primary desk's saved cue-review metadata.

use crate::{show::Workspace, state::AppState};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use tauri::State;
use uuid::Uuid;

use super::review_cmds::ReviewChange;

const MAX_ROWS: usize = 50;
const MAX_EVENTS: usize = 4_096;

#[derive(Deserialize)]
struct CueReviewAudit {
    time: DateTime<Utc>,
    cue_review: CueReviewTarget,
}

#[derive(Deserialize)]
struct CueReviewTarget {
    list_id: Uuid,
    cue_id: Uuid,
    #[serde(default)]
    change: ReviewChange,
}

#[derive(Serialize)]
pub struct CueReviewHistory {
    workspace_id: Uuid,
    workspace_name: String,
    rows: Vec<CueReviewHistoryRow>,
}

#[derive(Serialize)]
struct CueReviewHistoryRow {
    event_index: usize,
    time: DateTime<Utc>,
    list_name: Option<String>,
    cue_number: Option<String>,
    cue_name: Option<String>,
    change: ReviewChange,
}

/// Return recent review edits for the exact workspace displayed by the desk.
#[tauri::command]
pub fn get_cue_review_history(
    workspace_id: Uuid,
    state: State<'_, AppState>,
) -> Result<CueReviewHistory, String> {
    let ws = state.workspace.try_lock().map_err(|_| "Desk busy, refresh the history.")?;
    if ws.instance_id() != workspace_id {
        return Err("The workspace changed. Open Cue review history again.".into());
    }
    Ok(project(&ws))
}

fn project(ws: &Workspace) -> CueReviewHistory {
    let rows = ws.programming_history.iter().enumerate().rev().take(MAX_EVENTS)
        .filter_map(|(index, value)| {
            CueReviewAudit::deserialize(value).ok().map(|audit| (index, audit))
        })
        .take(MAX_ROWS)
        .map(|(event_index, audit)| {
            let list = ws.cue_list_by_id(audit.cue_review.list_id);
            let cue = list.and_then(|list| list.get_recursive(&audit.cue_review.cue_id));
            CueReviewHistoryRow {
                event_index,
                time: audit.time,
                list_name: list.map(|list| list.name.clone()),
                cue_number: cue.and_then(|cue| cue.number().map(str::to_owned)),
                cue_name: cue.map(|cue| cue.name().to_owned()),
                change: audit.cue_review.change,
            }
        })
        .collect();
    CueReviewHistory { workspace_id: ws.instance_id(), workspace_name: ws.metadata.name.clone(), rows }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{memo_cue::MemoCue, traits::Cue};
    use serde_json::json;

    #[test]
    fn history_resolves_current_labels_and_reports_removed_targets_without_changing_the_show() {
        let mut ws = Workspace::new("Tech");
        let list = ws.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let mut cue = MemoCue::new();
        cue.set_name("Current label".into());
        cue.set_number(Some("A4".into()));
        let cue_id = cue.id();
        list.push(Box::new(cue));

        let audit = |list_id, cue_id| json!({
            "time": "2026-11-22T10:23:49Z",
            "cue_review": {"list_id": list_id, "cue_id": cue_id}
        });
        ws.programming_history = vec![
            json!({"agent": {"action": "start"}}),
            audit(list_id, cue_id),
            audit(list_id, Uuid::new_v4()),
            json!({"time": "malformed", "cue_review": {"list_id": list_id, "cue_id": cue_id}}),
            audit(Uuid::new_v4(), cue_id),
        ];
        ws.programming_history[1]["cue_review"]["change"] = json!("flag_set");
        let before = ws.revision;
        let history = project(&ws);
        assert_eq!(history.workspace_id, ws.instance_id());
        assert_eq!(history.workspace_name, "Tech");
        assert_eq!(history.rows.iter().map(|row| row.event_index).collect::<Vec<_>>(), [4, 2, 1]);
        assert_eq!(history.rows[0].list_name, None);
        assert!(history.rows[1].list_name.is_some());
        assert_eq!(history.rows[1].cue_name, None);
        assert_eq!(history.rows[2].cue_number.as_deref(), Some("A4"));
        assert_eq!(history.rows[2].cue_name.as_deref(), Some("Current label"));
        assert_eq!(history.rows[2].change, ReviewChange::FlagSet);
        assert_eq!(history.rows[0].change, ReviewChange::Note);
        assert_eq!(ws.revision, before);

        for _ in 0..MAX_ROWS + 2 {
            ws.programming_history.push(audit(list_id, cue_id));
        }
        let recent = project(&ws);
        assert_eq!(recent.rows.len(), MAX_ROWS);
        assert_eq!(recent.rows[0].event_index, ws.programming_history.len() - 1);
    }
}
