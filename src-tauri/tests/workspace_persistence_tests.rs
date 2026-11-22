//! Zone (b) — workspace persistence & corrupt-file resilience.
//!
//! Two families:
//!  1. a real save → load round-trip through `Workspace::save`/`load`, and
//!  2. defensive loading of hand-crafted corrupt documents.
//!
//! Show-control angle: refusing to open a whole show over one bad cue is bad,
//! but silently dropping a cue the operator still sees in their paperwork is
//! *worse*. These tests pin down exactly which failures are fatal vs skipped,
//! and codify the silent-skip behaviour so any future change is deliberate.

mod common;

use common::*;
use inkue_lib::cue::types::CueType;
use inkue_lib::show::workspace::Workspace;

// ---------------------------------------------------------------------------
// Happy-path round-trip through the real on-disk save/load path
// ---------------------------------------------------------------------------

#[test]
fn projector_mask_is_embedded_in_save_collection_and_recovery() {
    use inkue_lib::engine::output_engine::{MaskImage, OutputConfig, MAIN_OUTPUT};

    let registry = full_registry();
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("arch.png");
    let mut png = png::Encoder::new(std::fs::File::create(&path).unwrap(), 2, 1);
    png.set_color(png::ColorType::Grayscale);
    png.set_depth(png::BitDepth::Eight);
    png.write_header().unwrap().write_image_data(&[0, 192]).unwrap();

    let mut ws = Workspace::new("Arch");
    let extra = OutputConfig::new("Front");
    let id = extra.id;
    ws.video_outputs.push(extra);
    ws.output_mask_images.insert(id, MaskImage::import_file(&path).unwrap());
    std::fs::remove_file(path).unwrap();
    let saved = dir.path().join("show.inkue");
    ws.save(Some(saved.clone())).unwrap();
    let collected = ws.collect_and_save(&dir.path().join("tour")).unwrap();
    let recovered = Workspace::from_json_str(&ws.to_recovery_json().unwrap(), None, &registry).unwrap();
    for reopened in [
        Workspace::load(saved, &registry).unwrap(),
        Workspace::load(collected.workspace_path.into(), &registry).unwrap(),
        recovered,
    ] {
        assert!(reopened.output_mask_images.get(&MAIN_OUTPUT).is_none());
        assert_eq!(reopened.output_mask_images[&id].pixels(), [0, 192]);
    }
}

#[test]
fn save_load_roundtrip_preserves_cues() {
    let registry = full_registry();
    let dir = temp_dir("ws_roundtrip");
    let path = dir.join("show.inkue");

    let mut ws = Workspace::new("My Show");
    {
        let list = ws.active_cue_list_mut().expect("default cue list");
        for (t, name) in [
            (CueType::Memo, "Intro note"),
            (CueType::Audio, "Walk-in music"),
            (CueType::Stop, "Kill music"),
        ] {
            let mut cue = registry.create(&t).unwrap();
            cue.set_name(name.to_string());
            list.push(cue);
        }
    }
    ws.save(Some(path.clone())).expect("save should succeed");
    assert!(path.exists(), ".inkue file must be written");

    let loaded = Workspace::load(path, &registry).expect("load should succeed");
    let list = loaded.active_cue_list().expect("active cue list after load");
    let names: Vec<&str> = list.cues.iter().map(|c| c.name()).collect();
    assert_eq!(list.cues.len(), 3, "all three cues must survive save/load");
    assert!(names.contains(&"Intro note"));
    assert!(names.contains(&"Walk-in music"));
    assert!(names.contains(&"Kill music"));
}

#[test]
fn source_channel_names_survive_audio_and_video_workspace_roundtrip() {
    let registry = full_registry();
    let path = temp_dir("source_names").join("labels.inkue");
    let mut ws = Workspace::new("Source labels");
    let list = ws.active_cue_list_mut().unwrap();
    for kind in [CueType::Audio, CueType::Video] {
        let mut old = registry.create(&kind).unwrap().serialize();
        old.as_object_mut().unwrap().remove("source_channel_names");
        let mut cue = registry.from_json(old).unwrap();
        assert!(cue.source_channel_names_mut().unwrap().is_empty());
        let names = cue.source_channel_names_mut().unwrap();
        names.resize(128, String::new());
        names[2] = "Dialogue".to_owned();
        names[127] = "Choir".to_owned();
        list.push(cue);
    }
    ws.save(Some(path.clone())).unwrap();
    let loaded = Workspace::load(path, &registry).unwrap();
    let list = loaded.active_cue_list().unwrap();
    assert_eq!(list.cues.len(), 2);
    for cue in &list.cues {
        let saved = cue.serialize();
        assert_eq!(saved["source_channel_names"][2], "Dialogue");
        assert_eq!(saved["source_channel_names"][127], "Choir");
        assert_eq!(saved["level_matrix"], serde_json::Value::Null);
    }
}

// ---------------------------------------------------------------------------
// Helpers to craft workspace documents
// ---------------------------------------------------------------------------

/// Wrap a list of cue JSON values into a minimal valid workspace document.
fn workspace_doc(schema: u32, cues: Vec<serde_json::Value>) -> String {
    let list_id = "11111111-1111-4111-8111-111111111111";
    serde_json::json!({
        "schema_version": schema,
        "workspace": {
            "name": "T",
            "created_at": "2024-01-01T00:00:00Z",
            "modified_at": "2024-01-01T00:00:00Z"
        },
        "cue_lists": [{
            "id": list_id,
            "name": "List 1",
            "mode": "sequential",
            "cues": cues
        }],
        "active_cue_list_id": list_id
    })
    .to_string()
}

/// A valid cue JSON of the given type, produced via the real serializer.
fn valid_cue(t: CueType, name: &str) -> serde_json::Value {
    let r = full_registry();
    let mut cue = r.create(&t).unwrap();
    cue.set_name(name.to_string());
    cue.serialize()
}

// ---------------------------------------------------------------------------
// Corrupt-document resilience
// ---------------------------------------------------------------------------

#[test]
fn load_skips_unknown_cue_type_but_keeps_the_rest() {
    let registry = full_registry();
    let cues = vec![
        valid_cue(CueType::Memo, "Good 1"),
        serde_json::json!({ "type": "quantum", "id": "22222222-2222-4222-8222-222222222222", "name": "Bad" }),
        valid_cue(CueType::Memo, "Good 2"),
    ];
    let doc = workspace_doc(1, cues);

    let ws = Workspace::from_json_str(&doc, None, &registry)
        .expect("workspace must still load despite one unrecognised cue");
    let list = ws.active_cue_list().unwrap();
    assert_eq!(list.cues.len(), 2, "the two valid cues survive; the unknown one is skipped");
    let names: Vec<&str> = list.cues.iter().map(|c| c.name()).collect();
    assert_eq!(names, vec!["Good 1", "Good 2"]);
}

#[test]
fn skipped_unknown_cue_is_counted() {
    // FINDING B1 (fixed): an unrecognised/malformed cue is still dropped, but the
    // loss is no longer silent — `cues_skipped_on_load` records it so the command
    // layer can raise an operator banner instead of the cue vanishing unnoticed.
    let registry = full_registry();
    let cues = vec![
        valid_cue(CueType::Memo, "Kept"),
        serde_json::json!({ "type": "not_a_real_type", "id": "33333333-3333-4333-8333-333333333333" }),
    ];
    let doc = workspace_doc(1, cues);

    let ws = Workspace::from_json_str(&doc, None, &registry).expect("loads");
    assert_eq!(ws.active_cue_list().unwrap().cues.len(), 1, "only the valid cue loads");
    assert_eq!(ws.cues_skipped_on_load, 1, "the dropped cue must be counted");
}

#[test]
fn clean_load_reports_zero_skips() {
    let registry = full_registry();
    let doc = workspace_doc(1, vec![
        valid_cue(CueType::Memo, "A"),
        valid_cue(CueType::Audio, "B"),
    ]);
    let ws = Workspace::from_json_str(&doc, None, &registry).expect("loads");
    assert_eq!(ws.cues_skipped_on_load, 0, "a fully valid file skips nothing");
}

#[test]
fn load_skips_malformed_group_child_keeps_group() {
    let registry = full_registry();
    // Build a group that serialises with one valid + one bogus child.
    let mut group_json = valid_cue(CueType::Group, "Act 1");
    group_json["children"] = serde_json::json!([
        valid_cue(CueType::Memo, "Cue in group"),
        { "type": "quantum", "id": "44444444-4444-4444-8444-444444444444" }
    ]);
    let doc = workspace_doc(1, vec![group_json]);

    let ws = Workspace::from_json_str(&doc, None, &registry).expect("loads");
    let list = ws.active_cue_list().unwrap();
    assert_eq!(list.cues.len(), 1, "the group itself must load");
    let group = &list.cues[0];
    let children = group.child_cues().expect("group exposes its children");
    assert_eq!(children.len(), 1, "the bogus child is skipped, the valid one kept");
    assert_eq!(ws.cues_skipped_on_load, 1, "the dropped group child must be counted too");
}

#[test]
fn load_rejects_future_schema_version() {
    let registry = full_registry();
    let doc = workspace_doc(999, vec![valid_cue(CueType::Memo, "x")]);
    // Workspace has no Debug impl, so extract the error via match rather than expect_err.
    let msg = match Workspace::from_json_str(&doc, None, &registry) {
        Ok(_) => panic!("a newer-schema file must be rejected, not silently mis-parsed"),
        Err(e) => e.to_string(),
    };
    assert!(
        msg.contains("newer version") || msg.contains("schema"),
        "error should explain the schema mismatch, got: {msg}"
    );
}

#[test]
fn load_rejects_invalid_json() {
    let registry = full_registry();
    let msg = match Workspace::from_json_str("{ this is not valid json ", None, &registry) {
        Ok(_) => panic!("invalid JSON must be a clean error, not a silent Ok"),
        Err(e) => e.to_string(),
    };
    assert!(!msg.is_empty(), "error must carry a message");
}

#[test]
fn load_missing_workspace_key_errors() {
    let registry = full_registry();
    let doc = serde_json::json!({ "schema_version": 1, "cue_lists": [] }).to_string();
    assert!(
        Workspace::from_json_str(&doc, None, &registry).is_err(),
        "a document without the `workspace` metadata key must error"
    );
}

#[test]
fn load_tolerates_wrong_field_types_via_defaults() {
    // Lenient field parsing: an audio cue whose `volume_db` is the wrong JSON
    // type falls back to its default rather than failing the whole cue. This
    // is deliberate resilience — the cue still loads and plays.
    let registry = full_registry();
    let mut audio = valid_cue(CueType::Audio, "Music");
    audio["volume_db"] = serde_json::json!("this should be a number");
    let doc = workspace_doc(1, vec![audio]);

    let ws = Workspace::from_json_str(&doc, None, &registry).expect("loads with defaults");
    assert_eq!(ws.active_cue_list().unwrap().cues.len(), 1);
}

#[test]
fn relative_media_paths_are_absolutised_on_load() {
    // The .inkue stores paths relative to the workspace dir; load must resolve
    // them against `base_dir` so a broken/relative path never reaches playback.
    let registry = full_registry();
    let base = temp_dir("ws_paths");

    let mut audio = valid_cue(CueType::Audio, "Track");
    audio["file_path"] = serde_json::json!("audio/track.wav");
    let doc = workspace_doc(1, vec![audio]);

    let ws = Workspace::from_json_str(&doc, Some(base.as_path()), &registry).expect("loads");
    let cue = &ws.active_cue_list().unwrap().cues[0];
    let resolved = cue.media_file_path().expect("audio cue exposes its media path");
    assert!(
        resolved.is_absolute(),
        "relative path must be absolutised on load, got {}",
        resolved.display()
    );
    assert!(
        resolved.ends_with("track.wav"),
        "resolved path should still point at the media file, got {}",
        resolved.display()
    );
}

// ---------------------------------------------------------------------------
// Duplicate cue ids (legacy copy/paste of a Group kept its children's ids)
// ---------------------------------------------------------------------------

fn group_with_child(group_id: &str, child_id: &str) -> serde_json::Value {
    let mut child = valid_cue(CueType::Memo, "Child");
    child["id"] = serde_json::json!(child_id);
    let mut group = valid_cue(CueType::Group, "Group");
    group["id"] = serde_json::json!(group_id);
    group["children"] = serde_json::json!([child]);
    group
}

#[test]
fn load_gives_fresh_ids_to_cues_that_share_an_id() {
    let registry = full_registry();
    let shared_child = "33333333-3333-4333-8333-333333333333";
    let cues = vec![
        group_with_child("44444444-4444-4444-8444-444444444444", shared_child),
        group_with_child("55555555-5555-4555-8555-555555555555", shared_child),
    ];
    let doc = workspace_doc(1, cues);

    let ws = Workspace::from_json_str(&doc, None, &registry).expect("load");
    let list = ws.active_cue_list().unwrap();
    let first_child = list.cues[0].child_cues().unwrap()[0].id();
    let second_child = list.cues[1].child_cues().unwrap()[0].id();

    assert_eq!(first_child.to_string(), shared_child, "first occurrence keeps its id");
    assert_ne!(first_child, second_child, "the later copy must get its own id");
    assert!(list.get_recursive(&second_child).is_some());
}

// ---------------------------------------------------------------------------
// Video outputs
// ---------------------------------------------------------------------------

#[test]
fn extra_video_outputs_survive_save_and_load() {
    use inkue_lib::engine::output_engine::OutputConfig;

    let regions = serde_json::json!([
        {"source": [5000, 0, 5000, 10000], "destination": [0, 0, 5000, 10000]}
    ]);
    let registry = full_registry();
    let dir = temp_dir("ws_outputs");
    let path = dir.join("show.inkue");

    let mut ws = Workspace::new("Two projectors");
    let mut facade = OutputConfig::new("Façade");
    facade.screen = Some(1);
    facade.transform.pan_x = 0.05;
    facade.transform.regions = serde_json::from_value(regions.clone()).unwrap();
    let retour = OutputConfig::new("Retour");
    ws.video_outputs = vec![facade.clone(), retour.clone()];
    ws.preferences.display.output_transform.regions = serde_json::from_value(regions).unwrap();
    ws.save(Some(path.clone())).expect("save");

    let loaded = Workspace::load(path, &registry).expect("load");
    assert_eq!(loaded.video_outputs, vec![facade, retour]);
    assert_eq!(
        loaded.preferences.display.output_transform.regions,
        ws.preferences.display.output_transform.regions,
    );
}

#[test]
fn invalid_stage_regions_in_main_or_extra_output_fail_workspace_load() {
    use serde_json::json;

    let registry = full_registry();
    let base: serde_json::Value = serde_json::from_str(&workspace_doc(1, vec![])).unwrap();
    let invalid_rect = json!({"source": [5000, 0, 5001, 10000], "destination": [0, 0, 5000, 10000]});
    for invalid in [json!(null), json!([invalid_rect])] {
        let mut main = base.clone();
        main["preferences"] = json!({"display": {"output_transform": {"regions": invalid}}});
        let error = Workspace::from_json_str(&main.to_string(), None, &registry)
            .err().expect("invalid regions on the main output must stop loading");
        assert!(error.to_string().contains("Invalid stage regions for the main output"));

        let mut extra = base.clone();
        extra["video_outputs"] = json!([
            {"id": "22222222-2222-4222-8222-222222222222", "name": "Écran", "screen": 1},
            {"id": "33333333-3333-4333-8333-333333333333", "name": "Rear", "screen": 2,
             "transform": {"regions": invalid}}
        ]);
        let error = Workspace::from_json_str(&extra.to_string(), None, &registry)
            .err().expect("invalid regions on an extra output must stop loading");
        assert!(error.to_string().contains("Invalid stage regions for extra output #2 (Rear)"));
    }
}

#[test]
fn legacy_non_region_preferences_and_output_errors_retain_their_defaults() {
    use serde_json::json;

    let registry = full_registry();
    let mut legacy: serde_json::Value = serde_json::from_str(&workspace_doc(1, vec![])).unwrap();
    legacy["preferences"] = json!({"display": {"output_screen": "none"}});
    legacy["video_outputs"] = json!([{"id": "bad-id", "name": "Legacy output", "screen": 1}]);
    let workspace = Workspace::from_json_str(&legacy.to_string(), None, &registry).unwrap();
    assert!(workspace.video_outputs.is_empty());
    assert_eq!(workspace.preferences.display.output_screen, None);
}

#[test]
fn malformed_audio_effects_cannot_drop_a_required_patch_on_reopen() {
    use serde_json::{json, Value};

    let registry = full_registry();
    let mut show: Value = serde_json::from_str(&workspace_doc(1, vec![])).unwrap();
    show["output_patches"] = json!([
        {"id": uuid::Uuid::new_v4(), "name": "Legacy", "device_id": "default", "channels": [0, 1]},
        {"id": uuid::Uuid::new_v4(), "name": "Exact", "device_id": "hw:0", "channels": [0, 1],
         "require_named_device": true, "audio_effects": [{"kind": "unknown_effect"}]},
    ]);
    let Err(error) = Workspace::from_json_str(&show.to_string(), None, &registry) else {
        panic!("loading must not discard the required patch and permit ordinary routing");
    };
    assert!(error.to_string().contains("Invalid workspace audio output patches"));
    show["output_patches"] = Value::Null;
    assert!(Workspace::from_json_str(&show.to_string(), None, &registry).unwrap().output_patches.is_empty());
}

#[test]
fn a_workspace_written_before_outputs_existed_has_none() {
    let registry = full_registry();
    let doc = workspace_doc(1, vec![valid_cue(CueType::Memo, "Note")]);
    let ws = Workspace::from_json_str(&doc, None, &registry).expect("load");
    assert!(ws.video_outputs.is_empty());
}

#[test]
fn a_visual_cue_remembers_its_output() {
    let registry = full_registry();
    let output = uuid::Uuid::new_v4();
    for cue_type in [CueType::Video, CueType::Image, CueType::Camera, CueType::Text] {
        let mut json = valid_cue(cue_type.clone(), "Visual");
        json["output_id"] = serde_json::json!(output.to_string());
        let cue = registry.from_json(json).expect("rebuild");
        assert_eq!(
            cue.serialize()["output_id"],
            output.to_string(),
            "{cue_type:?} must keep its output across a rebuild",
        );
    }
}

#[test]
fn a_visual_cue_without_output_plays_on_the_main_one() {
    let registry = full_registry();
    for cue_type in [CueType::Video, CueType::Image, CueType::Camera, CueType::Text] {
        let json = valid_cue(cue_type.clone(), "Visual");
        assert!(json["output_id"].is_null(), "{cue_type:?} defaults to the main output");
    }
    let _ = registry;
}
