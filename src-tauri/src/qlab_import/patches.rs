//! Workspace-level patch tables from a QLab workspace's `settings`.
//!
//! QLab cues reference their destinations by patch id. Inkue needs an OSC
//! patch object for an OSC cue, and a plain **device name** for a MIDI cue —
//! so the tables are read once up front and consulted while cues are mapped.

use std::collections::HashMap;

use serde_json::{json, Value};

/// A UUID of all zeroes — an OSC message with nowhere to go. Preflight reports
/// it as a missing patch rather than the import failing outright.
const NIL_UUID: &str = "00000000-0000-0000-0000-000000000000";

/// Resolved destination tables for one workspace.
#[derive(Debug, Default)]
pub struct Patches {
    /// Native output patches and the archived references used by cues.
    pub audio: Vec<Value>,
    audio_by_qlab_id: HashMap<String, (usize, Option<String>)>,
    archived_audio_names: HashMap<usize, Value>,
    /// Inkue `osc_patches` entries, emitted into the workspace document.
    pub osc: Vec<Value>,
    /// QLab `networkPatchID` → Inkue OSC patch uuid.
    osc_by_qlab_id: HashMap<String, String>,
    /// QLab `midiPatchID` → MIDI output port name.
    midi_by_qlab_id: HashMap<String, String>,
    /// QLab `cameraPatchID` → capture device name.
    camera_by_qlab_id: HashMap<String, String>,
}

impl Patches {
    /// Read every patch table out of a resolved `settings` dictionary.
    pub fn from_settings(settings: &Value) -> Self {
        let mut patches = Self::default();
        patches.read_audio(settings);
        patches.read_network(settings);
        patches.read_midi(settings);
        patches.read_camera(settings);
        patches
    }

    fn read_audio(&mut self, settings: &Value) {
        if let Some(list) = settings
            .pointer("/Audio/audioOutputPatches")
            .and_then(Value::as_array)
        {
            for patch in list {
                let Some(key) = patch["uniqueID"].as_str() else {
                    continue;
                };
                let count = patch["cueOutputChannels"].as_u64().unwrap_or(0);
                let routes = output_channels(patch, count);
                let system = patch["useSystemDevice"].as_bool() == Some(true);
                let device = patch
                    .pointer("/deviceDescription/name")
                    .and_then(Value::as_str)
                    .unwrap_or("unidentified device");
                let mut review = Vec::new();
                if !system {
                    review.push(format!("assign the local equivalent of {device}"));
                }
                if routes.is_none() {
                    review.push("rebuild the patch matrix".into());
                }
                let effects = ["cueOutputEffects", "patchOutputEffects"]
                    .iter()
                    .any(|key| {
                        patch[*key].as_array().is_some_and(|buses| {
                            buses.iter().any(|bus| {
                                bus["effects"]
                                    .as_array()
                                    .is_some_and(|effects| !effects.is_empty())
                            })
                        })
                    });
                if effects {
                    review.push("recreate its audio effects".into());
                }
                if ["soloChannels", "muteChannels"]
                    .iter()
                    .any(|key| selection_present(&patch[*key]))
                {
                    review.push("check its mute and solo selections".into());
                }
                // The sampled archives contain empty dictionaries. Keep nonempty originals
                // available for review until their channel key convention is verified.
                if selection_present(&patch["cueOutputNames"]) {
                    self.archived_audio_names.insert(self.audio.len(), patch["cueOutputNames"].clone());
                }
                let channels = routes.unwrap_or_default();
                let device_id = if system {
                    String::new()
                } else {
                    format!("QLab device: {device}")
                };
                let entry = json!({"id": normalise_uuid(key), "name": patch["name"].as_str().unwrap_or("QLab Audio Patch"),
                    "device_id": device_id, "channels": channels, "gain_db": super::cues::master_db(patch)});
                self.audio_by_qlab_id.insert(
                    key.to_ascii_lowercase(),
                    (
                        self.audio.len(),
                        (!review.is_empty()).then(|| review.join(", ")),
                    ),
                );
                self.audio.push(entry);
            }
        }
        if let Some(list) = settings
            .pointer("/Audio/patches")
            .and_then(Value::as_object)
        {
            for (number, patch) in list {
                let name = patch["name"].as_str().unwrap_or("Unknown device");
                let entry = json!({"id": uuid::Uuid::new_v4(), "name": format!("Patch {number} - {name}"),
                    "device_id": format!("QLab device: {name}"), "channels": [], "gain_db": 0.0});
                self.audio_by_qlab_id.insert(
                    number.clone(),
                    (
                        self.audio.len(),
                        Some(format!(
                            "assign the local equivalent of {name} and verify its channels"
                        )),
                    ),
                );
                self.audio.push(entry);
            }
        }
    }

    pub(super) fn assign_audio(&self, cue: &Value, fields: &mut serde_json::Map<String, Value>) {
        let key = cue["audioOutputPatchID"]
            .as_str()
            .map(str::to_ascii_lowercase)
            .or_else(|| cue["patch"].as_u64().map(|number| number.to_string()));
        let previous = cue["lastSeenAudioOutputPatchInfo"]["nameForMenuItem"]
            .as_str()
            .or(cue["lastSeenAudioOutputPatchInfo"]["name"].as_str());
        if let Some((index, review)) = key.as_ref().and_then(|key| self.audio_by_qlab_id.get(key)) {
            let patch = &self.audio[*index];
            if let Some(names) = self.archived_audio_names.get(index) {
                super::cues::add_note(fields, &format!("[Enter the archived QLab channel names in the output patch after checking their positions: {names}]"));
            }
            fields.insert("output_patch_id".into(), patch["id"].clone());
            if let Some(action) = review {
                let name = previous.or(patch["name"].as_str()).unwrap_or("");
                fields.insert("is_disabled".into(), json!(true));
                super::cues::add_note(fields, &format!("[QLab audio output {name}: {action} in Preferences → Audio → Output Patches, then arm this cue.]"));
            }
        } else if let Some(name) = previous.or(key.as_deref()) {
            fields.insert("is_disabled".into(), json!(true));
            super::cues::add_note(fields, &format!("[QLab audio output {name} is missing from the patch table. Assign a local output patch and verify its channels before arming this cue.]"));
        }
    }

    fn read_network(&mut self, settings: &Value) {
        let Some(list) = settings.pointer("/Network/networkPatches").and_then(Value::as_array)
        else {
            return;
        };
        for patch in list {
            let Some(data) = patch.get("data") else { continue };
            let Some(qlab_id) = data.get("uniqueID").and_then(Value::as_str) else { continue };
            let client = data
                .get("clientStates")
                .and_then(Value::as_array)
                .and_then(|c| c.first());
            let host = client
                .and_then(|c| c.get("host"))
                .and_then(Value::as_str)
                .unwrap_or("127.0.0.1");
            let port = client
                .and_then(|c| c.get("port"))
                .and_then(Value::as_i64)
                .unwrap_or(53000);

            let id = normalise_uuid(qlab_id);
            self.osc.push(json!({
                "id": id,
                "name": data.get("name").and_then(Value::as_str).unwrap_or("QLab Patch"),
                // Inkue sends to "{ip}:{port}", which resolves hostnames — but
                // spelling out loopback keeps a DNS lookup off the cue path.
                "ip": if host.eq_ignore_ascii_case("localhost") { "127.0.0.1" } else { host },
                "port": port.clamp(1, 65535),
            }));
            self.osc_by_qlab_id.insert(qlab_id.to_string(), id);
        }
    }

    fn read_midi(&mut self, settings: &Value) {
        let Some(list) = settings.pointer("/MIDI/midiPatches").and_then(Value::as_array) else {
            return;
        };
        for patch in list {
            let data = patch.get("data").unwrap_or(patch);
            let Some(qlab_id) = data.get("uniqueID").and_then(Value::as_str) else { continue };
            let name = data
                .get("destinationName")
                .or_else(|| data.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            self.midi_by_qlab_id.insert(qlab_id.to_string(), name.to_string());
        }
    }

    fn read_camera(&mut self, settings: &Value) {
        let Some(list) = settings.pointer("/Camera/cameraPatches").and_then(Value::as_array) else {
            return;
        };
        for patch in list {
            let Some(qlab_id) = patch.get("uniqueID").and_then(Value::as_str) else { continue };
            let name = patch
                .pointer("/serializedDescription/name")
                .or_else(|| patch.get("name"))
                .and_then(Value::as_str)
                .unwrap_or("");
            self.camera_by_qlab_id.insert(qlab_id.to_string(), name.to_string());
        }
    }

    /// The Inkue OSC patch id for a QLab network patch, falling back to the
    /// only patch in the show (the common single-destination case).
    pub fn osc_patch(&self, qlab_id: Option<&str>) -> String {
        if let Some(id) = qlab_id.and_then(|id| self.osc_by_qlab_id.get(id)) {
            return id.clone();
        }
        match self.osc.first().and_then(|p| p.get("id")).and_then(Value::as_str) {
            Some(only) => only.to_string(),
            None => NIL_UUID.to_string(),
        }
    }

    pub fn midi_port(&self, qlab_id: Option<&str>) -> String {
        qlab_id
            .and_then(|id| self.midi_by_qlab_id.get(id))
            .cloned()
            .unwrap_or_default()
    }

    pub fn camera_device(&self, qlab_id: Option<&str>) -> String {
        qlab_id
            .and_then(|id| self.camera_by_qlab_id.get(id))
            .cloned()
            .unwrap_or_default()
    }
}

/// A native patch maps each cue output to exactly one hardware channel at unity.
fn output_channels(patch: &Value, count: u64) -> Option<Vec<u16>> {
    let matrix = super::audio_matrix::read(&patch["levels"])?;
    if matrix.exceeds_capacity || count > crate::engine::voice::MATRIX_OUTPUTS as u64 {
        return None;
    }
    let count = if count == 0 {
        matrix.rows.len()
    } else {
        count as usize
    };
    (0..count)
        .map(|row| {
            let mut active = matrix
                .rows
                .get(row)?
                .iter()
                .enumerate()
                .filter(|(_, db)| **db > -60.0);
            let (column, gain) = active.next()?;
            (gain.abs() < 1e-8 && active.next().is_none()).then_some(column as u16)
        })
        .collect()
}

fn selection_present(selection: &Value) -> bool {
    match selection {
        Value::Null => false,
        Value::Array(items) => !items.is_empty(),
        Value::Object(items) => !items.is_empty(),
        _ => true,
    }
}

/// QLab ids are UUIDs already; normalise the case so they match Inkue's form.
fn normalise_uuid(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .unwrap_or_else(|_| uuid::Uuid::new_v4())
        .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn settings() -> Value {
        json!({
            "Network": { "networkPatches": [{
                "data": {
                    "uniqueID": "EEFA94D4-A2D7-4E60-B4B1-AF785383F70A",
                    "name": "Patch 1",
                    "clientStates": [{ "host": "localhost", "port": 53000 }],
                },
            }]},
            "Camera": { "cameraPatches": [{
                "uniqueID": "66E48FF8-D203-428C-A9A3-C7551CB684C0",
                "name": "Patch 1",
                "serializedDescription": { "name": "FaceTime HD Camera" },
            }]},
            "MIDI": { "midiPatches": [] },
        })
    }

    #[test]
    fn system_patch_preserves_uuid_master_and_crossed_channels() {
        let key = "2AB85AAB-3899-4A5B-B1AC-BE041772A279";
        let patch = json!({"uniqueID":key, "name":"Reversed", "useSystemDevice":true, "cueOutputChannels":2,
            "patchOutputEffects":[{"effects":[]}], "muteChannels":null, "cueOutputNames":{"2":"Backstage"},
            "levels":{"entries":[{"row":0,"column":0,"initialLevel":0.5},
                    {"row":1,"column":2},{"row":2,"column":1}]}});
        let patches = Patches::from_settings(&json!({"Audio":{"audioOutputPatches":[patch]}}));
        let parsed: crate::engine::device_manager::OutputPatch =
            serde_json::from_value(patches.audio[0].clone()).unwrap();
        assert_eq!(parsed.id, uuid::Uuid::parse_str(key).unwrap());
        assert_eq!(parsed.name, "Reversed");
        assert!(parsed.device_id.is_empty());
        assert_eq!(parsed.channels, [1, 0]);
        assert!((parsed.gain_db + 6.0206).abs() < 0.001);
        let mut fields = serde_json::Map::new();
        patches.assign_audio(&json!({"audioOutputPatchID":key}), &mut fields);
        assert_eq!(fields["output_patch_id"], key.to_ascii_lowercase());
        assert!(!fields.contains_key("is_disabled"));
        assert!(fields["notes"].as_str().unwrap().contains("Backstage"));
    }

    #[test]
    fn complex_and_unavailable_audio_outputs_require_repatch_and_review() {
        let patch = json!({"uniqueID":"bus", "name":"Monitor bus", "cueOutputChannels":1,
            "deviceDescription":{"name":"Former interface"}, "patchOutputEffects":[{"effects":[{"name":"Limiter"}]}],
            "soloChannels":[1], "levels":{"entries":[{"row":1,"column":1},{"row":1,"column":2}]}});
        let patches = Patches::from_settings(&json!({"Audio":{"audioOutputPatches":[patch],
            "patches":{"1":{"name":"Built-in Output","deviceUID":"AppleHDA"}}}}));
        for (source, expected_notes) in [
            (
                json!({"audioOutputPatchID":"bus"}),
                vec![
                    "Former interface",
                    "matrix",
                    "audio effects",
                    "mute and solo",
                ],
            ),
            (
                json!({"patch":1}),
                vec!["Built-in Output", "verify its channels"],
            ),
            (
                json!({"audioOutputPatchID":"missing"}),
                vec!["missing from the patch table"],
            ),
        ] {
            let mut fields = serde_json::Map::new();
            patches.assign_audio(&source, &mut fields);
            assert_eq!(fields["is_disabled"], true);
            let notes = fields["notes"].as_str().unwrap();
            for expected in expected_notes {
                assert!(notes.contains(expected), "{notes}");
            }
        }
    }

    #[test]
    fn a_network_patch_becomes_an_osc_patch() {
        let patches = Patches::from_settings(&settings());
        assert_eq!(patches.osc.len(), 1);
        assert_eq!(patches.osc[0]["port"], json!(53000));
        assert_eq!(patches.osc[0]["ip"], json!("127.0.0.1"), "loopback spelled out");
        assert_eq!(
            patches.osc_patch(Some("EEFA94D4-A2D7-4E60-B4B1-AF785383F70A")),
            "eefa94d4-a2d7-4e60-b4b1-af785383f70a"
        );
    }

    #[test]
    fn an_unknown_patch_falls_back_to_the_only_one() {
        let patches = Patches::from_settings(&settings());
        assert_eq!(patches.osc_patch(None), patches.osc[0]["id"].as_str().unwrap());
    }

    #[test]
    fn with_no_patches_at_all_osc_points_nowhere_rather_than_failing() {
        let patches = Patches::default();
        assert_eq!(patches.osc_patch(Some("whatever")), NIL_UUID);
    }

    #[test]
    fn a_camera_patch_yields_its_device_name() {
        let patches = Patches::from_settings(&settings());
        assert_eq!(
            patches.camera_device(Some("66E48FF8-D203-428C-A9A3-C7551CB684C0")),
            "FaceTime HD Camera"
        );
        assert_eq!(patches.camera_device(Some("nope")), "");
    }

    #[test]
    fn absent_settings_sections_are_not_an_error() {
        let patches = Patches::from_settings(&json!({}));
        assert!(patches.osc.is_empty());
        assert_eq!(patches.midi_port(Some("x")), "");
    }
}
