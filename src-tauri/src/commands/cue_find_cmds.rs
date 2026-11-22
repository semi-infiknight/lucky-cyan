//! Read-only search of authored cue content in the displayed list or cart.

use std::any::Any;

use tauri::State;
use uuid::Uuid;

use crate::{
    cue::{
        osc_cue::OscCue,
        osc_types::{OscArg, RawDatagram},
        script_cue::ScriptCue,
        text_cue::TextCue,
        traits::Cue,
    },
    show::Workspace,
    state::AppState,
};

/// Find matching cue IDs in show order for the current list and workspace.
#[tauri::command]
pub fn find_cues(
    workspace_id: Uuid,
    list_id: Uuid,
    query: String,
    state: State<'_, AppState>,
) -> Result<Vec<Uuid>, String> {
    let workspace = state.workspace.lock().map_err(|error| error.to_string())?;
    find_in_list(&workspace, workspace_id, list_id, &query)
}

fn find_in_list(
    workspace: &Workspace,
    workspace_id: Uuid,
    list_id: Uuid,
    query: &str,
) -> Result<Vec<Uuid>, String> {
    if workspace.instance_id() != workspace_id || workspace.active_cue_list_id != list_id {
        return Err("The workspace or cue list changed. Search the displayed list again.".into());
    }
    let mut found = Vec::new();
    let query = query.trim().to_lowercase();
    if !query.is_empty() {
        let list = workspace.active_cue_list().ok_or("No active cue list")?;
        collect_matches(&list.cues, &query, &mut found);
    }
    Ok(found)
}

fn collect_matches(cues: &[Box<dyn Cue>], query: &str, found: &mut Vec<Uuid>) {
    for cue in cues {
        if matches_cue(cue.as_ref(), query) {
            found.push(cue.id());
        }
        if let Some(children) = cue.child_cues() {
            collect_matches(children, query, found);
        }
    }
}

fn matches_cue(cue: &dyn Cue, query: &str) -> bool {
    let includes = |text: &str| text.to_lowercase().contains(query);
    if includes(cue.name())
        || cue.number().is_some_and(includes)
        || includes(cue.notes())
        || cue.memo_text().is_some_and(includes)
        || cue
            .media_file_path()
            .and_then(|path| path.file_name())
            .is_some_and(|name| includes(&name.to_string_lossy()))
    {
        return true;
    }

    let cue = cue as &dyn Any;
    if let Some(text) = cue.downcast_ref::<TextCue>() {
        return includes(&text.text);
    }
    if let Some(script) = cue.downcast_ref::<ScriptCue>() {
        return includes(&script.command) || script.args.iter().any(|arg| includes(arg));
    }
    cue.downcast_ref::<OscCue>().is_some_and(|network| {
        network.messages.iter().any(|message| match &message.raw {
            Some(RawDatagram::Text(data) | RawDatagram::Hex(data)) => includes(data),
            None => {
                includes(&message.address)
                    || message.args.iter().any(|arg| match arg {
                        OscArg::Str(value) => includes(value),
                        OscArg::Int(value) => includes(&value.to_string()),
                        OscArg::Float(value) => includes(&value.to_string()),
                        OscArg::Bool(value) => includes(&value.to_string()),
                    })
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        cue::{audio_cue::AudioCue, group_cue::GroupCue, memo_cue::MemoCue, osc_types::OscMessage},
        show::cue_list::CueList,
    };

    #[test]
    fn scoped_find_reads_nested_cue_content_in_show_order() {
        let mut workspace = Workspace::new("Find practice");
        let owner = workspace.instance_id();
        let list = workspace.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let mut note = MemoCue::new();
        note.set_notes("Hold for NORTH".into());
        note.set_number(Some("B.19".into()));
        let note_id = note.id();
        list.push(Box::new(note));
        let mut group = GroupCue::new();
        let mut text = TextCue::new();
        text.text = "North entrance".into();
        let text_id = text.id();
        group.children.push(Box::new(text));
        let mut network = OscCue::new();
        network.messages.push(OscMessage {
            patch_id: Uuid::new_v4(),
            address: "/standby".into(),
            raw: None,
            args: vec![OscArg::Str("north".into())],
        });
        network.messages.push(OscMessage {
            patch_id: Uuid::new_v4(),
            address: String::new(),
            raw: Some(RawDatagram::Text("LAMP 17".into())),
            args: vec![],
        });
        let network_id = network.id();
        group.children.push(Box::new(network));
        let mut script = ScriptCue::new();
        script.args.push("--place=NORTH".into());
        let script_id = script.id();
        group.children.push(Box::new(script));
        list.push(Box::new(group));
        let mut audio = AudioCue::new();
        audio.file_path = Some("hidden-directory/North.WAV".into());
        let audio_id = audio.id();
        list.push(Box::new(audio));
        let mut other = CueList::new("Another cart");
        let mut unrelated = MemoCue::new();
        unrelated.set_name("North".into());
        other.push(Box::new(unrelated));
        let other_id = other.id;
        workspace.cue_lists.push(other);
        let revision = workspace.revision;
        let find = |query| find_in_list(&workspace, owner, list_id, query).unwrap();

        assert_eq!(
            find(" NORTH "),
            [note_id, text_id, network_id, script_id, audio_id]
        );
        assert_eq!(find("lamp 17"), [network_id]);
        assert_eq!(find("b.19"), [note_id]);
        assert!(find("hidden-directory").is_empty());
        assert!(find("   ").is_empty());
        assert!(find_in_list(&workspace, owner, other_id, "north").is_err());
        assert!(find_in_list(&workspace, Uuid::new_v4(), list_id, "north").is_err());
        assert_eq!(workspace.revision, revision);
        assert_eq!(
            workspace.active_cue_list().unwrap().playhead_cue_id,
            Some(note_id)
        );
    }
}
