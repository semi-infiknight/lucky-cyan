//! Carrying decoded audio across cue rebuilds and copies.
//!
//! Audio lives inside `AudioCue`/`VideoCue`, but a Group owns its children, so
//! anything that rebuilds a cue from JSON (undo, duplicate, paste, an inspector
//! edit on the group) must carry the decoded samples of the **whole subtree** —
//! not just of the top-level cue — or every child comes back "audio not loaded".

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;
use uuid::Uuid;

use crate::cue::traits::Cue;
use crate::cue::types::CueId;

/// Decoded interleaved samples plus (channels, sample rate Hz, total duration).
pub type DecodedAudio = (Arc<Vec<f32>>, u16, u32, Duration);

/// Decoded audio of a cue and every descendant, keyed by cue id.
pub type DecodedAudioMap = HashMap<CueId, DecodedAudio>;

/// Gather the decoded audio of `cue` and all its descendants.
pub fn collect_decoded_audio(cue: &dyn Cue) -> DecodedAudioMap {
    let mut map = DecodedAudioMap::new();
    collect_into(cue, &mut map);
    map
}

fn collect_into(cue: &dyn Cue, map: &mut DecodedAudioMap) {
    if let Some(audio) = cue.extract_decoded_audio() {
        map.insert(cue.id(), audio);
    }
    for child in cue.child_cues().unwrap_or_default() {
        collect_into(child.as_ref(), map);
    }
}

/// Push every entry of `audio` into the matching cue of `cue`'s subtree.
pub fn apply_decoded_audio(cue: &mut dyn Cue, audio: &DecodedAudioMap) {
    if let Some((samples, channels, sample_rate, duration)) = audio.get(&cue.id()) {
        cue.accept_preloaded_audio(Arc::clone(samples), *channels, *sample_rate, *duration);
    }
    if let Some(children) = cue.child_cues_mut() {
        for child in children.iter_mut() {
            apply_decoded_audio(child.as_mut(), audio);
        }
    }
}

/// Give a serialised cue and all its serialised children fresh ids, so a copy
/// never shares an id with its source.  Returns `new id → old id` so decoded
/// audio collected before the copy can be re-attached afterwards.
pub fn assign_fresh_ids(json: &mut Value) -> HashMap<CueId, CueId> {
    assign_fresh_ids_to_copies(std::slice::from_mut(json))
        .pop()
        .unwrap_or_default()
}

/// Give a whole Duplicate Selection its new IDs before rewriting any links.
/// Return one new-to-old map per root for its preserved decoded media.
pub fn assign_fresh_ids_to_copies(cues: &mut [Value]) -> Vec<HashMap<CueId, CueId>> {
    let mut roots = HashMap::new();
    let mut first_copy = HashMap::new();
    let mut copies = Vec::with_capacity(cues.len());
    for cue in cues.iter_mut() {
        let old_root = read_cue_id(&cue["id"]);
        let mut old_to_new = HashMap::new();
        let mut new_to_old = HashMap::new();
        assign_fresh_ids_into(cue, &mut old_to_new, &mut new_to_old);
        if let Some(old) = old_root {
            roots.entry(old).or_insert(old_to_new[&old]);
        }
        for (&old, &new) in &old_to_new {
            first_copy.entry(old).or_insert(new);
        }
        copies.push((old_to_new, new_to_old));
    }
    for (cue, (local, _)) in cues.iter_mut().zip(&copies) {
        // If both a Group and one of its children were selected, links in
        // the copied Group must use its own children. Links between roots
        // prefer the directly selected copy, then the first containing one.
        let lookup = |id: &CueId| {
            local.get(id).or_else(|| roots.get(id)).or_else(|| first_copy.get(id)).copied()
        };
        remap_cue_targets(cue, &lookup);
    }
    copies.into_iter().map(|(_, new_to_old)| new_to_old).collect()
}

fn read_cue_id(value: &Value) -> Option<CueId> {
    value.as_str()?.parse().ok()
}

fn assign_fresh_ids_into(
    json: &mut Value,
    old_to_new: &mut HashMap<CueId, CueId>,
    new_to_old: &mut HashMap<CueId, CueId>,
) {
    let new_id = Uuid::new_v4();
    if let Some(old_id) = read_cue_id(&json["id"]) {
        old_to_new.entry(old_id).or_insert(new_id);
        new_to_old.insert(new_id, old_id);
    }
    json["id"] = serde_json::json!(new_id.to_string());
    let Some(children) = json.get_mut("children").and_then(|v| v.as_array_mut()) else {
        return;
    };
    for child in children {
        assign_fresh_ids_into(child, old_to_new, new_to_old);
    }
}

fn remap_cue_targets(json: &mut Value, lookup: &impl Fn(&CueId) -> Option<CueId>) {
    let update = |value: &mut Value| {
        if let Some(id) = read_cue_id(value).and_then(|id| lookup(&id)) {
            *value = Value::String(id.to_string());
        }
    };
    if let Some(targets) = json.get_mut("target_cue_ids").and_then(Value::as_array_mut) {
        for target in targets {
            update(target);
        }
    }
    for key in ["target_cue_id", "crossfade_into_id", "replacement_id"] {
        if let Some(value) = json.get_mut(key) {
            update(value);
        }
    }
    if let Some(children) = json.get_mut("children").and_then(Value::as_array_mut) {
        for child in children {
            remap_cue_targets(child, lookup);
        }
    }
}

/// Re-attach audio collected from the *source* cues to a freshly-id'd copy.
pub fn apply_decoded_audio_to_copy(
    copy: &mut dyn Cue,
    source_audio: &DecodedAudioMap,
    new_to_old: &HashMap<CueId, CueId>,
) {
    let remapped: DecodedAudioMap = new_to_old
        .iter()
        .filter_map(|(new_id, old_id)| source_audio.get(old_id).map(|a| (*new_id, a.clone())))
        .collect();
    apply_decoded_audio(copy, &remapped);
}

/// Ids, file paths and types of every Audio/Video cue in `cue`'s subtree that
/// has a file but no decoded audio yet — the ones that still need a decode.
pub fn media_cues_needing_decode(cue: &dyn Cue) -> Vec<(CueId, crate::cue::types::CueType, std::path::PathBuf)> {
    let mut out = Vec::new();
    collect_needing_decode(cue, &mut out);
    out
}

fn collect_needing_decode(
    cue: &dyn Cue,
    out: &mut Vec<(CueId, crate::cue::types::CueType, std::path::PathBuf)>,
) {
    let is_media = matches!(
        cue.cue_type(),
        crate::cue::types::CueType::Audio | crate::cue::types::CueType::Video
    );
    if is_media && cue.extract_decoded_audio().is_none() {
        if let Some(path) = cue.media_file_path().filter(|p| !p.as_os_str().is_empty()) {
            out.push((cue.id(), cue.cue_type(), path.to_path_buf()));
        }
    }
    for child in cue.child_cues().unwrap_or_default() {
        collect_needing_decode(child.as_ref(), out);
    }
}

#[cfg(test)]
mod target_copy_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn copy_retargets_internal_group_links_and_retains_external_targets() {
        let audio = Uuid::new_v4();
        let fade = Uuid::new_v4();
        let external = Uuid::new_v4();
        let mut group = json!({
            "type": "group", "id": Uuid::new_v4(),
            "children": [
                {"type": "group", "id": Uuid::new_v4(), "children": [
                    {"type": "audio", "id": audio, "name": "Rain"},
                ]},
                {"type": "fade", "id": fade, "target_cue_ids": [audio, external],
                    "crossfade_into_id": audio, "patch_target_id": audio, "notes": audio.to_string()},
                {"type": "target", "id": Uuid::new_v4(), "target_cue_ids": [fade], "replacement_id": audio},
                {"type": "stop", "id": Uuid::new_v4(), "target_cue_id": audio},
            ],
        });

        let map = assign_fresh_ids(&mut group);
        let new_audio = read_cue_id(&group["children"][0]["children"][0]["id"]).unwrap();
        let new_fade = read_cue_id(&group["children"][1]["id"]).unwrap();
        assert_ne!(new_audio, audio);
        assert_ne!(new_fade, fade);
        assert_eq!(map[&new_audio], audio);
        assert_eq!(group["children"][1]["target_cue_ids"], json!([new_audio, external]));
        assert_eq!(group["children"][1]["crossfade_into_id"], json!(new_audio));
        assert_eq!(group["children"][1]["patch_target_id"], json!(audio));
        assert_eq!(group["children"][1]["notes"], audio.to_string());
        assert_eq!(group["children"][2]["target_cue_ids"], json!([new_fade]));
        assert_eq!(group["children"][2]["replacement_id"], json!(new_audio));
        assert_eq!(group["children"][3]["target_cue_id"], json!(new_audio));
    }

    #[test]
    fn duplicate_selection_maps_cross_root_targets_and_keeps_group_links_local() {
        let audio = Uuid::new_v4();
        let external = Uuid::new_v4();
        let fade = Uuid::new_v4();
        let mut selected = [
            json!({"type": "group", "id": Uuid::new_v4(), "children": [
                {"type": "audio", "id": audio},
                {"type": "fade", "id": fade, "target_cue_ids": [audio]},
            ]}),
            json!({"type": "audio", "id": audio}),
            json!({"type": "fade", "id": Uuid::new_v4(), "target_cue_ids": [audio, fade, external]}),
        ];

        let maps = assign_fresh_ids_to_copies(&mut selected);
        let local_audio = read_cue_id(&selected[0]["children"][0]["id"]).unwrap();
        let local_fade = read_cue_id(&selected[0]["children"][1]["id"]).unwrap();
        let selected_audio = read_cue_id(&selected[1]["id"]).unwrap();
        assert_ne!(local_audio, selected_audio);
        assert_eq!(maps[0][&local_audio], audio);
        assert_eq!(maps[1][&selected_audio], audio);
        assert_eq!(selected[0]["children"][1]["target_cue_ids"], json!([local_audio]));
        assert_eq!(selected[2]["target_cue_ids"], json!([selected_audio, local_fade, external]));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{audio_cue::AudioCueFactory, registry::CueRegistry, types::CueType};

    fn registry() -> CueRegistry {
        let mut registry = CueRegistry::new();
        registry.register(CueType::Audio, Box::new(AudioCueFactory));
        registry
    }

    /// A group holding two audio children, the first one with a file.
    fn group_json() -> Value {
        serde_json::json!({
            "type": "group",
            "id": Uuid::new_v4().to_string(),
            "children": [
                { "type": "audio", "id": Uuid::new_v4().to_string(), "file_path": "C:/media/a.wav" },
                { "type": "audio", "id": Uuid::new_v4().to_string() },
            ],
        })
    }

    fn decode_first_child(group: &mut Box<dyn Cue>) -> CueId {
        let child = &mut group.child_cues_mut().unwrap()[0];
        child.accept_preloaded_audio(Arc::new(vec![0.0; 8]), 2, 48_000, Duration::from_millis(1));
        child.id()
    }

    #[test]
    fn collect_descends_into_groups() {
        let mut group = registry().from_json(group_json()).unwrap();
        let child_id = decode_first_child(&mut group);
        let map = collect_decoded_audio(group.as_ref());
        assert_eq!(map.len(), 1);
        assert!(map.contains_key(&child_id));
    }

    #[test]
    fn apply_restores_children_of_a_rebuilt_group() {
        let mut group = registry().from_json(group_json()).unwrap();
        decode_first_child(&mut group);
        let audio = collect_decoded_audio(group.as_ref());

        let mut rebuilt = registry().from_json(group.serialize()).unwrap();
        assert!(rebuilt.child_cues().unwrap()[0].extract_decoded_audio().is_none());
        apply_decoded_audio(rebuilt.as_mut(), &audio);
        assert!(rebuilt.child_cues().unwrap()[0].extract_decoded_audio().is_some());
    }

    #[test]
    fn fresh_ids_cover_children_and_map_back_to_the_originals() {
        let mut group = registry().from_json(group_json()).unwrap();
        let child_id = decode_first_child(&mut group);
        let mut json = group.serialize();
        let new_to_old = assign_fresh_ids(&mut json);

        let new_child_id: CueId = json["children"][0]["id"].as_str().unwrap().parse().unwrap();
        assert_ne!(new_child_id, child_id);
        assert_eq!(new_to_old[&new_child_id], child_id);
        assert_eq!(new_to_old.len(), 3);
    }

    #[test]
    fn copy_receives_the_source_audio_under_its_new_ids() {
        let mut group = registry().from_json(group_json()).unwrap();
        decode_first_child(&mut group);
        let source_audio = collect_decoded_audio(group.as_ref());
        let mut json = group.serialize();
        let new_to_old = assign_fresh_ids(&mut json);

        let mut copy = registry().from_json(json).unwrap();
        apply_decoded_audio_to_copy(copy.as_mut(), &source_audio, &new_to_old);
        assert!(copy.child_cues().unwrap()[0].extract_decoded_audio().is_some());
        assert_ne!(copy.child_cues().unwrap()[0].id(), group.child_cues().unwrap()[0].id());
    }

    #[test]
    fn needing_decode_lists_only_undecoded_media_with_a_file() {
        let mut group = registry().from_json(group_json()).unwrap();
        assert_eq!(media_cues_needing_decode(group.as_ref()).len(), 1);
        decode_first_child(&mut group);
        assert!(media_cues_needing_decode(group.as_ref()).is_empty());
    }
}
