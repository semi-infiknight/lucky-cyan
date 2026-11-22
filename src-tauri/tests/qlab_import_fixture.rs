//! Zone (🟠 import) — a real QLab workspace, imported end to end.
//!
//! Opt-in: the fixture lives with the `qlab2inkue` reference converter, outside
//! this repository.  Run it with
//!
//! ```text
//! INKUE_QLAB_FIXTURE="C:/qlab2inkue/samples/All Cue Types.qlab5" \
//!     cargo test --test qlab_import_fixture -- --ignored
//! ```

mod common;

use std::path::Path;

use inkue_lib::show::Workspace;

fn fixture() -> Option<std::path::PathBuf> {
    std::env::var_os("INKUE_QLAB_FIXTURE").map(Into::into)
}

#[test]
#[ignore = "needs INKUE_QLAB_FIXTURE pointing at a .qlab4 / .qlab5 file"]
fn a_real_workspace_imports_and_loads_every_cue() {
    let path = fixture().expect("set INKUE_QLAB_FIXTURE");
    let (json, report) = inkue_lib::qlab_import::import_workspace(&path).expect("import");
    assert!(report.cue_count > 0, "the fixture holds cues");

    let registry = common::full_registry();
    let workspace = Workspace::from_json_str(&json, path.parent(), &registry).expect("load");
    let loaded: usize = workspace.cue_lists.iter().map(|l| count(&l.cues)).sum();
    assert_eq!(loaded, report.cue_count, "the loader must not skip an imported cue");
}

#[test]
#[ignore = "needs INKUE_QLAB_FIXTURE pointing at a .qlab4 / .qlab5 file"]
fn every_picture_plays_on_a_declared_output() {
    let path = fixture().expect("set INKUE_QLAB_FIXTURE");
    let (json, report) = inkue_lib::qlab_import::import_workspace(Path::new(&path)).expect("import");
    let document: serde_json::Value = serde_json::from_str(&json).unwrap();
    let declared: Vec<String> = document["video_outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(declared.len(), report.video_outputs.len());

    let mut pictures = 0;
    walk(&document["cue_lists"], &mut |cue| {
        if matches!(cue["cue_type"].as_str(), Some("video" | "image" | "camera" | "text")) {
            pictures += 1;
            match cue["output_id"].as_str() {
                None => {}
                Some(id) => assert!(declared.contains(&id.to_string()), "cue on an undeclared output {id}"),
            }
        }
    });
    assert!(pictures > 0, "the fixture holds visual cues");
}

/// Impossible Wall Portal.qlab5, SHA-256 a147920332f257e1eb72f93d869bf54bf23b0e7ed3bf27204b45c2f612075ac3.
#[test]
#[ignore = "needs INKUE_QLAB_FIXTURE pointing at the Impossible Wall Portal QLab 5 workspace"]
fn portal_import_retains_its_three_continuous_video_loops() {
    let path = fixture().expect("set INKUE_QLAB_FIXTURE");
    let (json, _) = inkue_lib::qlab_import::import_workspace(&path).expect("import");
    let document: serde_json::Value = serde_json::from_str(&json).unwrap();
    let workspace = Workspace::from_json_str(&json, path.parent(), &common::full_registry()).expect("load");
    let expected = [("00.1", 0), ("60.1", u32::MAX), ("70.1", u32::MAX), ("80.1", u32::MAX)];
    let mut found = std::collections::BTreeSet::new();
    walk(&document["cue_lists"], &mut |cue| {
        let Some(number) = cue["number"].as_str() else { return };
        let Some((_, repeats)) = expected.iter().find(|(key, _)| number == *key) else { return };
        assert_eq!(cue["cue_type"], "video");
        let id = cue["id"].as_str().unwrap().parse().unwrap();
        let runtime_cue = workspace.cue_lists.iter().find_map(|list| list.get_recursive(&id)).unwrap();
        assert_eq!(runtime_cue.serialize()["loop_count"].as_u64(), Some(u64::from(*repeats)), "{number}");
        assert!(found.insert(number.to_owned()), "duplicate cue {number}");
    });
    assert_eq!(found.len(), expected.len());
}

fn count(cues: &[Box<dyn inkue_lib::cue::traits::Cue>]) -> usize {
    cues.iter().map(|c| 1 + c.child_cues().map(count).unwrap_or(0)).sum()
}

fn walk(value: &serde_json::Value, visit: &mut dyn FnMut(&serde_json::Value)) {
    match value {
        serde_json::Value::Array(items) => items.iter().for_each(|v| walk(v, visit)),
        serde_json::Value::Object(map) => {
            if map.contains_key("cue_type") {
                visit(value);
            }
            for key in ["cues", "children"] {
                if let Some(nested) = map.get(key) {
                    walk(nested, visit);
                }
            }
        }
        _ => {}
    }
}

/// Real QTableTop.qlab4, SHA-256 b2ba2eefcac198e9eb5cd889d573eebc4f35dafc72245f1bdc4fd013540c7dae.
/// The directional cues distinguish row × column from a transposed square grid.
#[test]
#[ignore = "needs INKUE_QTABLETOP_FIXTURE pointing at the QTableTop QLab 4 workspace"]
fn qtabletop_import_keeps_directional_pad_positions_and_the_empty_cell() {
    let path = std::env::var_os("INKUE_QTABLETOP_FIXTURE").expect("set INKUE_QTABLETOP_FIXTURE");
    let path = Path::new(&path);
    let (json, report) = inkue_lib::qlab_import::import_workspace(path).unwrap();
    let ws = Workspace::from_json_str(&json, path.parent(), &common::full_registry()).unwrap();
    let cart = ws.cue_lists.iter().find(|list| list.name == "QTableTop - Cart").unwrap();
    let layout = cart.cart_layout.as_ref().unwrap();
    assert_eq!((layout.rows, layout.columns), (8, 8));
    assert_eq!(layout.pads.values().flatten().count(), 63);
    for (number, row, column) in [("UP", 6, 7), ("RIGHT", 7, 8)] {
        let cue = cart.cues.iter().find(|cue| cue.number() == Some(number)).unwrap();
        let cell = layout.pads[&cue.id()].unwrap();
        assert_eq!((cell.row, cell.column), (row, column));
    }
    assert!(!layout.pads.values().flatten().any(|c| (c.row, c.column) == (1, 3)));
    assert!(report.cart_layout_notes.is_empty());
    assert!(ws.universe_outputs.is_empty());
    assert_eq!(ws.fixtures.len(), 1);
    let light = &ws.fixtures[0];
    assert_eq!(light.label, "SixPar");
    assert_eq!(light.address_span(), Some((1, 6)));
    assert_eq!(light.fixture_type.manufacturer, "Elation");
    assert!(report.lighting_patch_notes.iter().any(|note| note.contains("offline universe 1")));
}

/// QTableTop saves several Full Surface checkboxes as plist integers.
#[test]
#[ignore = "needs INKUE_QTABLETOP_FIXTURE pointing to QTableTop.qlab4"]
fn qtabletop_keeps_the_archived_full_surface_stretch() {
    let path = std::env::var_os("INKUE_QTABLETOP_FIXTURE").expect("set INKUE_QTABLETOP_FIXTURE");
    let path = Path::new(&path);
    let (json, _) = inkue_lib::qlab_import::import_workspace(path).unwrap();
    let show: serde_json::Value = serde_json::from_str(&json).unwrap();
    let mut stretches = 0;
    walk(&show["cue_lists"], &mut |cue| {
        if cue["cue_type"] == "video" && cue["geometry"]["fit_mode"] == "stretch" {
            stretches += 1;
        }
    });
    assert_eq!(stretches, 7);

    let workspace = Workspace::from_json_str(&json, path.parent(), &common::full_registry()).unwrap();
    let picture = |id: &str| {
        let id = uuid::Uuid::parse_str(id).unwrap();
        workspace.cue_lists.iter().find_map(|list| list.get_recursive(&id)).unwrap().serialize()
    };
    assert_eq!(picture("1614F52A-04A1-426B-BC30-78508F461A5A")["geometry"]["fit_mode"], "stretch");
    assert_eq!(picture("92F5F66F-4A09-4BBD-B22C-639859C4C18F")["geometry"]["fit_mode"], "fit");
}

/// Same real QTableTop archive as the cart test above.
#[test]
#[ignore = "needs INKUE_QTABLETOP_FIXTURE pointing to QTableTop.qlab4"]
fn qtabletop_reports_its_archived_audio_master_trim_without_rewriting_playback() {
    let path = std::env::var_os("INKUE_QTABLETOP_FIXTURE").expect("set INKUE_QTABLETOP_FIXTURE");
    let (json, report) = inkue_lib::qlab_import::import_workspace(Path::new(&path)).unwrap();
    let trimmed = report.cues.iter().filter(|cue| cue.qlab_class == "AudioCue"
        && cue.note.as_deref().is_some_and(|note| note.contains("Review QLab main trim")));
    assert_eq!(trimmed.count(), 51);

    let show: serde_json::Value = serde_json::from_str(&json).unwrap();
    let mut verified_pair = false;
    walk(&show["cue_lists"], &mut |cue| {
        if cue["notes"].as_str().is_some_and(|note| {
            note.contains("main level -12.00 dB and main trim -5.00 dB")
        }) {
            assert!((cue["volume_db"].as_f64().unwrap() + 17.0).abs() < 1e-5);
            assert!(cue.get("trim_db").is_none());
            verified_pair = true;
        }
    });
    assert!(verified_pair);
}
