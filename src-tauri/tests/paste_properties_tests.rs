mod common;

use inkue_lib::{
    commands::cue_properties_cmds::{apply_pasted_properties, PastePropertiesRequest},
    cue::types::CueType,
    show::{undo_stack::UndoHistory, Workspace},
    state::ClipboardCue,
};
use serde_json::json;
use uuid::Uuid;

#[test]
fn paste_refuses_paused_transport_in_another_cue_list() {
    let registry = common::full_registry();
    let (context, _, _) = common::recording_context();
    let mut workspace = Workspace::new("Practice");
    let mut other = inkue_lib::show::cue_list::CueList::new("Running cart");
    let mut wait = registry
        .from_json(json!({"type":"wait", "wait_duration_ms":10000}))
        .unwrap();
    wait.go(&context).unwrap();
    wait.pause(&context).unwrap();
    assert!(wait.is_paused());
    other.push(wait);
    workspace.cue_lists.push(other);
    let list = workspace.active_cue_list_mut().unwrap();
    let audio = registry.create(&CueType::Audio).unwrap();
    let id = audio.id();
    let list_id = list.id;
    list.push(audio);
    let clipboard = ClipboardCue {
        revision: Uuid::new_v4(),
        matrix_origin: None,
        audio: Default::default(),
        flagged_cues: Default::default(),
        second_colors: Default::default(),
        json: json!({"type":"audio", "volume_db":-12.0}),
    };
    let request: PastePropertiesRequest = serde_json::from_value(json!({
        "workspace_id": workspace.instance_id(), "cue_list_id": list_id, "clipboard_revision": clipboard.revision,
        "targets": [id], "groups": ["audio_level"],
    })).unwrap();
    let mut history = UndoHistory::new();
    let error =
        apply_pasted_properties(&mut workspace, &mut history, &registry, &clipboard, request)
            .unwrap_err();
    assert!(error.contains("running and paused"));
    assert!(!history.can_undo(list_id));
    assert_eq!(
        workspace
            .active_cue_list()
            .unwrap()
            .get_recursive(&id)
            .unwrap()
            .serialize()["volume_db"],
        0.0
    );
}
