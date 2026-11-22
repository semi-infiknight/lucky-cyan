//! Transfer the visual settings both desktops can represent directly.
use serde_json::{json, Map, Value};

use super::archive::checkbox;
use super::outputs::VideoOutputs;

pub fn apply(cue: &Value, fields: &mut Map<String, Value>, outputs: &VideoOutputs) {
    fields.insert("output_id".into(), json!(outputs.output_of(cue)));
    let kind = fields.get("cue_type").and_then(Value::as_str).unwrap_or("");
    let opacity = cue.get("opacity").and_then(Value::as_f64).unwrap_or(1.0);
    let layer = cue.get("layer").and_then(Value::as_f64);
    let mut review = Vec::new();

    if let Some(stage) = outputs.review(cue) {
        review.push(stage.to_owned());
    }
    let effects = cue.pointer("/videoEffects/effectEntries").and_then(Value::as_array);
    if let Some(effects) = effects.filter(|entries| !entries.is_empty()) {
        review.push(format!("Recreate the QLab video effect chain ({} entries)", effects.len()));
    } else if cue.get("doEffect").and_then(Value::as_bool) == Some(true) {
        review.push("Recreate the QLab video effect and its parameters".to_owned());
    }
    if kind == "text" {
        // The text overlay uses a separate rendering path.
        if opacity != 1.0 || layer.is_some_and(|n| n != 1000.0) {
            let layer = layer.map(|n| n.to_string()).unwrap_or_else(|| "automatic".into());
            review.push(format!(
                "Check title compositing (QLab opacity {opacity}, layer {layer})"
            ));
        }
    } else {
        let native_layer = layer.filter(|n| (0.0..=1000.0).contains(n) && n.fract() == 0.0);
        if native_layer != layer || !(0.0..=1.0).contains(&opacity) {
            review.push("Check the original layer and opacity range".to_owned());
        }
        let blend = cue
            .pointer("/blendMode/index")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        if blend != 0 {
            review.push(format!(
                "Select the corresponding blend mode (QLab index {blend})"
            ));
        }
        if kind == "video" {
            if let Some(hold) = cue.get("holdLastFrame").and_then(Value::as_bool) {
                fields.insert("hold_last_frame".into(), json!(hold));
            }
        }
        fields.insert(
            "layer_style".into(),
            json!({
                "layer": native_layer.map(|n| n as u32),
                "opacity": opacity.clamp(0.0, 1.0), "blend_mode": "normal",
            }),
        );
        let full_screen = cue.get("fullScreen").and_then(checkbox);
        if full_screen == Some(true) {
            let mode = full_screen_fit(cue, &mut review);
            fields.insert("geometry".into(), json!({"fit_mode": mode}));
        } else if full_screen == Some(false) {
            // QLab custom scale is relative to native pixels; Inkue scales
            // relative to the fitted frame. Without the media and the stage's
            // final display raster, treating the multipliers alike is incorrect.
            let scalar = |key, default| cue.get(key).and_then(Value::as_f64).unwrap_or(default);
            review.push(format!("Set custom geometry against the stage (QLab position {}, {} pixels, scale {} × {}, origin {}, {})",
                scalar("translationX", 0.0), scalar("translationY", 0.0),
                scalar("scaleX", 1.0), scalar("scaleY", 1.0),
                scalar("originX", 0.0), scalar("originY", 0.0)));
            if let Some(rotation) = cue.get("quaternion").filter(|v| !v.is_null()) {
                review.push(format!("Recreate the source rotation {rotation}"));
            }
        }
        let crop = ["cropLeft", "cropRight", "cropTop", "cropBottom"]
            .map(|k| cue.get(k).and_then(Value::as_f64).unwrap_or(0.0));
        if crop.iter().any(|v| *v != 0.0) {
            review.push(format!("Set source crop in the geometry inspector (QLab left/right/top/bottom {}/{}/{}/{} pixels)",
                crop[0], crop[1], crop[2], crop[3]));
        }
    }

    if !review.is_empty() {
        super::cues::add_note(fields, &review.join(". "));
    }
}

fn full_screen_fit(cue: &Value, review: &mut Vec<String>) -> &'static str {
    // QLab 5's Fill Stage style takes precedence over the legacy checkbox.
    match cue.get("fillStyle").and_then(Value::as_i64) {
        Some(0) => return "fit",
        Some(1) => return "fill",
        Some(2) => return "stretch",
        _ => {}
    }

    if cue.get("fillStyle").is_some_and(|value| !value.is_null()) {
        review.push("Check automatic sizing (unrecognized QLab fill style)".into());
    }
    match cue.get("preserveAspectRatio").filter(|value| !value.is_null()) {
        None => "fit",
        Some(value) => match checkbox(value) {
            Some(false) => "stretch",
            Some(true) => "fit",
            None => {
                review.push("Check automatic sizing (unrecognized QLab Preserve Aspect Ratio setting)".into());
                "fit"
            }
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picture_properties_match_archived_qlab_video_fields() {
        let mut fields = json!({"cue_type":"video"});
        apply(
            &json!({"fullScreen":true, "preserveAspectRatio":false, "opacity":0.0,
                      "layer":500.0, "holdLastFrame":true}),
            fields.as_object_mut().unwrap(),
            &VideoOutputs::default(),
        );
        assert_eq!(fields["geometry"]["fit_mode"], "stretch");
        assert_eq!(fields["layer_style"]["opacity"], 0.0);
        assert_eq!(fields["layer_style"]["layer"], 500);
        assert_eq!(fields["hold_last_frame"], true);
        assert!(fields.get("_conversion_note").is_none());
    }

    #[test]
    fn qlab4_full_surface_accepts_the_archived_integer_checkbox() {
        for (preserve, mode) in [(0, "stretch"), (1, "fit")] {
            let mut fields = json!({"cue_type":"video"});
            let source = json!({"fullScreen":true, "preserveAspectRatio":preserve});
            apply(&source, fields.as_object_mut().unwrap(), &VideoOutputs::default());
            assert_eq!(fields["geometry"]["fit_mode"], mode);
            assert!(fields.get("_conversion_note").is_none());
        }
    }

    #[test]
    fn qlab5_fill_stage_maps_the_three_known_styles_ahead_of_the_old_checkbox() {
        for (fill, preserve, mode) in [(0, false, "fit"), (1, true, "fill"), (2, true, "stretch")] {
            let mut fields = json!({"cue_type":"camera"});
            let source = json!({"fullScreen":true, "fillStyle":fill, "preserveAspectRatio":preserve});
            apply(&source, fields.as_object_mut().unwrap(), &VideoOutputs::default());
            assert_eq!(fields["geometry"]["fit_mode"], mode);
            assert!(fields.get("_conversion_note").is_none());
        }
    }

    #[test]
    fn unknown_sizing_is_reported_and_custom_geometry_does_not_inherit_fill_stage() {
        let mut fields = json!({"cue_type":"video"});
        apply(
            &json!({"fullScreen":true, "fillStyle":99, "preserveAspectRatio":0}),
            fields.as_object_mut().unwrap(),
            &VideoOutputs::default(),
        );
        assert_eq!(fields["geometry"]["fit_mode"], "stretch");
        assert!(fields["_conversion_note"].as_str().unwrap().contains("unrecognized QLab fill style"));

        let mut fields = json!({"cue_type":"video"});
        apply(
            &json!({"fullScreen":true, "preserveAspectRatio":"false"}),
            fields.as_object_mut().unwrap(),
            &VideoOutputs::default(),
        );
        assert_eq!(fields["geometry"]["fit_mode"], "fit");
        assert!(fields["_conversion_note"].as_str().unwrap().contains("unrecognized QLab Preserve Aspect Ratio"));

        let mut fields = json!({"cue_type":"video"});
        apply(
            &json!({"fullScreen":false, "fillStyle":1, "translationX":125}),
            fields.as_object_mut().unwrap(),
            &VideoOutputs::default(),
        );
        assert!(fields.get("geometry").is_none());
        assert!(fields["_conversion_note"].as_str().unwrap().contains("125"));
    }

    #[test]
    fn qlab4_absolute_opacity_fade_preserves_target_level_duration_and_stop() {
        let mapped = super::super::cues::fade(&json!({
            "doOpacity":true,"opacity":0.75,"duration":5.0,"stopTargetWhenDone":true,
            "geoFade":{"fadeType":"absolute"}
        }));
        assert_eq!(mapped["target_brightness_pct"], 75.0);
        assert_eq!(mapped["fade_duration_ms"], 5000);
        assert_eq!(mapped["fade_brightness"], true);
        assert_eq!(mapped["stop_at_end"], true);
        assert!(mapped.get("_conversion_note").is_none());
    }

    #[test]
    fn silent_video_soundtrack_and_embedded_routes_survive_the_cue_factory() {
        use crate::cue::{traits::CueFactory, video_cue::VideoCueFactory};
        let source = json!({"audioCue": {"levels": {"rows": 3, "columns": 4, "entries": [
            {"row":0,"column":0,"initialLevel":0.0,"trimLevel":1.0},
            {"row":1,"column":3,"initialLevel":0.25,"trimLevel":1.0}
        ]}}});
        let imported = super::super::cues::video(&source);
        let saved = VideoCueFactory.from_json(imported).unwrap().serialize();
        assert_eq!(saved["volume_db"], -60.0);
        assert_eq!(saved["matrix_mirror_mono"], false);
        let routed = saved["level_matrix"][0][2].as_f64().unwrap();
        assert!((routed - 20.0 * 0.25_f64.log10()).abs() < 0.001);
        assert_eq!(saved["level_matrix"][0][0], -60.0);
        let legacy = VideoCueFactory.from_json(json!({})).unwrap().serialize();
        assert_eq!(legacy["matrix_mirror_mono"], true);
    }

    #[test]
    fn audio_only_and_relative_fades_leave_visual_brightness_unchanged() {
        let audio_only = super::super::cues::fade(&json!({"doOpacity": false}));
        assert_eq!(audio_only["fade_brightness"], false);
        let relative = super::super::cues::fade(&json!({
            "doOpacity": true, "opacity": 0.5, "geoFade": {"fadeType": "relative"}
        }));
        assert_eq!(relative["fade_brightness"], false);
        assert!(relative["_conversion_note"]
            .as_str()
            .unwrap()
            .contains("relative"));
    }

    #[test]
    fn custom_geometry_and_prior_warnings_survive_in_the_review() {
        let mut fields = json!({"cue_type":"camera", "notes":"Director's note"});
        super::super::cues::add_note(fields.as_object_mut().unwrap(), "Reconnect camera");
        apply(
            &json!({"fullScreen":false, "scaleX":1.5, "scaleY":2.0, "translationY":76,
                      "cropTop":20, "videoEffects":{"effectEntries":[{}]}}),
            fields.as_object_mut().unwrap(),
            &VideoOutputs::default(),
        );
        let note = fields["_conversion_note"].as_str().unwrap();
        assert!(
            note.contains("76 pixels")
                && note.contains("1.5 × 2")
                && note.contains("Reconnect camera")
                && note.contains("video effect chain")
        );
        assert!(fields["notes"]
            .as_str()
            .unwrap()
            .contains("Director's note"));
    }
}
