//! Import a QLab workspace (`.qlab4`, `.qlab5`, or a verified legacy `.cues`) as an Inkue workspace.
//!
//! Reading someone else's file format to import work the **user already owns**
//! is interoperability, not appropriation: no QLab code is involved, and the
//! container is Apple's `NSKeyedArchiver`, not a Figure 53 format. QLab is a
//! trademark of Figure 53, LLC; Inkue is not affiliated with or endorsed by
//! them.
//!
//! Three layers:
//!
//! - [`archive`] resolves the NSKeyedArchiver object graph (the only genuinely
//!   novel code — everything else is mapping).
//! - [`patches`] reads the workspace's destination tables.
//! - [`outputs`] turns QLab's video stages / surfaces into Inkue outputs.
//! - [`cues`] maps one QLab cue to one Inkue cue.
//!
//! The reference implementation is the standalone Python converter in
//! `qlab2inkue/`, which can decode an unknown `.qlab5` offline and whose
//! `test_mapping.py` runs against a fixture holding one cue of every QLab 5
//! type. Decode a real file there before assuming a property name.
//!
//! **Media paths.** The document is emitted with bundle-relative paths, and
//! the caller loads it with the bundle folder as the base — so an imported
//! show plays immediately, without writing anything into the user's QLab
//! project. `Collect and Save` makes it self-contained afterwards.

pub mod archive;
mod audio_matrix;
mod cart;
mod slices;
pub mod cues;
mod lighting;
mod legacy_v3;
mod fixture_patch;
pub mod outputs;
mod visual;
pub mod patches;

use std::{collections::BTreeMap, path::Path};

use anyhow::{anyhow, Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::cue::types::CueColor;

use outputs::VideoOutputs;
use patches::Patches;

/// What one QLab cue became, for the post-import report.
#[derive(Debug, Clone, Serialize)]
pub struct ImportedCue {
    /// QLab class name, e.g. `"AudioCue"`.
    pub qlab_class: String,
    pub cue_number: Option<String>,
    pub cue_name: String,
    /// Inkue cue type it became.
    pub inkue_type: String,
    /// Set when the cue could not be represented faithfully.
    pub note: Option<String>,
}

/// The outcome of an import, for the UI to summarise.
#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub workspace_name: String,
    pub cue_count: usize,
    pub cue_list_count: usize,
    /// Cues the operator has to look at: Memo placeholders for types with no
    /// Inkue counterpart, plus ones imported deliberately incomplete (a Script
    /// cue arrives disarmed). Not the same as "failed" — nothing is lost.
    pub needs_attention: usize,
    pub media_found: usize,
    pub media_missing: Vec<String>,
    pub cart_layout_notes: Vec<String>,
    pub lighting_patch_notes: Vec<String>,
    /// Migration checks whose scope is the workspace, not a single cue.
    pub audio_map_notes: Vec<String>,
    /// Extra video outputs created from QLab's stages / surfaces — they need a
    /// screen before their cues can play anywhere but a floating window.
    pub video_outputs: Vec<String>,
    pub cues: Vec<ImportedCue>,
}

/// QLab class → Inkue control cue type. Inkue's `resume` has no QLab
/// counterpart. Target cues also carry a replacement number.
const CONTROL_CLASSES: [(&str, &str); 6] = [
    ("StartCue", "start"),
    ("PauseCue", "pause"),
    ("ResetCue", "reset"),
    ("GotoCue", "goto"),
    ("ArmCue", "arm"),
    ("DisarmCue", "disarm"),
];

/// Decode `path` and build an Inkue workspace document plus a report.
///
/// Returns `(workspace_json, report)`. The JSON carries bundle-relative media
/// paths; load it with the file's parent directory as the base.
pub fn import_workspace(path: &Path) -> Result<(String, ImportReport)> {
    let bytes = std::fs::read(path)
        .with_context(|| format!("cannot read {}", path.display()))?;
    let outer = archive::resolve(&bytes)?;

    let workspace_name = outer
        .get("workspaceName")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("Imported QLab Show")
        .to_string();
    let legacy_v3 = legacy_v3::applies(path, &outer)?;
    let settings = if legacy_v3 {
        json!({})
    } else {
        outer.get("settings").cloned().unwrap_or(json!({}))
    };
    let patches = Patches::from_settings(&settings);
    let lighting_patch = fixture_patch::from_settings(&settings);
    let video_outputs = VideoOutputs::from_settings(&settings);

    // The cue tree is a second archive nested inside the first.
    let nested = outer
        .get("cueLists")
        .and_then(archive::nested_data)
        .ok_or_else(|| anyhow!("no cue lists in this workspace"))?;
    let mut root = archive::resolve(&nested)?;
    if legacy_v3 {
        legacy_v3::prepare(&outer, &mut root)?;
    }

    // Pre-pass: learn each cue's starting pan from a following pan fade.
    let mut pan_starts = std::collections::HashMap::new();
    if !legacy_v3 {
        cues::collect_pan_starts(&root, &mut pan_starts);
    }

    let modern_group_modes = !legacy_v3 && path.extension().and_then(|ext| ext.to_str())
        .is_some_and(|ext| ext.eq_ignore_ascii_case("qlab4") || ext.eq_ignore_ascii_case("qlab5"));
    let mut ctx = MapContext {
        legacy_v3, modern_group_modes, patches: &patches,
        outputs: &video_outputs, pan_starts: &pan_starts,
    };
    let mut report_cues = Vec::new();
    let mut cue_lists = Vec::new();
    for list in root.get("cues").and_then(Value::as_array).unwrap_or(&Vec::new()) {
        cue_lists.push(map_cue_list(list, &mut ctx, &mut report_cues));
    }
    if cue_lists.is_empty() {
        cue_lists.push(json!({
            "id": uuid::Uuid::new_v4().to_string(),
            "name": "Cue List 1",
            "mode": "sequential",
            "playhead_cue_id": Value::Null,
            "cues": [],
        }));
    }

    let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let document = json!({
        "schema_version": 1,
        "workspace": { "name": workspace_name, "created_at": now, "modified_at": now },
        "output_patches": patches.audio, "default_output_patch": Value::Null,
        "osc_patches": patches.osc,
        "video_outputs": video_outputs.extras(),
        "input_patches": [], "universe_outputs": [],
        "fixtures": lighting_patch.fixtures, "fixture_groups": [],
        "cue_lists": cue_lists,
        "active_cue_list_id": cue_lists[0]["id"],
    });

    let base_dir = path.parent().unwrap_or(Path::new("."));
    let (media_found, media_missing) = count_media(&document, base_dir);
    let needs_attention = report_cues.iter().filter(|c| c.note.is_some()).count();

    let report = ImportReport {
        workspace_name: document["workspace"]["name"].as_str().unwrap_or("").to_string(),
        cue_count: report_cues.len(),
        cue_list_count: cue_lists.len(),
        needs_attention,
        media_found,
        lighting_patch_notes: lighting_patch.notes,
        audio_map_notes: object_audio_review(path),
        media_missing,
        cart_layout_notes: cue_lists.iter().filter(|list| list["mode"] == "cart").filter_map(|list| {
            let name = list["name"].as_str().unwrap_or("Cart");
            let Some(pads) = list["cart_layout"]["pads"].as_object() else {
                return Some(format!("{name}: source grid unavailable. Check positions in Edit pads."));
            };
            let unplaced = pads.values().filter(|cell| cell.is_null()).count();
            (unplaced > 0).then(|| format!("{name}: {unplaced} pad(s) have no confirmed cell. Inspect Outside grid in Edit pads."))
        }).collect(),
        video_outputs: video_outputs.names(),
        cues: report_cues,
    };
    Ok((serde_json::to_string(&document)?, report))
}

fn object_audio_review(path: &Path) -> Vec<String> {
    if !path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("qlab5")) {
        return Vec::new();
    }
    vec!["If the source show uses object audio, review those cues before the first GO. The importer converts supported channel routes. Recreate stationary mixes with Inkue audio maps, and rehearse cues that depend on QLab maps, objects or motion separately.".into()]
}

/// Everything the per-cue mapping needs beyond the cue itself.
struct MapContext<'a> {
    legacy_v3: bool,
    modern_group_modes: bool,
    patches: &'a Patches,
    /// QLab stage / surface → Inkue output.
    outputs: &'a VideoOutputs,
    /// Target cue id → the pan a following pan fade starts from.
    pan_starts: &'a std::collections::HashMap<String, f64>,
}

#[derive(Default)]
struct CueMetadata {
    flagged_cues: Vec<Value>,
    second_colors: BTreeMap<String, CueColor>,
}

fn map_cue_list(list: &Value, ctx: &mut MapContext, report: &mut Vec<ImportedCue>) -> Value {
    let mut metadata = CueMetadata::default();

    // QLab cue *carts* are a distinct kind of list and Inkue has the same
    // concept — flattening them to sequential would change how the show runs.
    // QLab 4/5 stores a cart as a GroupCue with groupMode 5. Its "cart"
    // dictionary holds cell assignments and is present even in sequential lists.
    let is_cart = list["groupMode"].as_i64() == Some(5) || list["cart"].as_bool() == Some(true);
    let cues: Vec<Value> = list.get("cues").and_then(Value::as_array).map(|items| {
        items.iter().map(|cue| map_cue(cue, ctx, report, &mut metadata, !is_cart)).collect()
    }).unwrap_or_default();
    let playhead = if is_cart {
        Value::Null
    } else {
        cues.first().map(|c| c["id"].clone()).unwrap_or(Value::Null)
    };

    json!({
        "id": list.get("uniqueID").and_then(Value::as_str)
            .and_then(|s| uuid::Uuid::parse_str(s).ok())
            .unwrap_or_else(uuid::Uuid::new_v4).to_string(),
        "name": list.get("name").and_then(Value::as_str).unwrap_or("Cue List"),
        "number": list["number"].as_str().filter(|number| !number.trim().is_empty()),
        "mode": if is_cart { "cart" } else { "sequential" },
        "chain_cart_cues": false,
        "cart_layout": is_cart.then(|| cart::import_layout(list, &cues)).flatten(),
        "playhead_cue_id": playhead,
        "flagged_cues": metadata.flagged_cues,
        "second_colors": metadata.second_colors,
        "cues": cues,
    })
}

/// Baseline group conversion. The scoped QLab 4/5 mappings for Start First
/// And Enter (1) and Start First (2) are checked in `map_cue`. Timeline (3)
/// uses child Pre-Waits, 4 is Start Random, and 6 is Playlist.
fn group_mode(code: i64) -> (&'static str, bool) {
    match code {
        3 => ("simultaneous", false),
        4 => ("start_random", false),
        6 => ("playlist", true),
        _ => ("sequential", false),
    }
}

fn map_cue(
    cue: &Value,
    ctx: &mut MapContext,
    report: &mut Vec<ImportedCue>,
    metadata: &mut CueMetadata,
    in_main_list: bool,
) -> Value {
    let class = cue.get("__class__").and_then(Value::as_str).unwrap_or("?").to_string();
    let name = cue.get("name").and_then(Value::as_str).unwrap_or("").to_string();
    let number = cue.get("number").and_then(Value::as_str).filter(|s| !s.is_empty())
        .map(str::to_string);

    let mut note: Option<String> = None;
    let mapped = match class.as_str() {
        other if ctx.legacy_v3 && !legacy_v3::verified_class(other) =>
            cues::unconvertible(cue, &format!("Unconverted QLab 3 {other}"), ""),
        "AudioCue" => cues::audio(cue, ctx.pan_starts),
        "VideoCue" => cues::video(cue),
        // A camera cue has no file. One that carries a media target anyway
        // (seen in older workspaces) is really a video cue.
        "CameraCue" => {
            if cues::media_path(cue).is_some() {
                cues::video(cue)
            } else {
                cues::camera(cue, ctx.patches)
            }
        }
        "GroupCue" => {
            let source_children = cue["cues"].as_array().map(Vec::as_slice).unwrap_or_default();
            let mut children: Vec<Value> = source_children.iter()
                .map(|child| map_cue(child, ctx, report, metadata, false))
                .collect();
            let source_mode = cue["groupMode"].as_i64().unwrap_or(0);
            let (baseline_mode, mut loops) = group_mode(source_mode);
            let direct_manual_group = in_main_list && cue["continueMode"].as_i64() == Some(0)
                && !source_children.is_empty() && source_children.iter().all(|child| {
                    matches!(child["__class__"].as_str(), Some("AudioCue" | "VideoCue" | "MemoCue" | "WaitCue"))
                        && matches!(child["continueMode"].as_i64(), Some(0..=2))
                });
            let source_tail = source_children.iter().rfind(|child| child["armed"].as_bool() != Some(false));
            let manual_boundary = source_tail.is_none_or(|child| child["continueMode"].as_i64() == Some(0))
                && children.iter().rfind(|child| child["is_disabled"].as_bool() != Some(true))
                    .is_none_or(|child| child["continue_mode"].as_str() == Some("do_not_continue"));
            // Conversion can disarm a child and change either the first
            // eligible cue or which continuation reaches the handoff.
            let same_entrances = source_children.iter().zip(&children).all(|(source, child)| {
                child["is_disabled"].as_bool() == Some(source["armed"].as_bool() == Some(false))
                    && (source["__class__"] != "MemoCue" || child["pre_wait_ms"] == 0)
            });
            let qualified = direct_manual_group && manual_boundary && source_tail.is_some() && same_entrances;
            let (mode, warning) = match (ctx.modern_group_modes, source_mode) {
                (true, 1) => ("start_first_and_enter", (!qualified).then_some(
                    "Review QLab Start First And Enter before arming this group. Check direct child types, arming and timing, and continuation after the last armed child.",
                )),
                (true, 2) => ("start_first", (!qualified).then_some(
                    "Review QLab Start First before arming this group. Check its location, direct child types, arming and timing, and the group's and final child's Continue settings.",
                )),
                (true, 6) => {
                    let source_loop = archive::checkbox(&cue["playlistLoop"]);
                    loops = source_loop.unwrap_or(false);
                    let plain_settings = source_loop.is_some()
                        && ["playlistCrossfade", "playlistShuffle", "secondTriggerOnRelease"].iter()
                            .all(|key| archive::checkbox(&cue[*key]) == Some(false))
                        && cue["secondTriggerAction"].as_i64() == Some(0);
                    let qualified = direct_manual_group && source_tail.is_some() && same_entrances && plain_settings;
                    if qualified {
                        // QLab includes the child's duration as its Playlist
                        // Post-Wait. Inkue already waits for natural completion.
                        for child in &mut children {
                            child["post_wait_ms"] = json!(0);
                        }
                    }
                    ("playlist", (!qualified).then_some(
                        "Review QLab Playlist before arming. Compare loop, crossfade, shuffle, second-trigger behavior and each child's timing against the source.",
                    ))
                }
                _ => (baseline_mode, None),
            };
            let mut group = match cues::memo(cue, "") {
                Value::Object(map) => map,
                _ => unreachable!(),
            };
            group.insert("type".into(), json!("group"));
            group.insert("cue_type".into(), json!("group"));
            group.remove("memo_text");
            group.insert("group_mode".into(), json!(mode));
            if let Some(warning) = warning {
                group.insert("is_disabled".into(), json!(true));
                cues::add_note(&mut group, warning);
                note = Some(warning.into());
            }
            group.insert("playlist_loop".into(), json!(loops));
            if ctx.modern_group_modes && source_mode == 6 {
                group.insert("playlist_advance_playhead".into(), json!(true));
            }
            if mode == "start_random" && ctx.modern_group_modes {
                group.insert("random_advance_playhead".into(), json!(true));
            }
            group.insert("children".into(), json!(children));
            Value::Object(group)
        }
        "MemoCue" => cues::memo(cue, &name),
        "TitlesCue" => cues::titles(cue),
        "MicCue" => cues::mic(cue),
        "OSCCue" => cues::osc(cue, ctx.patches),
        "MIDICue" => cues::midi(cue, ctx.patches),
        "MIDIFileCue" => cues::midi_file(cue, ctx.patches),
        "TimecodeCue" => cues::timecode(cue, ctx.patches),
        "FadeCue" => cues::fade(cue),
        "StopCue" => cues::stop(cue),
        "DevampCue" => cues::devamp(cue),
        "WaitCue" => cues::wait(cue),
        "ScriptCue" => {
            note = Some("AppleScript kept in the notes; the cue is disarmed".into());
            cues::script(cue)
        }
        "LightCue" => {
            let (light, warning) = lighting::light(cue);
            note = Some(warning);
            light
        }
        "LoadCue" => {
            let mut load = cues::control(cue, "load");
            let source = &cue["loadTime"];
            if !source.is_null() && source.as_f64() != Some(0.0) {
                let ms = source.as_f64().map(|seconds| (seconds * 1000.0).round())
                    .filter(|ms| (i64::MIN as f64..i64::MAX as f64).contains(ms));
                if let Some(ms) = ms {
                    load["load_time_ms"] = json!(ms as i64);
                }
                load["is_disabled"] = json!(true);
                let warning = format!("Review the QLab Load Time of {source} seconds and its target before arming this Load cue.");
                cues::add_note(load.as_object_mut().expect("control conversion produces an object"), &warning);
                note = Some(warning);
            }
            load
        }
        "TargetCue" => {
            let mut target = cues::control(cue, "target");
            target["replacement_number"] = cue.get("targetCueNumber").cloned().unwrap_or(Value::Null);
            note = Some("Check the numbered replacement and the cue to retarget in the same list".into());
            target
        }
        other => {
            if let Some((_, cue_type)) = CONTROL_CLASSES.iter().find(|(c, _)| *c == other) {
                cues::control(cue, cue_type)
            } else {
                note = Some("no Inkue equivalent".into());
                cues::unconvertible(cue, &format!("Unconverted QLab {other}"), "")
            }
        }
    };

    let mut mapped = mapped;
    if class == "AudioCue" && !ctx.legacy_v3 {
        if let (Some(fields), Some(review)) = (mapped.as_object_mut(), cues::master_trim_review(cue)) {
            cues::add_note(fields, &review);
        }
    }
    if matches!(mapped["cue_type"].as_str(), Some("audio" | "video")) {
        if let Some(fields) = mapped.as_object_mut() {
            slices::apply(cue, fields);
        }
    }
    if matches!(mapped["cue_type"].as_str(), Some("audio" | "video" | "mic")) {
        let source = cue.get("audioCue").filter(|v| v.is_object()).unwrap_or(cue);
        if let Some(fields) = mapped.as_object_mut() {
            ctx.patches.assign_audio(source, fields);
        }
    }
    // Pictures keep the stage QLab gave them.
    if matches!(mapped["cue_type"].as_str(), Some("video" | "image" | "camera" | "text")) {
        if let Some(fields) = mapped.as_object_mut() {
            visual::apply(cue, fields, ctx.outputs);
        }
    }

    if ctx.legacy_v3 {
        let mut warnings = Vec::new();
        if let Some(warning) = legacy_v3::review(&class, cue) {
            warnings.push(warning);
        }
        if ["useHotKey", "useMIDITrigger", "useTimecode", "useWallClock"].iter()
            .any(|key| cue[*key] == true) {
            warnings.push("Reassign this QLab 3 cue’s enabled external or keyboard trigger.");
        }
        if !matches!(cue["continueMode"].as_i64(), Some(0..=2)) {
            warnings.push("Check the unverified QLab 3 continue mode before arming.");
        }
        for warning in warnings {
            mapped["is_disabled"] = json!(true);
            cues::add_note(mapped.as_object_mut().unwrap(), warning);
            if let Some(existing) = &mut note {
                existing.push(' ');
                existing.push_str(warning);
            }
        }
    }

    if cue["useSecondColor"].as_bool() == Some(true) {
        if let Some(color) = cues::parse_cue_color(&cue["secondColorName"]) {
            let id = mapped["id"].as_str().expect("mapped cue id");
            metadata.second_colors.insert(id.to_owned(), color);
        } else {
            let warning = match cue["secondColorName"].as_str().filter(|name| !name.trim().is_empty()) {
                Some(name) => format!(
                    "Review the configured QLab second color. Inkue has no equivalent for {:?}.",
                    name.chars().take(80).collect::<String>(),
                ),
                None => "Review the configured QLab second color. The archive has no valid color name.".into(),
            };
            cues::add_note(mapped.as_object_mut().unwrap(), &warning);
            if let Some(note) = &mut note {
                note.push(' ');
                note.push_str(&warning);
            }
        }
    }

    // Mapper-specific losses (for example an unsupported MIDI message or an
    // unpatched mic) must reach the review dialog as well as the cue notes.
    if let Some(detail) = mapped.as_object_mut().and_then(|fields| fields.remove("_conversion_note")) {
        if note.is_none() {
            note = detail.as_str().map(str::to_owned);
        }
    }

    if cue["flagged"].as_bool() == Some(true) {
        metadata.flagged_cues.push(mapped["id"].clone());
    }
    report.push(ImportedCue {
        qlab_class: class,
        cue_number: number,
        cue_name: name,
        inkue_type: mapped["cue_type"].as_str().unwrap_or("?").to_string(),
        note,
    });
    mapped
}

/// How many referenced media files resolve against the bundle folder.
fn count_media(document: &Value, base_dir: &Path) -> (usize, Vec<String>) {
    fn walk(cues: &Value, base_dir: &Path, found: &mut usize, missing: &mut Vec<String>) {
        for cue in cues.as_array().unwrap_or(&Vec::new()) {
            if let Some(path) = cue.get("file_path").and_then(Value::as_str) {
                if !path.is_empty() {
                    if base_dir.join(path).is_file() {
                        *found += 1;
                    } else {
                        missing.push(path.to_string());
                    }
                }
            }
            if let Some(children) = cue.get("children") {
                walk(children, base_dir, found, missing);
            }
        }
    }
    let mut found = 0;
    let mut missing = Vec::new();
    for list in document["cue_lists"].as_array().unwrap_or(&Vec::new()) {
        walk(&list["cues"], base_dir, &mut found, &mut missing);
    }
    (found, missing)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn main_trim_review_reaches_the_import_report_and_saved_audio_cue_notes() {
        let patches = Patches::default();
        let outputs = VideoOutputs::default();
        let pans = std::collections::HashMap::new();
        let mut ctx = MapContext { legacy_v3: false, modern_group_modes: true, patches: &patches, outputs: &outputs, pan_starts: &pans };
        let mut report = Vec::new();
        let source = json!({
            "__class__":"AudioCue", "number":"A4", "notes":"Previous note",
            "levels":{"entries":[{"row":0,"column":0,"initialLevel":0.5,"trimLevel":0.5}]}
        });
        let audio = map_cue(&source, &mut ctx, &mut report, &mut CueMetadata::default(), true);
        assert!((audio["volume_db"].as_f64().unwrap() + 12.041199826).abs() < 1e-8);
        assert_eq!(audio["is_disabled"], false);
        assert!(audio.get("trim_db").is_none());
        assert!(audio.get("_conversion_note").is_none());
        let report_note = report[0].note.as_deref().unwrap();
        assert!(report_note.contains("Review QLab main trim"));
        let saved_note = audio["notes"].as_str().unwrap();
        assert!(saved_note.starts_with("Previous note"));
        assert!(saved_note.contains(report_note));

        let video = map_cue(&json!({"__class__":"VideoCue","audioCue":source}), &mut ctx, &mut report, &mut CueMetadata::default(), true);
        assert_eq!(video["volume_db"], audio["volume_db"]);
        assert!(!video["notes"].as_str().unwrap().contains("Review QLab main trim"));
    }

    #[test]
    fn a_musical_hold_and_its_release_survive_native_reload() {
        use crate::cue::{audio_cue::AudioCueFactory, devamp_cue::DevampCueFactory, traits::CueFactory, video_cue::VideoCueFactory};
        let patches = Patches::default();
        let outputs = VideoOutputs::default();
        let pans = std::collections::HashMap::new();
        let mut ctx = MapContext { legacy_v3: false, modern_group_modes: true, patches: &patches, outputs: &outputs, pan_starts: &pans };
        let mut report = Vec::new();
        let id = uuid::Uuid::new_v4();
        let song = json!({"__class__":"AudioCue","uniqueID":id,"number":"M1",
            "slices":[{"__class__":"AudioSlice","time":1.0,"playCount":2,"infiniteLoop":false},
                      {"__class__":"AudioSlice","time":2.5,"playCount":1,"infiniteLoop":true}],
            "lastSlice":{"__class__":"AudioSlice","time":0,"playCount":3,"infiniteLoop":false}});
        let mapped = map_cue(&song, &mut ctx, &mut report, &mut CueMetadata::default(), true);
        let saved = AudioCueFactory.from_json(mapped).unwrap().serialize();
        assert_eq!(saved["slices"], json!({"markers":[1000,2500],"play_counts":[2,u32::MAX,3]}));
        assert_eq!(saved["is_disabled"], false);
        let mut video = song.clone();
        video["__class__"] = json!("VideoCue");
        video["audioCue"] = json!({"slices":[]});
        let mapped = map_cue(&video, &mut ctx, &mut report, &mut CueMetadata::default(), true);
        let picture = VideoCueFactory.from_json(mapped).unwrap().serialize();
        assert_eq!(picture["slices"], saved["slices"]);
        video["audioCue"]["slices"] = json!([{"time":2.0,"playCount":8}]);
        let mapped = map_cue(&video, &mut ctx, &mut report, &mut CueMetadata::default(), true);
        assert_eq!(mapped["is_disabled"], true);
        assert!(mapped["notes"].as_str().unwrap().contains("different slice programs"));
        let release = json!({"__class__":"DevampCue","number":"M2","devampType":1,
            "cueTargetUniqueID":id,"stopTargetWhenDone":true,"fireNextCueAtMomentLoopEnds":false});
        let mapped = map_cue(&release, &mut ctx, &mut report, &mut CueMetadata::default(), true);
        let saved = DevampCueFactory.from_json(mapped).unwrap().serialize();
        assert_eq!(saved["is_disabled"], false);
        assert_eq!(saved["stop_at_end"], true);
        assert_eq!(saved["target_cue_ids"], json!([id]));

        let mut skipped = song;
        skipped["slices"][0]["playCount"] = json!(0);
        let mapped = map_cue(&skipped, &mut ctx, &mut report, &mut CueMetadata::default(), true);
        let saved = AudioCueFactory.from_json(mapped).unwrap().serialize();
        assert_eq!(saved["is_disabled"], true);
        assert!(saved["notes"].as_str().unwrap().contains("zero-play segment"));
        assert_eq!(report.last().unwrap().cue_number.as_deref(), Some("M1"));
        assert!(report.last().unwrap().note.as_deref().unwrap().contains("2.5"));
        // Whole-cue release (devampType 2) imports armed; with a boundary
        // successor it also waits for the pass end and fires the next cue.
        let mut whole = release.clone();
        whole["devampType"] = json!(2);
        whole["fireNextCueAtMomentLoopEnds"] = json!(false);
        let mapped = map_cue(&whole, &mut ctx, &mut report, &mut CueMetadata::default(), true);
        let saved = DevampCueFactory.from_json(mapped).unwrap().serialize();
        assert_eq!(saved["is_disabled"], false);
        assert_eq!(saved["scope"], json!("whole_cue"));
        assert_eq!(saved["start_next_at_boundary"], json!(false));

        let mut boundary = release.clone();
        boundary["devampType"] = json!(2);
        boundary["fireNextCueAtMomentLoopEnds"] = json!(true);
        let mapped = map_cue(&boundary, &mut ctx, &mut report, &mut CueMetadata::default(), true);
        let saved = DevampCueFactory.from_json(mapped).unwrap().serialize();
        assert_eq!(saved["is_disabled"], false);
        assert_eq!(saved["scope"], json!("whole_cue"));
        assert_eq!(saved["start_next_at_boundary"], json!(true));
        assert_eq!(saved["continue_mode"], json!("auto_follow"));

        // Slice-boundary successor with stop-at-slice-end imports armed.
        let mut slice_next = release.clone();
        slice_next["devampType"] = json!(1);
        slice_next["fireNextCueAtMomentLoopEnds"] = json!(true);
        let mapped = map_cue(&slice_next, &mut ctx, &mut report, &mut CueMetadata::default(), true);
        let saved = DevampCueFactory.from_json(mapped).unwrap().serialize();
        assert_eq!(saved["is_disabled"], false);
        assert_eq!(saved["start_next_at_boundary"], json!(true));
        assert_eq!(saved["continue_mode"], json!("auto_follow"));
        assert_eq!(saved["stop_at_end"], json!(true));

        // Without stop-at-slice-end the boundary successor stays disarmed.
        let mut unsupported = release.clone();
        unsupported["devampType"] = json!(1);
        unsupported["fireNextCueAtMomentLoopEnds"] = json!(true);
        unsupported["stopTargetWhenDone"] = json!(false);
        let mapped = map_cue(&unsupported, &mut ctx, &mut report, &mut CueMetadata::default(), true);
        let saved = DevampCueFactory.from_json(mapped).unwrap().serialize();
        assert_eq!(saved["is_disabled"], true);
        assert!(report.last().unwrap().note.as_deref().unwrap().contains("boundary"));
    }

    #[test]
    fn a_missing_file_is_an_error_not_a_panic() {
        let err = import_workspace(Path::new("no/such/show.qlab5")).unwrap_err();
        assert!(err.to_string().contains("cannot read"));
    }

    #[test]
    fn a_file_that_is_not_a_workspace_is_rejected() {
        let path = std::env::temp_dir().join(format!("{}.qlab5", uuid::Uuid::new_v4()));
        std::fs::write(&path, b"definitely not a plist").unwrap();
        assert!(import_workspace(&path).is_err());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn qlab5_conversion_report_retains_object_audio_review() {
        let notes = object_audio_review(Path::new("My Show.QLAB5"));
        assert!(notes[0].contains("before the first GO"));
        assert!(notes[0].contains("supported channel routes"));
        assert!(object_audio_review(Path::new("My Show.qlab4")).is_empty());
    }

    #[test]
    fn group_modes_map_from_qlabs_integers() {
        assert_eq!(group_mode(0), ("sequential", false));
        assert_eq!(group_mode(3), ("simultaneous", false));
        assert_eq!(group_mode(4), ("start_random", false));
        assert_eq!(group_mode(6), ("playlist", true));
    }

    #[test]
    fn imported_random_groups_use_advancing_standby_only_for_supported_sources() {
        let patches = Patches::default();
        let outputs = VideoOutputs::from_settings(&json!({}));
        let pans = std::collections::HashMap::new();
        let mut ctx = MapContext {
            legacy_v3: false,
            modern_group_modes: true,
            patches: &patches,
            outputs: &outputs,
            pan_starts: &pans,
        };
        let source = json!({"__class__": "GroupCue", "groupMode": 4, "cues": []});
        let modern = map_cue(&source, &mut ctx, &mut Vec::new(), &mut CueMetadata::default(), true);
        assert_eq!(modern["group_mode"], "start_random");
        assert_eq!(modern["random_advance_playhead"], true);

        ctx.legacy_v3 = true;
        ctx.modern_group_modes = false;
        let legacy = map_cue(&source, &mut ctx, &mut Vec::new(), &mut CueMetadata::default(), true);
        assert!(legacy.get("random_advance_playhead").is_none());
        assert_eq!(legacy["is_disabled"], true);
    }

    #[test]
    fn imported_enter_groups_require_review_for_nested_or_unsupported_programming() {
        let patches = Patches::default();
        let outputs = VideoOutputs::default();
        let pan_starts = std::collections::HashMap::new();
        let mut ctx = MapContext {
            legacy_v3: false, modern_group_modes: true, patches: &patches,
            outputs: &outputs, pan_starts: &pan_starts,
        };
        let source = json!({"__class__": "GroupCue", "groupMode": 1, "continueMode": 0, "cues": [
            {"__class__": "VideoCue", "continueMode": 0, "armed": true},
            {"__class__": "VideoCue", "continueMode": 0, "armed": false, "flagged": true}
        ]});
        let mut report = vec![];
        let mapped = map_cue_list(&json!({"cues": [source.clone()]}), &mut ctx, &mut report);
        assert_eq!(mapped["cues"][0]["group_mode"], "start_first_and_enter");
        assert_eq!(mapped["cues"][0]["is_disabled"], false);
        assert_eq!(mapped["cues"][0]["children"][1]["is_disabled"], true);
        assert_eq!(mapped["flagged_cues"][0], mapped["cues"][0]["children"][1]["id"]);

        let mut unsupported = source.clone();
        unsupported["cues"][0]["__class__"] = json!("ScriptCue");
        let mut crosses_boundary = source.clone();
        crosses_boundary["cues"][0]["continueMode"] = json!(2);
        for (unqualified, in_main_list) in [(source.clone(), false), (unsupported, true), (crosses_boundary, true)] {
            let mapped = map_cue(&unqualified, &mut ctx, &mut report, &mut CueMetadata::default(), in_main_list);
            assert_eq!(mapped["group_mode"], "start_first_and_enter");
            assert_eq!(mapped["is_disabled"], true);
            assert!(report.last().unwrap().note.as_deref().unwrap().contains("Start First And Enter"));
            assert_eq!(mapped["children"].as_array().unwrap().len(), 2);
        }
        ctx.modern_group_modes = false;
        let other = map_cue(&source, &mut ctx, &mut report, &mut CueMetadata::default(), true);
        assert_eq!(other["group_mode"], "sequential");
        assert_eq!(other["is_disabled"], false);
    }

    #[test]
    fn imported_start_first_group_advances_outer_standby_and_keeps_its_internal_chain() {
        use crate::{
            cue::{
                group_cue::GroupCueFactory,
                registry::CueRegistry,
                types::CueType,
                wait_cue::WaitCueFactory,
            },
            show::{cue_list::CueList, transport::Transport},
        };

        let patches = Patches::default();
        let outputs = VideoOutputs::default();
        let pan_starts = std::collections::HashMap::new();
        let mut ctx = MapContext {
            legacy_v3: false, modern_group_modes: true, patches: &patches,
            outputs: &outputs, pan_starts: &pan_starts,
        };
        let source = json!({"cues": [
            {"__class__": "GroupCue", "groupMode": 2, "continueMode": 0, "cues": [
                {"__class__": "WaitCue", "continueMode": 1, "duration": 30.0},
                {"__class__": "WaitCue", "continueMode": 0, "duration": 30.0},
                {"__class__": "WaitCue", "continueMode": 2, "armed": false, "flagged": true}
            ]},
            {"__class__": "WaitCue", "continueMode": 0, "duration": 30.0}
        ]});
        let mut report = vec![];
        let mapped = map_cue_list(&source, &mut ctx, &mut report);
        assert_eq!(mapped["cues"][0]["group_mode"], "start_first");
        assert_eq!(mapped["cues"][0]["is_disabled"], false);
        assert_eq!(mapped["cues"][0]["children"][2]["is_disabled"], true);
        assert_eq!(mapped["flagged_cues"][0], mapped["cues"][0]["children"][2]["id"]);
        assert!(report.iter().all(|row| row.note.is_none()));

        let mut registry = CueRegistry::new();
        registry.register(CueType::Group, Box::new(GroupCueFactory));
        registry.register(CueType::Wait, Box::new(WaitCueFactory));
        let mut list = CueList::from_json(mapped, &registry).unwrap();
        let group_id = list.cues[0].id();
        let outside_id = list.cues[1].id();
        let (context, _, _) = crate::test_support::recording_context();
        assert_eq!(Transport::new(context).go(&mut list).unwrap().triggered, [group_id]);
        assert_eq!(list.playhead_cue_id, Some(outside_id));
        assert!(!list.get(&outside_id).unwrap().is_running());
        let children = list.get(&group_id).unwrap().child_cues().unwrap();
        assert!(children[0].is_running());
        assert!(children[1].is_running());
        assert!(!children[2].is_running());
    }

    #[test]
    fn unsupported_qlab_start_first_groups_keep_children_and_arrive_disarmed() {
        let patches = Patches::default();
        let outputs = VideoOutputs::default();
        let pan_starts = std::collections::HashMap::new();
        let mut ctx = MapContext {
            legacy_v3: false, modern_group_modes: true, patches: &patches,
            outputs: &outputs, pan_starts: &pan_starts,
        };
        let source = json!({"__class__": "GroupCue", "name": "Deck review", "notes": "Original notes",
            "flagged": true, "groupMode": 2, "continueMode": 0, "armed": true, "cues": [
            {"__class__": "WaitCue", "name": "First", "continueMode": 0, "armed": true, "flagged": true},
            {"__class__": "WaitCue", "name": "Second", "continueMode": 0, "armed": true}
        ]});
        let mut scripted = source.clone();
        scripted["cues"][0]["__class__"] = json!("ScriptCue");
        let mut child_group = source.clone();
        child_group["cues"][0]["__class__"] = json!("GroupCue");
        let mut effect = source.clone();
        effect["cues"][0]["__class__"] = json!("AudioCue");
        effect["cues"][0]["effects"] = json!([{"__class__": "AudioEffect"}]);
        let mut automatic_parent = source.clone();
        automatic_parent["continueMode"] = json!(1);
        let mut automatic_tail = source.clone();
        automatic_tail["cues"][1]["continueMode"] = json!(2);
        let mut untimed_memo = source.clone();
        untimed_memo["cues"][0]["__class__"] = json!("MemoCue");
        untimed_memo["cues"][0]["preWait"] = json!(2.0);
        for (row, in_main_list) in [
            (source.clone(), false),
            (scripted, true),
            (child_group, true),
            (effect, true),
            (automatic_parent, true),
            (automatic_tail, true),
            (untimed_memo, true),
        ] {
            let mut report = vec![];
            let mut metadata = CueMetadata::default();
            let mapped = map_cue(&row, &mut ctx, &mut report, &mut metadata, in_main_list);
            assert_eq!(mapped["group_mode"], "start_first");
            assert_eq!(mapped["is_disabled"], true);
            assert_eq!(mapped["children"].as_array().unwrap().len(), 2);
            assert_eq!(mapped["children"][1]["name"], "Second");
            assert_eq!(metadata.flagged_cues, vec![mapped["children"][0]["id"].clone(), mapped["id"].clone()]);
            let notes = mapped["notes"].as_str().unwrap();
            assert!(notes.starts_with("Original notes"));
            assert!(notes.contains("Review QLab Start First"));
            assert!(report.last().unwrap().note.as_deref().unwrap().contains("Review QLab Start First"));
        }
        ctx.modern_group_modes = false;
        let legacy = map_cue(&source, &mut ctx, &mut Vec::new(), &mut CueMetadata::default(), true);
        assert_eq!(legacy["group_mode"], "sequential");
        assert_eq!(legacy["is_disabled"], false);
    }

    #[test]
    #[ignore = "needs INKUE_QLAB_ENTER_FIXTURE pointing to the tabletop QLab 4 archive"]
    fn leaves_the_nested_start_first_group_disarmed_in_the_tabletop_archive() {
        fn find_group(rows: &[Value]) -> Option<&Value> {
            rows.iter().find_map(|row| {
                if row["name"] == "Area of Effect Tokens" {
                    return Some(row);
                }
                row["children"].as_array().and_then(|children| find_group(children))
            })
        }

        let fixture = std::env::var_os("INKUE_QLAB_ENTER_FIXTURE").expect("set INKUE_QLAB_ENTER_FIXTURE");
        let (programming, report) = import_workspace(std::path::Path::new(&fixture)).unwrap();
        let document: Value = serde_json::from_str(&programming).unwrap();
        let group = document["cue_lists"].as_array().unwrap().iter()
            .find_map(|list| list["cues"].as_array().and_then(|rows| find_group(rows)))
            .expect("the tabletop archive contains its Area of Effect Tokens group");

        assert_eq!(group["group_mode"], "start_first");
        assert_eq!(group["is_disabled"], true);
        let children = group["children"].as_array().unwrap();
        assert_eq!(children.len(), 3);
        assert!(children.iter().all(|cue| cue["type"] == "group"));
        assert!(report.cues.iter().any(|cue| cue.cue_name == "Area of Effect Tokens"
            && cue.note.as_deref().is_some_and(|note| note.contains("Review QLab Start First"))));
    }

    #[test]
    #[ignore = "needs INKUE_QLAB_ENTER_FIXTURE pointing to the tabletop QLab 4 archive"]
    fn preserves_the_enter_mode_and_disarms_changed_children_in_the_tabletop_archive() {
        let fixture = std::env::var_os("INKUE_QLAB_ENTER_FIXTURE").expect("set INKUE_QLAB_ENTER_FIXTURE");
        let (programming, report) = import_workspace(std::path::Path::new(&fixture)).unwrap();
        let document: Value = serde_json::from_str(&programming).unwrap();
        let group = document["cue_lists"].as_array().unwrap().iter()
            .flat_map(|list| list["cues"].as_array().into_iter().flatten())
            .find(|cue| cue["name"] == "Adventure Handouts")
            .expect("the tabletop archive contains its Adventure Handouts group");

        assert_eq!(group["group_mode"], "start_first_and_enter");
        assert_eq!(group["is_disabled"], true);
        let children = group["children"].as_array().unwrap();
        assert_eq!(children.len(), 3);
        assert!(children.iter().all(|cue| cue["type"] == "video"));
        assert!(children.iter().any(|cue| cue["is_disabled"] == true));
        assert!(report.cues.iter().any(|cue| cue.cue_name == "Adventure Handouts"
            && cue.note.as_deref().is_some_and(|note| note.contains("Review QLab Start First And Enter"))));
    }

    #[test]
    fn qlab_timed_load_preserves_signed_offset_for_review() {
        let patches = Patches::default();
        let outputs = VideoOutputs::default();
        let pans = std::collections::HashMap::new();
        let mut ctx = MapContext {
            legacy_v3: false, modern_group_modes: true, patches: &patches,
            outputs: &outputs, pan_starts: &pans,
        };
        for (source, expected, flagged) in [(0.0, Value::Null, false), (-1.25, json!(-1250), true), (3.125, json!(3125), true)] {
            let mut report = vec![];
            let mapped = map_cue(&json!({"__class__":"LoadCue", "loadTime":source, "armed":true}),
                &mut ctx, &mut report, &mut CueMetadata::default(), true);
            assert_eq!(mapped["type"], "load");
            assert_eq!(mapped["load_time_ms"], expected);
            assert_eq!(mapped["is_disabled"], flagged);
            assert_eq!(report.last().unwrap().note.is_some(), flagged);
        }
    }

    #[test]
    fn qlab_playlist_preserves_explicit_loop_and_does_not_repeat_child_duration_as_gap() {
        let patches = Patches::default();
        let outputs = VideoOutputs::default();
        let pan_starts = std::collections::HashMap::new();
        let mut ctx = MapContext {
            legacy_v3: false, modern_group_modes: true, patches: &patches,
            outputs: &outputs, pan_starts: &pan_starts,
        };
        let mut source = json!({
            "__class__": "GroupCue", "name": "Lobby bed", "groupMode": 6, "continueMode": 0,
            "playlistLoop": false, "playlistCrossfade": false, "playlistShuffle": false,
            "secondTriggerAction": 0, "secondTriggerOnRelease": false, "cues": [
                {"__class__": "WaitCue", "name": "First wait", "continueMode": 1, "duration": 1.25, "postWait": 1.25},
                {"__class__": "WaitCue", "name": "Second wait", "continueMode": 1, "duration": 3.0, "postWait": 3.0},
            ]
        });
        for looped in [false, true] {
            source["playlistLoop"] = json!(looped);
            let mut report = vec![];
            let mapped = map_cue(&source, &mut ctx, &mut report, &mut CueMetadata::default(), true);
            assert_eq!(mapped["group_mode"], "playlist");
            assert_eq!(mapped["playlist_advance_playhead"], true);
            assert_eq!(mapped["playlist_loop"], looped);
            assert_eq!(mapped["is_disabled"], false);
            assert_eq!(mapped["children"][0]["wait_duration_ms"], 1250);
            assert_eq!(mapped["children"][0]["post_wait_ms"], 0);
            assert_eq!(mapped["children"][1]["post_wait_ms"], 0);
            assert!(report.last().unwrap().note.is_none());
        }
        for (field, value, in_main_list) in [
            ("playlistLoop", Value::Null, true),
            ("playlistCrossfade", json!(true), true),
            ("playlistShuffle", json!(true), true),
            ("secondTriggerAction", json!(1), true),
            ("secondTriggerOnRelease", json!(true), true),
            ("continueMode", json!(1), true),
            ("playlistLoop", json!(true), false),
        ] {
            let mut unqualified = source.clone();
            unqualified[field] = value;
            let mut report = vec![];
            let mapped = map_cue(&unqualified, &mut ctx, &mut report, &mut CueMetadata::default(), in_main_list);
            assert_eq!(mapped["group_mode"], "playlist");
            assert_eq!(mapped["is_disabled"], true, "{field}");
            assert_eq!(mapped["children"][0]["post_wait_ms"], 1250);
            assert!(report.last().unwrap().note.as_deref().unwrap().contains("Review QLab Playlist"));
        }
    }

    #[test]
    fn qlab_carts_load_without_legacy_inkue_chaining() {
        let patches = Patches::default();
        let outputs = VideoOutputs::from_settings(&json!({}));
        let pan_starts = std::collections::HashMap::new();
        let mut ctx = MapContext { legacy_v3: false, modern_group_modes: true, patches: &patches, outputs: &outputs, pan_starts: &pan_starts };
        let mapped = map_cue_list(
            &json!({"__class__": "GroupCue", "groupMode": 5, "cart": {}, "cues": []}),
            &mut ctx, &mut vec![],
        );
        let sequential = map_cue_list(
            &json!({"__class__": "GroupCue", "groupMode": 0, "cart": {}, "cues": []}),
            &mut ctx, &mut vec![],
        );
        assert_eq!(sequential["mode"], "sequential");
        let registry = crate::cue::registry::CueRegistry::new();
        let loaded = crate::show::cue_list::CueList::from_json(mapped, &registry).unwrap();
        assert!(loaded.independent_pads());
        assert_eq!(loaded.playhead_cue_id, None);
    }

    #[test]
    fn mapper_notes_reach_the_report_even_for_group_children() {
        let patches = Patches::default();
        let outputs = VideoOutputs::from_settings(&json!({}));
        let pan_starts = std::collections::HashMap::new();
        let mut ctx = MapContext { legacy_v3: false, modern_group_modes: true, patches: &patches, outputs: &outputs, pan_starts: &pan_starts };
        let mut report = vec![];
        let mapped = map_cue(&json!({
            "__class__": "GroupCue", "cues": [
                {"__class__": "MicCue", "number": "4.1", "notes": "Sound check"},
                {"__class__": "ScriptCue", "source": "example"}
            ]
        }), &mut ctx, &mut report, &mut CueMetadata::default(), true);
        assert!(report[0].note.as_deref().unwrap().contains("Mic tab"));
        assert_eq!(report[0].cue_number.as_deref(), Some("4.1"));
        assert!(mapped["children"][0]["notes"].as_str().unwrap().starts_with("Sound check"));
        assert!(mapped["children"][0].get("_conversion_note").is_none());
        assert!(report[1].note.as_deref().unwrap().contains("disarmed"));
        assert_eq!(mapped["children"][1]["is_disabled"], true);
    }

    #[test]
    fn imports_flags_on_nested_cues_without_flagging_list_wrappers() {
        let patches = Patches::default();
        let outputs = VideoOutputs::from_settings(&json!({}));
        let pan_starts = std::collections::HashMap::new();
        let mut ctx = MapContext { legacy_v3: false, modern_group_modes: true, patches: &patches, outputs: &outputs, pan_starts: &pan_starts };
        let list = map_cue_list(&json!({"flagged": true, "cues": [
            {"__class__":"GroupCue", "flagged":false, "cues":[
                {"__class__":"MemoCue", "name":"Review me", "flagged":true},
                {"__class__":"MemoCue", "name":"Keep running", "flagged":"true"}
            ]},
            {"__class__":"MemoCue", "name":"Marked pad", "flagged":true}
        ]}), &mut ctx, &mut Vec::new());
        assert_eq!(list["flagged_cues"], json!([
            list["cues"][0]["children"][0]["id"], list["cues"][1]["id"]
        ]));
        assert_ne!(list["cues"][0]["children"][0]["is_disabled"], true);
    }

    #[test]
    fn imports_second_colors_by_mapped_id_including_explicit_none() {
        let patches = Patches::default();
        let outputs = VideoOutputs::default();
        let pan_starts = std::collections::HashMap::new();
        let mut ctx = MapContext { legacy_v3: false, modern_group_modes: true, patches: &patches, outputs: &outputs, pan_starts: &pan_starts };
        let mut report = Vec::new();
        let list = map_cue_list(&json!({
            "useSecondColor": true, "secondColorName": "orange", "cues": [
                {"__class__":"GroupCue", "useSecondColor":true, "secondColorName":"Midnight",
                    "colorName":"Gray", "cues":[
                        {"__class__":"MemoCue", "uniqueID":"bad id", "useSecondColor":true, "secondColorName":"none"},
                        {"__class__":"MemoCue", "useSecondColor":false, "secondColorName":72},
                        {"__class__":"MemoCue", "secondColorName":"red"}
                    ]},
                {"__class__":"VideoCue", "audioCue":{"useSecondColor":true, "secondColorName":"pink"}}
            ]
        }), &mut ctx, &mut report);
        let group = &list["cues"][0];
        let colors = list["second_colors"].as_object().unwrap();
        assert_eq!(colors.len(), 2);
        assert_eq!(group["color"], "gray");
        assert_eq!(colors[group["id"].as_str().unwrap()], "midnight");
        assert_eq!(colors[group["children"][0]["id"].as_str().unwrap()], "none");
        assert!(report.iter().all(|cue| cue.note.as_deref().is_none_or(|note| !note.contains("QLab second color"))));
    }

    #[test]
    fn msc_list_number_survives_qlab_import_and_native_save() {
        let patches = Patches::default();
        let outputs = VideoOutputs::default();
        let pan_starts = std::collections::HashMap::new();
        let mut ctx = MapContext { legacy_v3: false, modern_group_modes: true, patches: &patches, outputs: &outputs, pan_starts: &pan_starts };
        let mapped = map_cue_list(&json!({"name":"Lighting", "number":"07", "cues":[]}), &mut ctx, &mut Vec::new());
        let registry = crate::cue::registry::CueRegistry::new();
        let list = crate::show::cue_list::CueList::from_json(mapped, &registry).unwrap();
        assert_eq!(list.number.as_deref(), Some("07"));
        assert_eq!(list.to_json()["number"], "07");
    }

    #[test]
    fn reviews_invalid_enabled_second_colors_even_on_cues_with_other_conversion_notes() {
        let patches = Patches::default();
        let outputs = VideoOutputs::default();
        let pan_starts = std::collections::HashMap::new();
        let mut ctx = MapContext { legacy_v3: false, modern_group_modes: true, patches: &patches, outputs: &outputs, pan_starts: &pan_starts };
        let mut report = Vec::new();
        let list = map_cue_list(&json!({"cues": [
            {"__class__":"MemoCue", "useSecondColor":true, "secondColorName":"ultraviolet", "notes":"Operator note"},
            {"__class__":"ScriptCue", "useSecondColor":true}
        ]}), &mut ctx, &mut report);
        assert_eq!(list["second_colors"], json!({}));
        assert!(report.iter().all(|cue| cue.note.as_deref().unwrap().contains("QLab second color")));
        assert!(report[0].note.as_deref().unwrap().contains("ultraviolet"));
        assert!(report[1].note.as_deref().unwrap().contains("AppleScript"));
        let notes = list["cues"][0]["notes"].as_str().unwrap();
        assert!(notes.starts_with("Operator note"));
        assert!(notes.contains("QLab second color"));
        assert_eq!(list["cues"][0]["is_disabled"], false);
    }

    #[test]
    #[ignore = "needs INKUE_QLAB_REDUNDANCY_FIXTURE pointing to the published QLab 5 archive"]
    fn preserves_both_enabled_second_colors_in_the_published_redundancy_workspace() {
        let path = std::env::var_os("INKUE_QLAB_REDUNDANCY_FIXTURE").expect("set INKUE_QLAB_REDUNDANCY_FIXTURE");
        let (document, _) = import_workspace(Path::new(&path)).unwrap();
        let document: Value = serde_json::from_str(&document).unwrap();
        let colors: BTreeMap<_, _> = document["cue_lists"].as_array().unwrap().iter()
            .flat_map(|list| list["second_colors"].as_object().unwrap())
            .map(|(id, color)| (id.as_str(), color)).collect();
        assert_eq!(colors.len(), 2);
        assert_eq!(colors["8680b2f5-8c4f-4554-9d8d-b67320b65a90"], "midnight");
        assert_eq!(colors["01e8cf44-6622-4048-acc8-3be9563fcde4"], "midnight");
    }

    #[test]
    fn media_counting_separates_found_from_missing() {
        let dir = std::env::temp_dir();
        let name = format!("{}.txt", uuid::Uuid::new_v4());
        std::fs::write(dir.join(&name), b"x").unwrap();
        let document = json!({ "cue_lists": [{ "cues": [
            { "file_path": name },
            { "file_path": "audio/gone.wav" },
            { "children": [{ "file_path": "video/also-gone.mov" }] },
        ]}]});
        let (found, missing) = count_media(&document, &dir);
        assert_eq!(found, 1);
        assert_eq!(missing.len(), 2);
        let _ = std::fs::remove_file(dir.join(&name));
    }
}
