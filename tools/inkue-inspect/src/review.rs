use crate::live::LiveDesk;
use anyhow::{ensure, Context, Result};
use clap::{Subcommand, ValueEnum};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

const MAX_NOTE_BYTES: usize = 2_000;

#[derive(Clone, Copy, ValueEnum)]
pub(crate) enum FlagValue {
    Clear,
    Set,
}

#[derive(Subcommand)]
pub(crate) enum ReviewCommand {
    /// List cue lists in the workspace open for local review.
    #[command(name = "review-lists")]
    Lists,
    /// List cue IDs and their parents in one cue list.
    #[command(name = "review-cues")]
    Cues { list_id: String },
    /// Read the current cue note as JSON.
    #[command(name = "review-note")]
    Note { list_id: String, cue_id: String },
    /// Replace a cue note if it still exactly matches the expected file.
    #[command(name = "review-set-note")]
    SetNote {
        list_id: String,
        cue_id: String,
        /// File containing the original note, including its whitespace.
        #[arg(long, visible_alias = "from")]
        expected_file: PathBuf,
        /// File containing the replacement note. An empty file clears it.
        #[arg(long, visible_alias = "to")]
        note_file: PathBuf,
    },
    /// Read the current cue review flag as JSON.
    #[command(name = "review-flag")]
    Flag { list_id: String, cue_id: String },
    /// Set or clear a cue review flag if its current value still matches.
    #[command(name = "review-set-flag")]
    SetFlag {
        list_id: String,
        cue_id: String,
        #[arg(long, value_enum)]
        expected: FlagValue,
        #[arg(long, value_enum)]
        flag: FlagValue,
    },
}

#[derive(Serialize)]
#[serde(tag = "command", rename_all = "snake_case")]
enum Request<'a> {
    Lists,
    Cues {
        workspace_id: &'a str,
        list_id: &'a str,
    },
    Note {
        workspace_id: &'a str,
        list_id: &'a str,
        cue_id: &'a str,
    },
    SetNote {
        workspace_id: &'a str,
        list_id: &'a str,
        cue_id: &'a str,
        expected_note: &'a str,
        note: &'a str,
    },
    Flag {
        workspace_id: &'a str,
        list_id: &'a str,
        cue_id: &'a str,
    },
    SetFlag {
        workspace_id: &'a str,
        list_id: &'a str,
        cue_id: &'a str,
        expected_flagged: bool,
        flagged: bool,
    },
}

#[derive(Deserialize, Serialize)]
struct Lists {
    workspace_id: String,
    workspace_name: String,
    lists: Vec<ListRow>,
}

#[derive(Deserialize, Serialize)]
struct ListRow {
    id: String,
    name: String,
}

#[derive(Deserialize, Serialize)]
struct Cues {
    cues: Vec<CueRow>,
}

#[derive(Deserialize, Serialize)]
struct CueRow {
    id: String,
    parent_id: Option<String>,
    number: Option<String>,
    name: String,
}

#[derive(Deserialize, Serialize)]
struct Note {
    note: String,
}

#[derive(Deserialize, Serialize)]
struct NoteChange {
    changed: bool,
    note: String,
}

#[derive(Deserialize, Serialize)]
struct Flag {
    flagged: bool,
}

#[derive(Deserialize, Serialize)]
struct FlagChange {
    changed: bool,
    flagged: bool,
}

pub(crate) fn run(desk: &LiveDesk, command: ReviewCommand) -> Result<Value> {
    let result = match command {
        ReviewCommand::Lists => serde_json::to_value(desk.review::<Lists>(&Request::Lists)?),
        ReviewCommand::Cues { list_id } => {
            let workspace_id = workspace_id(desk)?;
            serde_json::to_value(desk.review::<Cues>(&Request::Cues {
                workspace_id: &workspace_id,
                list_id: &list_id,
            })?)
        }
        ReviewCommand::Note { list_id, cue_id } => {
            let workspace_id = workspace_id(desk)?;
            serde_json::to_value(desk.review::<Note>(&Request::Note {
                workspace_id: &workspace_id,
                list_id: &list_id,
                cue_id: &cue_id,
            })?)
        }
        ReviewCommand::SetNote {
            list_id,
            cue_id,
            expected_file,
            note_file,
        } => {
            let expected = read_note(&expected_file).context("read the expected note file")?;
            let note = read_note(&note_file).context("read the replacement note file")?;
            let workspace_id = workspace_id(desk)?;
            serde_json::to_value(desk.review::<NoteChange>(&Request::SetNote {
                workspace_id: &workspace_id,
                list_id: &list_id,
                cue_id: &cue_id,
                expected_note: &expected,
                note: &note,
            })?)
        }
        ReviewCommand::Flag { list_id, cue_id } => {
            let workspace_id = workspace_id(desk)?;
            serde_json::to_value(desk.review::<Flag>(&Request::Flag {
                workspace_id: &workspace_id,
                list_id: &list_id,
                cue_id: &cue_id,
            })?)
        }
        ReviewCommand::SetFlag { list_id, cue_id, expected, flag } => {
            let workspace_id = workspace_id(desk)?;
            serde_json::to_value(desk.review::<FlagChange>(&Request::SetFlag {
                workspace_id: &workspace_id,
                list_id: &list_id,
                cue_id: &cue_id,
                expected_flagged: matches!(expected, FlagValue::Set),
                flagged: matches!(flag, FlagValue::Set),
            })?)
        }
    }?;
    Ok(result)
}

fn workspace_id(desk: &LiveDesk) -> Result<String> {
    Ok(desk.review::<Lists>(&Request::Lists)?.workspace_id)
}

fn read_note(path: &Path) -> Result<String> {
    let mut bytes = Vec::new();
    File::open(path)?
        .take((MAX_NOTE_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= MAX_NOTE_BYTES,
        "cue notes may contain at most 2,000 UTF-8 bytes"
    );
    String::from_utf8(bytes).context("cue note must be UTF-8")
}
