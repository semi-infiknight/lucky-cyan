//! Preserve a QLab lighting cue for offline review against the new desk's patch.
//! QLab instrument identifiers are labels, and its default parameter and units
//! belong to the original instrument definition. Guessing a DMX address here
//! would route the cue to an arbitrary fixture in the destination venue.
use serde_json::{json, Value};

use super::cues;

pub(super) fn light(cue: &Value) -> (Value, String) {
    let mut out = cues::common(cue, "light");
    let text = cue.get("commandText").and_then(Value::as_str);
    let duration = cue.get("duration").and_then(Value::as_f64).unwrap_or(0.0);
    out.insert("is_disabled".into(), json!(true));
    out.insert("targets".into(), json!([]));
    out.insert("fade".into(), json!({
        "duration_ms": (duration.max(0.0) * 1000.0).round() as u64,
        "curve": "s_curve",
    }));
    // Keep the resolved source, including unknown lighting data, curve shapes
    // and collate flags. It is inert and survives Save As and Collect.
    out.insert("qlab_light_source".into(), cue.clone());
    let mut note = match text.filter(|s| !s.trim().is_empty()) {
        Some(_) => "Light cue disarmed. Review the original commands against patched fixture names, parameter units and the fade curve in the Light inspector.",
        None => "Light cue disarmed. This archive did not provide command text. Its decoded cue data is available in the Light inspector for rebuilding the look.",
    }.to_owned();
    if cue.get("alwaysCollate").and_then(Value::as_bool) == Some(true) {
        note.push_str(" QLab collates earlier light cues here. Rebuild that accumulated look before enabling this cue.");
    }
    (Value::Object(out), note)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cue::{light_cue::LightCueFactory, traits::CueFactory};

    #[test]
    fn imported_source_and_timing_survive_the_native_light_factory() {
        let source = json!({"__class__":"LightCue", "uniqueID":"61673897-58d3-46c3-b45e-66b72abb988b",
            "name":"Act one", "number":"LX2", "commandText":"front.intensity = 42\nfill = cue A",
            "duration":1.25, "alwaysCollate":true, "notes":"Operator notes", "continueMode":2});
        let (doc, warning) = light(&source);
        let saved = LightCueFactory.from_json(doc).unwrap().serialize();
        assert_eq!(saved["type"], "light");
        assert_eq!(saved["qlab_light_source"], source);
        assert_eq!(saved["id"], source["uniqueID"]);
        assert_eq!(saved["number"], "LX2");
        assert_eq!(saved["notes"], "Operator notes");
        assert_eq!(saved["continue_mode"], "auto_follow");
        assert_eq!(saved["fade"]["duration_ms"], 1250);
        assert_eq!(saved["targets"], json!([]));
        assert_eq!(saved["is_disabled"], true);
        assert!(warning.contains("collates"));
    }

    #[test]
    fn archives_without_command_text_retain_the_decoded_data() {
        let source = json!({"__class__":"LightCue", "fade":{"entries":{"unrecognised": 4}}});
        let (doc, warning) = light(&source);
        assert_eq!(doc["qlab_light_source"], source);
        assert!(warning.contains("did not provide command text"));
    }
    /// Verifies the LightCue and its native round-trip using an actual decoded
    /// QLab archive. Workspaces with a saved lighting template can exercise
    /// this even when their running list contains only audio/video cues.
    #[test]
    #[ignore = "needs INKUE_QLAB_FIXTURE pointing at a QLab workspace with a LightCue or template"]
    fn archived_light_cue_roundtrip() {
        fn find(value: &Value) -> Option<&Value> {
            match value {
                Value::Object(_) if value["__class__"] == "LightCue" => Some(value),
                Value::Object(fields) => fields.values().find_map(find),
                Value::Array(items) => items.iter().find_map(find),
                _ => None,
            }
        }
        let path = std::env::var_os("INKUE_QLAB_FIXTURE").expect("provide a QLab archive");
        let bytes = std::fs::read(path).unwrap();
        let outer = super::super::archive::resolve(&bytes).unwrap();
        let nested = outer.get("cueLists").and_then(super::super::archive::nested_data).unwrap();
        let tree = super::super::archive::resolve(&nested).unwrap();
        let source = find(&tree).or_else(|| find(&outer)).expect("archive contains a LightCue");
        let (doc, _) = light(source);
        let saved = LightCueFactory.from_json(doc).unwrap().serialize();
        assert_eq!(saved["qlab_light_source"], *source);
        assert_eq!(saved["type"], "light");
        assert_eq!(saved["is_disabled"], true);
        assert_eq!(saved["targets"], json!([]));
    }

}
