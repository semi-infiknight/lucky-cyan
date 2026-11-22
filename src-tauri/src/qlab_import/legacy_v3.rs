//! Verified subset of the 3.0.14 keyed archive. This extension also existed
//! before QLab 3, so the file name alone never selects this reader.
use std::{collections::HashSet, path::Path};

use anyhow::{bail, ensure, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub(super) fn applies(path: &Path, outer: &Value) -> Result<bool> {
    let extension = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    let version = outer["QLabShortVersionString"].as_str().unwrap_or("");
    if !["cues", "qlab3"]
        .iter()
        .any(|s| extension.eq_ignore_ascii_case(s))
        && !version.starts_with("3.")
    {
        return Ok(false);
    }
    ensure!(version == "3.0.14", "Legacy workspace version {} is unverified. This reader accepts QLab 3.0.14. Open a separately saved QLab 4/5 copy or reconstruct the cue sheet.", if version.is_empty() { "unknown" } else { version });
    Ok(true)
}

fn cue_id(node: &Value) -> Result<&str> {
    node["uniqueID"]
        .as_str()
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .context("Missing or invalid QLab 3 cue ID")
}

fn collect_ids(node: &Value, ids: &mut HashSet<String>) -> Result<()> {
    let id = cue_id(node)?;
    ensure!(ids.insert(id.to_owned()), "Duplicate QLab 3 cue ID {id}");
    if node["__class__"] == "GroupCue" {
        for child in node["cues"]
            .as_array()
            .context("Missing QLab 3 group children")?
        {
            collect_ids(child, ids)?;
        }
    } else if node.get("cues").is_some_and(|c| !c.is_null()) {
        bail!("Unverified QLab 3 cue container for ID {id}");
    }
    Ok(())
}

// Decimal IDs are scoped to the source workspace. The same source always
// produces the same ID, including when a control points forward or into a group.
fn scoped_id(workspace: Uuid, id: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"Inkue QLab 3 cue ID\0");
    hash.update(workspace.as_bytes());
    hash.update(id.as_bytes());
    let bytes: [u8; 16] = hash.finalize()[..16].try_into().unwrap();
    uuid::Builder::from_custom_bytes(bytes)
        .into_uuid()
        .to_string()
}

fn normalize(node: &mut Value, workspace: Uuid, ids: &HashSet<String>) {
    let id = scoped_id(workspace, node["uniqueID"].as_str().unwrap());
    node["uniqueID"] = json!(id);
    if node["__class__"] == "StartCue" {
        node["cueTargetUniqueID"] = node["cueTargetUniqueID"]
            .as_str()
            .filter(|target| ids.contains(*target))
            .map(|target| json!(scoped_id(workspace, target)))
            .unwrap_or(Value::Null);
    }
    // 3.0.14 stores relativePath on the cue, beside its macOS alias.
    if node["__class__"] == "AudioCue" {
        node["fileTarget"] = json!({"relativePath": node["relativePath"].as_str()});
    }
    if let Some(children) = node["cues"].as_array_mut() {
        for child in children {
            normalize(child, workspace, ids);
        }
    }
}

pub(super) fn prepare(outer: &Value, root: &mut Value) -> Result<()> {
    let workspace = outer["uniqueID"]
        .as_str()
        .and_then(|id| Uuid::parse_str(id).ok())
        .context("Missing QLab 3 workspace ID")?;
    ensure!(
        root["__class__"] == "GroupCue",
        "Unverified QLab 3 workspace container"
    );
    let lists = root["cues"]
        .as_array_mut()
        .context("Missing QLab 3 cue lists")?;
    ensure!(!lists.is_empty(), "No QLab 3 cue lists");
    let mut ids = HashSet::new();
    for list in lists.iter() {
        ensure!(
            list["__class__"] == "GroupCue"
                && (list["groupMode"].is_null() || list["groupMode"] == 0),
            "Unverified QLab 3 cue list mode"
        );
        collect_ids(list, &mut ids)?;
    }
    if let Some(count) = outer["totalCues"].as_u64() {
        ensure!(
            ids.len() as u64 == count,
            "QLab 3 tree contains {} items but the archive declares {count}",
            ids.len()
        );
    }
    for list in lists {
        normalize(list, workspace, &ids);
    }
    Ok(())
}

/// Explicit allowlist. Unsupported outputs stay inert even if the modern
/// reader happens to know the same class name.
pub(super) fn verified_class(class: &str) -> bool {
    matches!(
        class,
        "AudioCue" | "MemoCue" | "ScriptCue" | "GroupCue" | "StartCue"
    )
}

pub(super) fn review(class: &str, cue: &Value) -> Option<&'static str> {
    let detail = match class {
        "AudioCue" => "QLab 3 audio is disarmed. Check media, levels, routing, effects and timing before arming.",
        "StartCue" if cue["cueTargetUniqueID"].is_null() => "QLab 3 Start target is missing. Reassign it before arming.",
        "StartCue" => "QLab 3 Start is disarmed. Check the linked target and its list before arming.",
        "GroupCue" => "QLab 3 group is disarmed. Review child order, group mode and GO behavior before arming.",
        "MemoCue" | "ScriptCue" => return None,
        _ => "Unverified QLab 3 cue class. Rebuild from the original inspector before use.",
    };
    Some(detail)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_legacy_suffix_requires_the_verified_archived_version() {
        for name in ["show.cues", "Show.CUES", "renamed.qlab3"] {
            assert!(applies(Path::new(name), &json!({"QLabShortVersionString":"3.0.14"})).unwrap());
            for header in [
                json!({}),
                json!({"QLabShortVersionString":"3.1"}),
                json!({"QLabShortVersionString":"2.2"}),
            ] {
                assert!(applies(Path::new(name), &header).is_err());
            }
        }
        assert!(applies(
            Path::new("renamed.qlab5"),
            &json!({"QLabShortVersionString":"3.1"})
        )
        .is_err());
        assert!(!applies(Path::new("modern.qlab4"), &json!({})).unwrap());
    }

    fn sample() -> (Value, Value) {
        let outer = json!({"uniqueID":"d8794e9e-7059-48c8-bb66-d8bf0d887c9b"});
        let root = json!({"__class__":"GroupCue", "cues":[{"__class__":"GroupCue","uniqueID":"0","cues":[
            {"__class__":"StartCue","uniqueID":"51","cueTargetUniqueID":"12"},
            {"__class__":"StartCue","uniqueID":"52","cueTargetUniqueID":"404"},
            {"__class__":"AudioCue","uniqueID":"12","relativePath":"../media/call.wav",
             "fileTarget":{"lastKnownPath":"/old/desk/media/wrong.wav"}}]}]});
        (outer, root)
    }

    #[test]
    fn forward_targets_are_stable_workspace_scoped_and_missing_targets_stay_empty() {
        let (mut outer, source) = sample();
        let convert = |outer: &Value| {
            let mut root = source.clone();
            prepare(outer, &mut root).unwrap();
            root["cues"][0]["cues"].clone()
        };
        let cues = convert(&outer);
        let target = &cues[2]["uniqueID"];
        assert_eq!(cues[0]["cueTargetUniqueID"], *target);
        assert!(Uuid::parse_str(target.as_str().unwrap()).is_ok());
        assert!(cues[1]["cueTargetUniqueID"].is_null());
        assert_eq!(cues[2]["fileTarget"]["relativePath"], "../media/call.wav");
        assert_eq!(convert(&outer), cues);
        outer["uniqueID"] = json!("d8794e9e-7059-48c8-bb66-d8bf0d887c9c");
        assert_ne!(convert(&outer)[2]["uniqueID"], *target);
    }

    #[test]
    fn malformed_trees_and_duplicate_ids_fail_explicitly() {
        let (outer, mut tree) = sample();
        tree["cues"][0]["cues"][0]["uniqueID"] = json!("12");
        assert!(prepare(&outer, &mut tree).is_err());
        for mut tree in [
            json!({}),
            json!({"__class__":"GroupCue", "cues":[]}),
            json!({"__class__":"GroupCue", "cues":[{"__class__":"GroupCue", "uniqueID":"0", "groupMode":5}]}),
        ] {
            assert!(prepare(&outer, &mut tree).is_err());
        }
    }
}
