//! Opt-in read-only checks against two author-published demonstrations.
//! See docs/operator-guide/qlab3 for the pinned sources. No media or outputs are needed.
use std::{collections::HashMap, path::PathBuf};

use inkue_lib::qlab_import::{import_workspace, ImportReport};
use serde_json::Value;
use sha2::{Digest, Sha256};

fn fixture(variable: &str, digest: &str) -> (Value, ImportReport) {
    let path = PathBuf::from(std::env::var_os(variable).expect(variable));
    let contents = std::fs::read(&path).unwrap();
    assert_eq!(format!("{:x}", Sha256::digest(contents)), digest);
    let (document, report) = import_workspace(&path).unwrap();
    (serde_json::from_str(&document).unwrap(), report)
}

fn cues(document: &Value) -> Vec<&Value> {
    fn visit<'a>(items: &'a Value, output: &mut Vec<&'a Value>) {
        for cue in items.as_array().into_iter().flatten() {
            output.push(cue);
            visit(&cue["children"], output);
        }
    }
    let mut output = Vec::new();
    for list in document["cue_lists"].as_array().unwrap() {
        visit(&list["cues"], &mut output);
    }
    output
}

#[test]
#[ignore = "requires the pinned QLX 1.2 demo in INKUE_QLX3_FIXTURE"]
fn qlx_keeps_lists_continuations_media_and_start_target_without_executing_scripts() {
    let (doc, report) = fixture(
        "INKUE_QLX3_FIXTURE",
        "cf7471f80a96212d0381464895cee13ef21691e401c3370adff0558522a1bc50",
    );
    assert_eq!((report.cue_list_count, report.cue_count), (2, 50));
    let flat = cues(&doc);
    assert_eq!(flat.len(), 50);
    let by_id: HashMap<_, _> = flat
        .iter()
        .map(|c| (c["id"].as_str().unwrap(), *c))
        .collect();
    assert_eq!(by_id.len(), flat.len());
    assert_eq!(
        flat.iter()
            .filter(|c| c["continue_mode"] == "auto_follow")
            .count(),
        1
    );
    assert_eq!(
        flat.iter()
            .filter(|c| c["continue_mode"] == "auto_continue")
            .count(),
        6
    );
    let scripts: Vec<_> = flat.iter().filter(|c| c["cue_type"] == "script").collect();
    assert_eq!(scripts.len(), 36);
    assert!(scripts.iter().all(|c| c["is_disabled"] == true
        && c["command"] == ""
        && c["args"].as_array().unwrap().is_empty()));
    let start = flat.iter().find(|c| c["cue_type"] == "start").unwrap();
    assert_eq!(start["is_disabled"], true);
    let target = by_id[start["target_cue_ids"][0].as_str().unwrap()];
    assert_eq!(target["name"], "QLXC Chan 1 at Full");
    let audio = flat.iter().find(|c| c["cue_type"] == "audio").unwrap();
    let relative = audio["file_path"].as_str().unwrap();
    assert!(relative.starts_with("../") && relative.ends_with(".m4a"));
    assert_eq!(audio["end_time_ms"], 256560);
    assert_eq!(audio["is_disabled"], true);
    assert!(audio["output_patch_id"].is_null());
}

#[test]
#[ignore = "requires the pinned Q-Chromakey demo in INKUE_CHROMA3_FIXTURE"]
fn chromakey_keeps_original_order_and_all_camera_and_osc_outputs_inert() {
    let (doc, report) = fixture(
        "INKUE_CHROMA3_FIXTURE",
        "bd070e3246ee87028b07da9365fb689d5aa847d16b7a5d8982f98020bfb9d71f",
    );
    assert_eq!((report.cue_list_count, report.cue_count), (1, 12));
    let flat = cues(&doc);
    assert_eq!(flat.len(), 12);
    assert!(flat.iter().all(|c| c["cue_type"] == "memo"));
    let external: Vec<_> = report
        .cues
        .iter()
        .enumerate()
        .filter(|(_, c)| matches!(c.qlab_class.as_str(), "CameraCue" | "OSCCue"))
        .collect();
    assert_eq!(external.len(), 4);
    assert!(external
        .iter()
        .all(|(i, c)| flat[*i]["is_disabled"] == true && c.note.is_some()));
    for key in [
        "video_outputs",
        "osc_patches",
        "universe_outputs",
        "output_patches",
    ] {
        assert!(doc[key].as_array().unwrap().is_empty());
    }
}
