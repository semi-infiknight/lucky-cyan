//! Command vocabulary shared by the native desk, CLI and MCP server.
mod cue_sheet;
pub use cue_sheet::{AudioMark, CueSheet};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Go,
    Stop,
    Panic,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub enum Command {
    Summary,
    LiveStatus,
    List {
        list_id: Option<Uuid>,
    },
    Cue {
        id: Uuid,
    },
    Transport {
        action: Action,
        #[serde(default)]
        request_approval: bool,
    },
}

#[derive(Serialize, Deserialize)]
pub struct LiveStatus {
    pub workspace_id: Uuid,
    pub name: String,
    pub active_cue_list_id: Uuid,
    pub lists: Vec<LiveList>,
}

#[derive(Serialize, Deserialize)]
pub struct LiveList {
    pub id: Uuid,
    pub name: String,
    pub standby: Option<LiveCue>,
    pub active: Vec<LiveRun>,
}

#[derive(Serialize, Deserialize)]
pub struct LiveCue {
    pub id: Uuid,
    pub number: Option<String>,
    pub name: String,
    pub kind: String,
    pub is_disabled: bool,
}

#[derive(Serialize, Deserialize)]
pub struct LiveRun {
    pub cue: LiveCue,
    pub parent_id: Option<Uuid>,
    pub state: LiveRunState,
    pub elapsed_ms: u64,
    pub remaining_ms: Option<u64>,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveRunState {
    Running,
    Paused,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    pub port: u16,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub osc_port: Option<u16>,
    pub token: String,
}
