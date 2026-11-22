//! Read model for the saved workspace. Reading a cue never resolves its media,
//! starts the playback engine or runs a script stored in the show.
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{fs::File, path::Path};

#[derive(Deserialize)]
pub struct Show {
    #[serde(default)]
    schema_version: u64,
    workspace: Metadata,
    cue_lists: Vec<CueList>,
    active_cue_list_id: Option<String>,
}

#[derive(Deserialize)]
struct Metadata {
    name: String,
    modified_at: Option<String>,
}

#[derive(Deserialize)]
struct CueList {
    id: String,
    name: String,
    mode: Option<String>,
    playhead_cue_id: Option<String>,
    cues: Vec<Cue>,
}

#[derive(Deserialize)]
struct Cue {
    id: String,
    number: Option<String>,
    name: String,
    #[serde(rename = "type")]
    kind: String,
    #[serde(default)]
    children: Vec<Cue>,
    #[serde(flatten)]
    properties: serde_json::Map<String, Value>,
}

pub enum Query<'a> {
    Summary,
    List(Option<&'a str>),
    Cue(&'a str),
}

impl Show {
    pub fn open(path: &Path) -> Result<Self> {
        let file = File::open(path).with_context(|| format!("open {}", path.display()))?;
        Self::from_reader(std::io::BufReader::new(file))
    }

    pub fn from_reader(reader: impl std::io::Read) -> Result<Self> {
        let show: Self = serde_json::from_reader(reader).context("read Inkue workspace JSON")?;
        if show.schema_version > 3 {
            bail!("unsupported workspace schema {}", show.schema_version);
        }
        Ok(show)
    }

    pub fn media_paths(&self) -> Result<std::collections::BTreeSet<&str>> {
        let mut paths = std::collections::BTreeSet::new();
        for (_, cue) in self.cue_lists.iter().flat_map(CueList::walk) {
            if matches!(cue.kind.as_str(), "audio" | "video" | "image" | "midi_file") {
                let path = cue
                    .properties
                    .get("file_path")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .with_context(|| format!("cue {} ({}) has no media file", cue.name, cue.id))?;
                paths.insert(path);
            }
        }
        Ok(paths)
    }

    pub fn query(&self, query: Query<'_>) -> Result<Value> {
        match query {
            Query::Summary => Ok(json!({
                "source": "saved_workspace",
                "name": self.workspace.name,
                "modified_at": self.workspace.modified_at,
                "active_cue_list_id": self.active_cue_list_id,
                "lists": self.cue_lists.iter().map(|list| json!({
                    "id": list.id, "name": list.name, "mode": list.mode.as_deref().unwrap_or("sequential"),
                    "saved_playhead_cue_id": list.playhead_cue_id,
                    "cue_count": list.walk().count(),
                })).collect::<Vec<_>>()
            })),
            Query::List(filter) => {
                if let Some(id) = filter {
                    if !self.cue_lists.iter().any(|list| list.id == id) {
                        bail!("unknown cue list {id}");
                    }
                }
                let mut rows = Vec::new();
                for list in self
                    .cue_lists
                    .iter()
                    .filter(|l| filter.is_none_or(|id| l.id == id))
                {
                    for (parent, cue) in list.walk() {
                        rows.push(json!({"list_id": list.id, "parent_id": parent, "id": cue.id,
                            "number": cue.number, "name": cue.name, "type": cue.kind,
                            "is_disabled": cue.properties.get("is_disabled").and_then(Value::as_bool).unwrap_or(false)}));
                    }
                }
                Ok(json!(rows))
            }
            Query::Cue(id) => {
                let mut matches = self
                    .cue_lists
                    .iter()
                    .flat_map(|list| list.walk())
                    .filter(|(_, cue)| cue.id == id);
                let (_, cue) = matches.next().context("cue ID not found")?;
                if matches.next().is_some() {
                    bail!("duplicate cue ID in saved show; open and save a repaired copy in Inkue");
                }
                Ok(
                    json!({"id": cue.id, "number": cue.number, "name": cue.name, "type": cue.kind,
                    "child_ids": cue.children.iter().map(|child| &child.id).collect::<Vec<_>>(),
                    "properties": cue.properties}),
                )
            }
        }
    }
}

impl CueList {
    fn walk(&self) -> impl Iterator<Item = (Option<&str>, &Cue)> {
        let mut pending: Vec<_> = self.cues.iter().rev().map(|c| (None, c)).collect();
        std::iter::from_fn(move || {
            let (parent, cue) = pending.pop()?;
            pending.extend(
                cue.children
                    .iter()
                    .rev()
                    .map(|child| (Some(cue.id.as_str()), child)),
            );
            Some((parent, cue))
        })
    }
}
