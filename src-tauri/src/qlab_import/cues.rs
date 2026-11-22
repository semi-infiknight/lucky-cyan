//! QLab cue → Inkue cue JSON.
//!
//! A port of the reference implementation in `qlab2inkue/qlab_to_inkue.py`,
//! which is where a new QLab property should be decoded and proven first (it
//! can decode an unknown `.qlab5` offline, and its `test_mapping.py` runs
//! against a fixture holding one cue of every type).
//!
//! Two rules the whole module follows:
//!
//! - **Nothing is ever dropped silently.** A cue with no Inkue counterpart
//!   becomes a clearly-named Memo carrying whatever it can, so the operator
//!   sees what needs rebuilding and where.
//! - **Nothing is pretended.** A QLab Script cue holds AppleScript, which has
//!   no command-line equivalent; it is imported disarmed with the script in
//!   the notes rather than as a cue that would silently do nothing.

use serde_json::{json, Map, Value};

use crate::cue::types::CueColor;

use super::patches::Patches;

/// QLab `continueMode` → Inkue.
fn continue_mode(cue: &Value) -> &'static str {
    match cue.get("continueMode").and_then(Value::as_i64).unwrap_or(0) {
        1 => "auto_continue",
        2 => "auto_follow",
        _ => "do_not_continue",
    }
}

pub(super) fn parse_cue_color(value: &Value) -> Option<CueColor> {
    let name = value.as_str()?.trim().to_ascii_lowercase().replace(' ', "_");
    serde_json::from_value(Value::String(name)).ok()
}

/// Seconds → milliseconds, treating 0 and absent alike (QLab writes both).
fn ms(cue: &Value, key: &str) -> Option<u64> {
    let seconds = cue.get(key).and_then(Value::as_f64)?;
    if seconds == 0.0 {
        return None;
    }
    Some((seconds * 1000.0).round() as u64)
}

fn ms_or_zero(cue: &Value, key: &str) -> u64 {
    ms(cue, key).unwrap_or(0)
}

/// Reuse QLab's `uniqueID` as the Inkue cue id — it is already a UUID, which
/// is what makes every cue target resolve without a lookup table.
fn cue_id(cue: &Value) -> String {
    cue.get("uniqueID")
        .and_then(Value::as_str)
        .and_then(|s| uuid::Uuid::parse_str(s).ok())
        .unwrap_or_else(uuid::Uuid::new_v4)
        .to_string()
}

/// `cueTargetUniqueID` as Inkue's target list.
fn target_ids(cue: &Value) -> Vec<String> {
    match cue.get("cueTargetUniqueID").and_then(Value::as_str) {
        Some(id) if !id.is_empty() => vec![uuid::Uuid::parse_str(id)
            .unwrap_or_else(|_| uuid::Uuid::new_v4())
            .to_string()],
        _ => Vec::new(),
    }
}

fn loop_count(cue: &Value) -> u64 {
    // QLab 5's Impossible Wall workspace stores Infinite Loop as integer 1
    // on three Video cues and their nested Audio cues.
    if cue.get("infiniteLoop").and_then(super::archive::checkbox) == Some(true) {
        return u32::MAX as u64;
    }
    let plays = cue.get("playCount").and_then(Value::as_i64).unwrap_or(1);
    plays.saturating_sub(1).max(0) as u64
}

/// The fields every cue type shares.
pub(super) fn common(cue: &Value, cue_type: &str) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("type".into(), json!(cue_type));
    out.insert("cue_type".into(), json!(cue_type));
    out.insert("id".into(), json!(cue_id(cue)));
    let number = cue.get("number").and_then(Value::as_str).filter(|s| !s.is_empty());
    out.insert("number".into(), json!(number));
    out.insert("name".into(), json!(cue.get("name").and_then(Value::as_str).unwrap_or("")));
    out.insert("notes".into(), json!(cue.get("notes").and_then(Value::as_str).unwrap_or("")));
    out.insert("color".into(), json!(parse_cue_color(&cue["colorName"]).unwrap_or_default()));
    out.insert("pre_wait_ms".into(), json!(ms_or_zero(cue, "preWait")));
    out.insert("post_wait_ms".into(), json!(ms_or_zero(cue, "postWait")));
    out.insert("continue_mode".into(), json!(continue_mode(cue)));
    // QLab arms a cue; Inkue disables it.
    let armed = cue.get("armed").and_then(Value::as_bool).unwrap_or(true);
    out.insert("is_disabled".into(), json!(!armed));
    out
}

/// Append an explanatory line to a cue's notes without losing what is there.
pub(super) fn add_note(cue_json: &mut Map<String, Value>, extra: &str) {
    let existing = cue_json.get("notes").and_then(Value::as_str).unwrap_or("").to_string();
    let combined = if existing.is_empty() {
        extra.to_string()
    } else {
        format!("{existing}\n\n{extra}")
    };
    cue_json.insert("notes".into(), json!(combined));
    // Consumed by map_cue before the workspace is serialized.
    let warning = match cue_json.get("_conversion_note").and_then(Value::as_str) {
        Some(prior) => format!("{prior}\n{extra}"),
        None => extra.to_string(),
    };
    cue_json.insert("_conversion_note".into(), json!(warning));
}

// ---------------------------------------------------------------------------
// Media paths
// ---------------------------------------------------------------------------

/// Bundle-relative media path, across QLab 4 and 5.
///
/// QLab 5 wraps the target in an `F53Alias` carrying `relativePath`; QLab 4
/// uses an `NSURL`. Either way a bundle-relative path is what we want: the
/// caller resolves it against the workspace folder.
pub fn media_path(cue: &Value) -> Option<String> {
    let target = cue.get("fileTarget")?;
    if let Some(rel) = target.get("relativePath").and_then(Value::as_str) {
        if !rel.is_empty() {
            return Some(rel.to_string());
        }
    }
    if target.get("__class__").and_then(Value::as_str) == Some("NSURL") {
        if let Some(rel) = target.get("relative").and_then(Value::as_str) {
            return Some(rel.replace("file://", ""));
        }
    }
    // Last resort: keep from the last media folder of an absolute path.
    let last_known = target.get("lastKnownPath").and_then(Value::as_str)?;
    Some(relative_from_absolute(last_known))
}

fn relative_from_absolute(path: &str) -> String {
    let normalised = path.replace('\\', "/");
    let lower = normalised.to_lowercase();
    for folder in ["/audio/", "/video/", "/midi file/", "/midi/"] {
        if let Some(index) = lower.rfind(folder) {
            return normalised[index + 1..].to_string();
        }
    }
    normalised.rsplit('/').next().unwrap_or(&normalised).to_string()
}

// ---------------------------------------------------------------------------
// Audio levels
// ---------------------------------------------------------------------------

/// Find one archived crosspoint in an `AudioLevelMatrix`.
fn crosspoint(matrix: &Value, row: i64, column: i64) -> Option<&Value> {
    let entries = matrix.get("entries")?;
    let matches_channel = |entry: &&Value| {
        entry.get("row").and_then(Value::as_i64) == Some(row)
            && entry.get("column").and_then(Value::as_i64) == Some(column)
    };
    match entries {
        Value::Object(map) => map.values().find(matches_channel),
        Value::Array(items) => items.iter().find(matches_channel),
        _ => None,
    }
}

/// Master level in dB. QLab silence (0.0 linear) maps to Inkue's -60 floor; a
/// missing matrix means unity. This is the level a following Fade ramps *from*,
/// so a wrong value here silently breaks fade-ins.
pub fn master_db(cue: &Value) -> f64 {
    let Some(matrix) = cue.get("levels") else { return 0.0 };
    let gain = crosspoint(matrix, 0, 0).map(|entry| {
        entry["initialLevel"].as_f64().unwrap_or(1.0) * entry["trimLevel"].as_f64().unwrap_or(1.0)
    });
    match gain {
        None => 0.0,
        Some(gain) if gain <= 0.0 => -60.0,
        Some(gain) => (20.0 * gain.log10()).max(-60.0),
    }
}

/// Report the two source controls while retaining the existing folded import.
/// Decoded QLab 4 archives contain non-unity values at this master crosspoint.
/// The examined QLab 5 masters carry the same fields with unity trim.
pub(super) fn master_trim_review(cue: &Value) -> Option<String> {
    let master = crosspoint(cue.get("levels")?, 0, 0)?;
    let trim = master["trimLevel"].as_f64()?;
    if trim == 1.0 {
        return None;
    }
    let level = |gain: f64| match gain {
        0.0 => "silence".to_owned(),
        gain if gain > 0.0 => format!("{:+.2} dB", 20.0 * gain.log10()),
        gain => format!("invalid archive gain ({gain})"),
    };
    Some(format!(
        "Review QLab main trim. Source main level {} and main trim {}. The imported main level already includes this trim. Rebuild the two controls together on a rehearsal copy and verify linked Fades, Stop and retrigger.",
        master["initialLevel"].as_f64().map(level).unwrap_or_else(|| "unavailable in archive".into()),
        level(trim),
    ))
}

// ---------------------------------------------------------------------------
// Per-type mappings
// ---------------------------------------------------------------------------

pub(super) fn apply_audio_levels(out: &mut Map<String, Value>, cue: &Value, has_pan_fade: bool) {
    // The AudioCue "effects" array also appears on a movie's embedded
    // soundtrack. Its units and states require operator reconstruction.
    if let Some(effects) = cue.get("effects").and_then(Value::as_array).filter(|effects| !effects.is_empty()) {
        out.insert("is_disabled".into(), json!(true));
        add_note(out, &format!(
            "[Rebuild and compare the QLab audio effect chain ({} entries) before arming this cue.]",
            effects.len(),
        ));
    }
    if let Some(names) = cue.get("inputNames").filter(|value| match value {
        Value::Object(entries) => !entries.is_empty(),
        Value::Array(entries) => !entries.is_empty(),
        Value::String(text) => !text.trim().is_empty(),
        _ => false,
    }) {
        add_note(out, &format!("[Review QLab source channel names against the original channel order: {names}]"));
    }
    out.insert("volume_db".into(), json!(master_db(cue)));
    if let Some(matrix) = cue.get("levels").and_then(super::audio_matrix::read) {
        if has_pan_fade {
            add_note(out, "[This cue keeps its imported stereo pan fade. Review the original crosspoints before assigning a different output.]");
        } else {
            out.insert("matrix_mirror_mono".into(), json!(false));
            out.insert("level_matrix".into(), json!(matrix.rows));
        }
        if matrix.exceeds_capacity {
            add_note(out, "[An active audio route exceeds 128 channels. Repatch it before running this cue.]");
        }
    }
    if ["muteChannels","soloChannels"].iter().any(|key| cue.get(key).is_some_and(|v| !v.is_null())) {
        out.insert("is_disabled".into(), json!(true));
        add_note(out, "[Review the original cue’s mute and solo selections before arming it.]");
    }

}

pub fn audio(cue: &Value, pan_starts: &std::collections::HashMap<String, f64>) -> Value {
    let mut out = common(cue, "audio");
    out.insert("file_path".into(), json!(media_path(cue)));
    // A following pan fade's start position wins over the cue's own matrix.
    out.insert("pan".into(), json!(start_pan(cue, pan_starts)));
    apply_audio_levels(&mut out, cue, pan_starts.contains_key(&cue_id(cue)));
    out.insert("fade_in_ms".into(), Value::Null);
    out.insert("fade_in_curve".into(), Value::Null);
    out.insert("fade_out_ms".into(), Value::Null);
    out.insert("fade_out_curve".into(), Value::Null);
    out.insert("start_time_ms".into(), json!(ms(cue, "startTime")));
    out.insert("end_time_ms".into(), json!(ms(cue, "endTime")));
    out.insert("loop_count".into(), json!(loop_count(cue)));
    out.insert("output_patch_id".into(), Value::Null);
    out.insert("rate".into(), json!(cue.get("rate").and_then(Value::as_f64).unwrap_or(1.0)));
    Value::Object(out)
}

pub fn video(cue: &Value) -> Value {
    let mut out = common(cue, "video");
    out.insert("file_path".into(), json!(media_path(cue)));
    // Real QLab 4 and 5 Video cues archive a private AudioCue for their
    // soundtrack, including an independent master level and routing matrix.
    let audio = cue.get("audioCue").filter(|v| v.is_object()).unwrap_or(cue);
    apply_audio_levels(&mut out, audio, false);
    out.insert("start_time_ms".into(), json!(ms(cue, "startTime")));
    out.insert("end_time_ms".into(), json!(ms(cue, "endTime")));
    out.insert("loop_count".into(), json!(loop_count(cue)));
    out.insert("output_patch_id".into(), Value::Null);
    Value::Object(out)
}

pub fn camera(cue: &Value, patches: &Patches) -> Value {
    let mut out = common(cue, "camera");
    let device = patches.camera_device(cue.get("cameraPatchID").and_then(Value::as_str));
    out.insert(
        "source".into(),
        json!({ "kind": "device", "id": device, "name": device }),
    );
    if !device.is_empty() {
        add_note(
            &mut out,
            &format!("[QLab camera patch: \"{device}\" — a macOS capture device name; \
                      re-pick the camera on Windows or Linux]"),
        );
    }
    Value::Object(out)
}

pub fn memo(cue: &Value, text: &str) -> Value {
    let mut out = common(cue, "memo");
    out.insert("memo_text".into(), json!(text));
    Value::Object(out)
}

pub fn wait(cue: &Value) -> Value {
    let mut out = common(cue, "wait");
    out.insert("wait_duration_ms".into(), json!(ms_or_zero(cue, "duration")));
    Value::Object(out)
}

/// Start / Pause / Load / Reset / Goto / Arm / Disarm — one Inkue type each.
pub fn control(cue: &Value, cue_type: &str) -> Value {
    let mut out = common(cue, cue_type);
    out.insert("target_cue_ids".into(), json!(target_ids(cue)));
    out.insert("target_cue_numbers".into(), json!(Vec::<String>::new()));
    Value::Object(out)
}

pub fn stop(cue: &Value) -> Value {
    let mut out = common(cue, "stop");
    out.insert("target_cue_ids".into(), json!(target_ids(cue)));
    out.insert("target_cue_numbers".into(), json!(Vec::<String>::new()));
    // QLab's Stop uses the target's own fade time; Inkue's soft stop matches.
    out.insert("hard_stop_mode".into(), json!(false));
    Value::Object(out)
}

pub fn devamp(cue: &Value) -> Value {
    let mut out = common(cue, "devamp");
    out.insert("target_cue_ids".into(), json!(target_ids(cue)));
    out.insert("target_cue_numbers".into(), json!(Vec::<String>::new()));
    let stop = cue.get("stopTargetWhenDone").or_else(|| cue.get("stopTargetWhenSliceEnds"));
    out.insert("stop_at_end".into(), json!(stop.and_then(Value::as_bool).unwrap_or(false)));
    let mode = cue.get("devampType").and_then(Value::as_i64);
    let next = cue.get("fireNextCueAtMomentLoopEnds").or_else(|| cue.get("startNextCueWhenSliceEnds"));
    if mode != Some(1) || next.is_some_and(|v| v.as_bool() != Some(false)) ||
        stop.is_some_and(|v| v.as_bool().is_none()) {
        out.insert("is_disabled".into(), json!(true));
        let scope = match mode {
            Some(2) => "whole looping cue".into(),
            Some(1) => "current slice".into(),
            _ => format!("unconfirmed scope ({})", cue["devampType"]),
        };
        add_note(&mut out, &format!(
            "[Devamp disarmed. QLab scope: {scope}. Start next cue at boundary: {}. Stop target at boundary: {}. Check the original inspector. Rebuild whole-cue releases or boundary-triggered cues and rehearse before arming.]",
            next.unwrap_or(&Value::Bool(false)), stop.unwrap_or(&Value::Bool(false))
        ));
    }
    Value::Object(out)
}

pub fn script(cue: &Value) -> Value {
    let mut out = common(cue, "script");
    out.insert("command".into(), json!(""));
    out.insert("args".into(), json!(Vec::<String>::new()));
    out.insert("working_dir".into(), Value::Null);
    out.insert("timeout_ms".into(), json!(30_000));
    // Disarmed on purpose: a cue that silently did nothing would be worse.
    out.insert("is_disabled".into(), json!(true));
    let source = cue.get("source").and_then(Value::as_str).unwrap_or("");
    add_note(
        &mut out,
        &format!(
            "[QLab AppleScript — no command-line equivalent; rewrite this as an \
             executable + arguments, then re-arm the cue]\n\n{source}"
        ),
    );
    Value::Object(out)
}

/// Type an OSC argument the way QLab's single message string implies.
fn osc_arg(token: &str) -> Value {
    if let Ok(int) = token.parse::<i64>() {
        return json!({ "type": "int", "value": int });
    }
    if let Ok(float) = token.parse::<f64>() {
        return json!({ "type": "float", "value": float });
    }
    json!({ "type": "str", "value": token })
}

pub fn osc(cue: &Value, patches: &Patches) -> Value {
    let mut out = common(cue, "osc");
    let raw = cue.get("oscString").and_then(Value::as_str).unwrap_or("").trim().to_string();
    let patch_id = patches.osc_patch(cue.get("networkPatchID").and_then(Value::as_str));
    let mut parts = raw.split_whitespace();
    let messages = match parts.next() {
        Some(address) => json!([{
            "patch_id": patch_id,
            "address": address,
            "args": parts.map(osc_arg).collect::<Vec<_>>(),
        }]),
        None => json!([]),
    };
    out.insert("messages".into(), messages);
    let text = cue.get("plainTextString").and_then(Value::as_str).filter(|s| !s.is_empty());
    let hex = cue.get("hexCodesString").and_then(Value::as_str).filter(|s| !s.trim().is_empty());
    if text.is_some() || hex.is_some() {
        out.insert("is_disabled".into(), json!(true));
        match (text, hex, raw.is_empty()) {
            (Some(payload), None, true) | (None, Some(payload), true) => {
                let format = if text.is_some() { "text" } else { "hex" };
                out.insert("messages".into(), json!([{"patch_id":patch_id, "address":"/", "args":[],
                    "raw":{"format":format, "value":payload}}]));
                add_note(&mut out, "[Review the imported raw UDP payload and network patch before arming this cue.]");
            }
            _ => {
                add_note(&mut out, &format!("[Multiple QLab Network payloads. Select the original format and verify the patch before arming.]\nOSC: {raw}\nText: {}\nHex: {}", text.unwrap_or(""), hex.unwrap_or("")));
            }
        }
    }
    Value::Object(out)
}

/// QLab's voice-message index follows the MIDI status nibbles.
fn midi_message_type(status: i64) -> Option<&'static str> {
    match status {
        0 => Some("note_off"),
        1 => Some("note_on"),
        2 => Some("key_pressure"),
        3 => Some("control_change"),
        4 => Some("program_change"),
        5 => Some("channel_pressure"),
        6 => Some("pitch_bend"),
        _ => None,
    }
}

fn midi_for_review(cue: &Value, mut out: Map<String, Value>, reason: &str) -> Value {
    out.insert("is_disabled".into(), json!(true));
    out.insert("messages".into(), json!([]));
    add_note(&mut out, reason);
    let notes = out["notes"].as_str().unwrap_or("");
    out.insert("notes".into(), json!(format!("{notes}\n\nOriginal QLab MIDI cue fields:\n{cue}")));
    Value::Object(out)
}

fn msc(cue: &Value, patches: &Patches, mut out: Map<String, Value>) -> Value {
    use crate::cue::msc::{MscCommand, MscMessage};
    let Some(command) = cue["command"].as_u64().and_then(MscCommand::from_code) else {
        return midi_for_review(cue, out, "Review unsupported MSC command.");
    };
    let byte = |key: &str| cue[key].as_u64().and_then(|v| u8::try_from(v).ok());
    let operand = |key: &str| match cue.get(key) {
        None | Some(Value::Null) => Some(String::new()),
        Some(value) => value.as_str().map(str::to_owned),
    };
    let (Some(device_id), Some(command_format), Some(q_number), Some(q_list), Some(q_path)) =
        (byte("deviceID"), byte("commandFormat"), operand("qNumber"), operand("qList"), operand("qPath"))
    else {
        return midi_for_review(cue, out, "Review missing or invalid MSC device, format or operand.");
    };
    let message = MscMessage { device_id, command_format, command, q_number, q_list, q_path };
    if let Err(error) = message.to_bytes() {
        return midi_for_review(cue, out, &error);
    }
    let port = patches.midi_port(cue.get("midiPatchID").and_then(Value::as_str));
    out.insert("messages".into(), json!([{"message_type":"msc","port_name":port,
        "channel":1,"data1":0,"data2":0,"msc":message}]));
    out.insert("is_disabled".into(), json!(true));
    add_note(&mut out, "Verify the individual MSC receiver, output port, command and cue address before arming.");
    Value::Object(out)
}

pub fn midi(cue: &Value, patches: &Patches) -> Value {
    let mut out = common(cue, "midi");
    if cue["messageType"].as_i64() == Some(2) {
        return msc(cue, patches, out);
    }
    if let Some(family) = cue.get("messageType") {
        if family.as_i64() != Some(1) {
            return midi_for_review(cue, out, "Review QLab MIDI message family (SysEx or unknown).");
        }
    }
    if cue.get("doFade").is_some_and(|v| *v != false && *v != 0) {
        return midi_for_review(cue, out, "Review QLab MIDI fade timing and values before rebuilding this cue.");
    }
    let status = cue.get("status").and_then(Value::as_i64).unwrap_or(-1);
    let Some(kind) = midi_message_type(status) else {
        return midi_for_review(cue, out, "Review unknown QLab MIDI voice command.");
    };
    let data_byte = |field: &str| cue[field].as_u64().filter(|v| *v <= 127);
    let mut first = data_byte("byte1");
    let mut second = data_byte("byte2");
    if kind == "pitch_bend" && cue.get("byteCombo").is_some() {
        let Some(value) = cue["byteCombo"].as_u64().filter(|v| *v <= 16383) else {
            return midi_for_review(cue, out, "Review out-of-range QLab pitch bend.");
        };
        first = Some(value & 127);
        second = Some(value >> 7);
    } else if kind == "control_change" && first == Some(0) && data_byte("controlNumber").unwrap_or(0) != 0 {
        first = data_byte("controlNumber");
        second = data_byte("controlValue");
    }
    if matches!(kind, "program_change" | "channel_pressure") {
        second = Some(0);
    }
    let channel = cue["channel"].as_u64().filter(|v| (1..=16).contains(v));
    let (Some(data1), Some(data2), Some(channel)) = (first, second, channel) else {
        return midi_for_review(cue, out, "Review missing or invalid QLab MIDI channel or data bytes.");
    };
    let port = patches.midi_port(cue.get("midiPatchID").and_then(Value::as_str));
    if port.trim().is_empty() {
        out.insert("is_disabled".into(), json!(true));
        add_note(&mut out, "Select a local MIDI output port and rehearse the route before arming this cue.");
    }
    out.insert(
        "messages".into(),
        json!([{
            "port_name": port,
            "message_type": kind, "channel": channel, "data1": data1, "data2": data2,
        }]),
    );
    Value::Object(out)
}

pub fn midi_file(cue: &Value, patches: &Patches) -> Value {
    let mut out = common(cue, "midi_file");
    out.insert("file_path".into(), json!(media_path(cue)));
    out.insert(
        "port_name".into(),
        json!(patches.midi_port(cue.get("midiPatchID").and_then(Value::as_str))),
    );
    out.insert(
        "playback_rate".into(),
        json!(cue.get("playbackRate").and_then(Value::as_f64).unwrap_or(1.0)),
    );
    Value::Object(out)
}

pub fn titles(cue: &Value) -> Value {
    let mut out = common(cue, "text");
    let font = cue
        .get("lastSeenFontNames")
        .and_then(Value::as_array)
        .and_then(|fonts| fonts.first())
        .and_then(Value::as_str)
        .unwrap_or("Helvetica");
    out.insert(
        "text".into(),
        json!(cue.get("titlesAttributedString").and_then(Value::as_str).unwrap_or("")),
    );
    out.insert("font".into(), json!(font));
    out.insert("font_size".into(), json!(48));
    out.insert("text_color".into(), json!("#FFFFFF"));
    out.insert("position".into(), json!("center"));
    out.insert("display_duration_ms".into(), json!(ms(cue, "duration")));
    Value::Object(out)
}

pub fn mic(cue: &Value) -> Value {
    let mut out = common(cue, "mic");
    let count = cue.get("channels").and_then(Value::as_i64).unwrap_or(1).max(1);
    let offset = cue.get("channelOffset").and_then(Value::as_i64).unwrap_or(0);
    let channels: Vec<i64> = (offset..offset + count).collect();
    out.insert("input_patch_id".into(), Value::Null);
    out.insert("input_channels".into(), json!(channels));
    out.insert("output_patch_id".into(), Value::Null);
    out.insert("volume_db".into(), json!(master_db(cue)));
    out.insert("pan".into(), json!(0.0));
    out.insert("fade_in_ms".into(), Value::Null);
    out.insert("fade_in_curve".into(), Value::Null);
    out.insert("fade_out_ms".into(), Value::Null);
    out.insert("fade_out_curve".into(), Value::Null);
    add_note(
        &mut out,
        "[Input Patch not set — QLab's audio input patch has no Inkue equivalent \
         to import; pick one in the Mic tab]",
    );
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// Fade
// ---------------------------------------------------------------------------

/// The `live` fade entries a QLab Fade cue actually animates.
fn live_fade_entries(cue: &Value) -> Vec<&Value> {
    let Some(entries) = cue.pointer("/fade/entries") else { return Vec::new() };
    let values: Vec<&Value> = match entries {
        Value::Object(map) => map.values().collect(),
        Value::Array(items) => items.iter().collect(),
        _ => return Vec::new(),
    };
    values
        .into_iter()
        .filter(|e| e.get("live").and_then(Value::as_bool).unwrap_or(false))
        .collect()
}

fn linear_to_db(gain: f64) -> f64 {
    if gain <= 0.0 { -60.0 } else { (20.0 * gain.log10()).max(-60.0) }
}

/// Target master level in dB, or `None` when the fade does not touch the
/// master crosspoint (a pan or geometry fade).
fn fade_target_db(cue: &Value) -> Option<f64> {
    live_fade_entries(cue)
        .into_iter()
        .find(|e| {
            e.get("row").and_then(Value::as_i64) == Some(0)
                && e.get("column").and_then(Value::as_i64) == Some(0)
        })
        .map(|e| linear_to_db(e.get("endValue").and_then(Value::as_f64).unwrap_or(1.0)))
}

/// Inkue pan (-1 left … +1 right) from a linear left/right output pair.
fn pan_from_lr(left: f64, right: f64) -> f64 {
    let total = left + right;
    if total <= 0.0 { 0.0 } else { ((right - left) / total).clamp(-1.0, 1.0) }
}

/// The live row-0 crosspoints on *output* columns, sorted left to right.
///
/// A pan fade animates those rather than the master, so the lowest active
/// column is L and the highest is R.
fn pan_entries(cue: &Value) -> Vec<&Value> {
    let mut entries: Vec<&Value> = live_fade_entries(cue)
        .into_iter()
        .filter(|e| {
            e.get("row").and_then(Value::as_i64) == Some(0)
                && e.get("column").and_then(Value::as_i64).unwrap_or(0) > 0
        })
        .collect();
    entries.sort_by_key(|e| e.get("column").and_then(Value::as_i64).unwrap_or(0));
    entries
}

/// Destination pan of a QLab pan fade, or `None` if it is not one.
fn fade_target_pan(cue: &Value) -> Option<f64> {
    let entries = pan_entries(cue);
    if entries.len() < 2 {
        return None;
    }
    let value = |e: &Value| e.get("endValue").and_then(Value::as_f64).unwrap_or(0.0);
    Some(pan_from_lr(value(entries[0]), value(entries[entries.len() - 1])))
}

/// Starting pan of a QLab pan fade.
///
/// QLab applies these at the fade's onset, which is how a cue "begins in the
/// left speaker" even though its own matrix is centred. Inkue's fade reads the
/// voice's *current* pan at GO, so this is lifted onto the target cue's initial
/// pan by [`collect_pan_starts`].
fn fade_start_pan(cue: &Value) -> Option<f64> {
    let entries = pan_entries(cue);
    if entries.len() < 2 {
        return None;
    }
    let value = |e: &Value| e.get("startValue").and_then(Value::as_f64).unwrap_or(0.0);
    Some(pan_from_lr(value(entries[0]), value(entries[entries.len() - 1])))
}

/// Walk the cue tree recording each pan fade's start pan against its target,
/// so an Audio cue can inherit the position its following pan fade begins at.
pub fn collect_pan_starts(cue: &Value, out: &mut std::collections::HashMap<String, f64>) {
    if cue.get("__class__").and_then(Value::as_str) == Some("FadeCue") {
        if let (Some(pan), Some(target)) = (fade_start_pan(cue), cue.get("cueTargetUniqueID")) {
            if let Some(id) = target.as_str().filter(|s| !s.is_empty()) {
                out.entry(normalise_id(id)).or_insert(pan);
            }
        }
    }
    for child in cue.get("cues").and_then(Value::as_array).unwrap_or(&Vec::new()) {
        collect_pan_starts(child, out);
    }
}

fn normalise_id(id: &str) -> String {
    uuid::Uuid::parse_str(id)
        .unwrap_or_else(|_| uuid::Uuid::new_v4())
        .to_string()
}

/// A QLab Fade cue. A pan-only fade must not move the level, hence
/// `fade_volume: false` when the master crosspoint is untouched.
pub fn fade(cue: &Value) -> Value {
    let mut out = common(cue, "fade");
    let target_db = fade_target_db(cue);
    let target_pan = fade_target_pan(cue);
    out.insert("target_cue_ids".into(), json!(target_ids(cue)));
    out.insert("target_cue_numbers".into(), json!(Vec::<String>::new()));
    out.insert("target_volume_db".into(), json!(target_db.unwrap_or(0.0)));
    let changes_opacity = cue.get("doOpacity").and_then(Value::as_bool) == Some(true);
    let absolute = cue.pointer("/geoFade/fadeType").and_then(Value::as_str).unwrap_or("absolute") == "absolute";
    let opacity = cue.get("opacity").and_then(Value::as_f64)
        .filter(|v| changes_opacity && absolute && (0.0..=1.0).contains(v));
    out.insert("target_brightness_pct".into(), json!(opacity.unwrap_or(1.0) * 100.0));
    out.insert("fade_brightness".into(), json!(opacity.is_some()));
    out.insert("target_pan".into(), json!(target_pan));
    out.insert("fade_volume".into(), json!(target_db.is_some()));
    out.insert("fade_duration_ms".into(), json!(ms_or_zero(cue, "duration")));
    // QLab's default shape. Reading upShape/downShape is still open.
    out.insert("fade_curve".into(), json!("s_curve"));
    out.insert(
        "stop_at_end".into(),
        json!(cue.get("stopTargetWhenDone").and_then(Value::as_bool).unwrap_or(false)),
    );
    if changes_opacity && opacity.is_none() {
        add_note(&mut out, "Recreate the relative or out-of-range QLab opacity fade");
    }
    let moves_image = ["doTranslation", "doScale", "doRotation", "doCrop", "doOrigin"]
        .iter().any(|key| cue.get(*key).and_then(Value::as_bool) == Some(true));
    if moves_image || (target_db.is_none() && target_pan.is_none() && !changes_opacity) {
        add_note(
            &mut out,
            "[Review QLab geometry and effects on this fade. Recreate animated position, \
             rotation, scale or crop against the original show]",
        );
    }
    Value::Object(out)
}

/// The pan an Audio cue should start at, given the pan-start table.
pub fn start_pan(cue: &Value, pan_starts: &std::collections::HashMap<String, f64>) -> f64 {
    pan_starts.get(&cue_id(cue)).copied().unwrap_or(0.0)
}

/// `F53Timecode` counts ticks of 1/48,000,000 s of **real** time.
const F53_TICKS_PER_SECOND: f64 = 48_000_000.0;

/// `F53Timecode` → Inkue `TcPosition` JSON.
///
/// At "video speed" (the 1000/1001 pull-down) the timecode clock runs slower
/// than real time, so real seconds must be divided by 1.001 to get the label:
/// 172,972,800,000 ticks = 3603.6 s real = exactly 01:00:00:00 at 29.97.
pub fn timecode_position(tc: Option<&Value>) -> Value {
    let Some(tc) = tc else { return default_position() };
    let framerate = tc.get("F53TimecodeFramerate");
    let fps = framerate
        .and_then(|f| f.get("F53FramerateFramesPerSecond"))
        .and_then(Value::as_i64)
        .unwrap_or(30)
        .max(1);
    let drop_frame = framerate
        .and_then(|f| f.get("F53FramerateDropFrame"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let video_speed = framerate
        .and_then(|f| f.get("F53FramerateVideoSpeed"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let Some(ticks) = tc.get("F53TimecodeCommonTicks").and_then(Value::as_f64) else {
        return default_position();
    };

    let mut seconds = ticks / F53_TICKS_PER_SECOND;
    if video_speed {
        seconds /= 1.001;
    }
    let total_frames = (seconds * fps as f64).round() as i64;
    let rate = match (fps, video_speed, drop_frame) {
        (30, true, true) => "29.97df",
        (30, true, false) => "29.97",
        (24, _, _) => "24",
        (25, _, _) => "25",
        _ => "30",
    };
    json!({
        "h": (total_frames / (3600 * fps)) % 24,
        "m": (total_frames / (60 * fps)) % 60,
        "s": (total_frames / fps) % 60,
        "f": total_frames % fps,
        "rate": rate,
    })
}

fn default_position() -> Value {
    json!({ "h": 0, "m": 0, "s": 0, "f": 0, "rate": "29.97df" })
}

pub fn timecode(cue: &Value, patches: &Patches) -> Value {
    let mut out = common(cue, "timecode");
    let start = timecode_position(cue.get("startTimecode"));
    let rate = start.get("rate").cloned().unwrap_or(json!("29.97df"));
    let port = patches.midi_port(cue.get("midiPatchID").and_then(Value::as_str));
    let is_ltc = cue.get("outputType").and_then(Value::as_i64).unwrap_or(0) != 0;
    out.insert("tc_type".into(), json!(if is_ltc { "ltc" } else { "mtc" }));
    out.insert("midi_port".into(), if port.is_empty() { Value::Null } else { json!(port) });
    out.insert("output_patch_id".into(), Value::Null);
    out.insert("rate".into(), rate);
    out.insert("start_frame".into(), start);
    out.insert("end_frame".into(), Value::Null);
    Value::Object(out)
}

/// A QLab type with no Inkue counterpart, kept as an explicitly named Memo.
pub fn unconvertible(cue: &Value, label: &str, detail: &str) -> Value {
    let text = if detail.is_empty() {
        format!("[{label}]")
    } else {
        format!("[{label}] {detail}")
    };
    let mut out = match memo(cue, &text) {
        Value::Object(map) => map,
        _ => unreachable!("memo always builds an object"),
    };
    let name = cue.get("name").and_then(Value::as_str).unwrap_or("");
    out.insert("name".into(), json!(format!("[{label}] {name}").trim().to_string()));
    if !detail.is_empty() {
        add_note(&mut out, detail);
    }
    Value::Object(out)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn continue_mode_maps_qlabs_integers() {
        assert_eq!(continue_mode(&json!({ "continueMode": 0 })), "do_not_continue");
        assert_eq!(continue_mode(&json!({ "continueMode": 1 })), "auto_continue");
        assert_eq!(continue_mode(&json!({ "continueMode": 2 })), "auto_follow");
        assert_eq!(continue_mode(&json!({})), "do_not_continue");
    }

    #[test]
    fn armed_becomes_not_disabled() {
        let out = common(&json!({ "armed": false }), "memo");
        assert_eq!(out["is_disabled"], json!(true));
        let out = common(&json!({ "armed": true }), "memo");
        assert_eq!(out["is_disabled"], json!(false));
    }

    #[test]
    fn a_qlab_unique_id_is_reused_as_the_cue_id() {
        // This is what makes every control cue's target resolve for free.
        let id = "68a9990a-7835-46ae-b9b8-2e231407d2fe";
        let cue = json!({ "uniqueID": id.to_uppercase() });
        assert_eq!(cue_id(&cue), id);
        assert_eq!(target_ids(&json!({ "cueTargetUniqueID": id })), vec![id]);
    }

    #[test]
    fn infinite_loop_becomes_the_sentinel() {
        assert_eq!(loop_count(&json!({ "infiniteLoop": true })), u32::MAX as u64);
        for flag in [json!(false), json!(0), json!(2), json!("1")] {
            assert_eq!(loop_count(&json!({ "infiniteLoop": flag, "playCount": 3 })), 2);
        }
        assert_eq!(loop_count(&json!({})), 0);
    }

    #[test]
    fn archived_integer_loop_keeps_audio_and_video_running() {
        let source = json!({ "infiniteLoop": 1, "playCount": 1 });
        assert_eq!(audio(&source, &Default::default())["loop_count"], json!(u32::MAX));
        assert_eq!(video(&source)["loop_count"], json!(u32::MAX));
    }

    #[test]
    fn master_level_reads_the_matrix_and_floors_silence() {
        let unity = json!({ "levels": { "entries": {
            "0": { "row": 0, "column": 0, "initialLevel": 1.0, "trimLevel": 1.0 } } } });
        assert_eq!(master_db(&unity), 0.0);

        // QLab's Mic cue really does default to silence at the master
        // crosspoint; importing it as 0 dB would put a live mic up at unity.
        let silent = json!({ "levels": { "entries": {
            "0": { "row": 0, "column": 0, "initialLevel": 0.0, "trimLevel": 1.0 } } } });
        assert_eq!(master_db(&silent), -60.0);

        assert_eq!(master_db(&json!({})), 0.0, "no matrix means unity");
    }

    #[test]
    fn trim_review_reports_the_source_controls_without_changing_the_folded_level() {
        let adjusted = json!({"levels":{"entries":[
            {"row":0,"column":0,"initialLevel":0.25118863582611084,"trimLevel":0.5623413324356079},
            {"row":0,"column":1,"initialLevel":1.0,"trimLevel":0.5}
        ]}});
        assert!((master_db(&adjusted) + 17.0).abs() < 1e-5);
        assert!(master_trim_review(&adjusted).unwrap().contains("main level -12.00 dB and main trim -5.00 dB"));

        let silence = json!({"levels":{"entries":{"main":{
            "row":0,"column":0,"initialLevel":0.0,"trimLevel":3.981071710586548
        }}}});
        assert_eq!(master_db(&silence), -60.0);
        assert!(master_trim_review(&silence).unwrap().contains("main level silence and main trim +12.00 dB"));

        let not_recorded = json!({"levels":{"entries":[{"row":0,"column":0,"trimLevel":0.5}]}});
        assert!(master_trim_review(&not_recorded).unwrap().contains("main level unavailable in archive"));
        for levels in [
            json!({"entries":[{"row":0,"column":0,"initialLevel":0.4,"trimLevel":1.0}]}),
            json!({"entries":[{"row":0,"column":0,"initialLevel":0.4}]}),
            json!({"entries":[{"row":0,"column":1,"initialLevel":0.4,"trimLevel":0.5}]}),
        ] {
            assert!(master_trim_review(&json!({"levels":levels})).is_none());
        }
    }

    #[test]
    fn a_qlab5_alias_gives_a_bundle_relative_path() {
        let cue = json!({ "fileTarget": { "relativePath": "audio/Intro.wav" } });
        assert_eq!(media_path(&cue).as_deref(), Some("audio/Intro.wav"));
    }

    #[test]
    fn a_qlab4_nsurl_gives_a_path_too() {
        let cue = json!({ "fileTarget": { "__class__": "NSURL", "relative": "file://video/A.mov" } });
        assert_eq!(media_path(&cue).as_deref(), Some("video/A.mov"));
    }

    #[test]
    fn an_absolute_bookmark_falls_back_to_the_media_folder() {
        let cue = json!({ "fileTarget": {
            "lastKnownPath": "/Users/x/Show.qlab5/audio/Cue 1.aif" } });
        assert_eq!(media_path(&cue).as_deref(), Some("audio/Cue 1.aif"));
    }

    #[test]
    fn a_script_cue_arrives_disarmed_with_its_source() {
        let cue = json!({ "source": "tell application \"QLab\"" });
        let out = script(&cue);
        assert_eq!(out["is_disabled"], json!(true));
        assert_eq!(out["command"], json!(""));
        assert!(out["notes"].as_str().unwrap().contains("tell application"));
    }

    #[test]
    fn an_osc_string_splits_into_address_and_typed_args() {
        let patches = Patches::default();
        let out = osc(&json!({ "oscString": "/cue/1/go 2 0.5 hello" }), &patches);
        let message = &out["messages"][0];
        assert_eq!(message["address"], json!("/cue/1/go"));
        assert_eq!(message["args"][0], json!({ "type": "int", "value": 2 }));
        assert_eq!(message["args"][1], json!({ "type": "float", "value": 0.5 }));
        assert_eq!(message["args"][2], json!({ "type": "str", "value": "hello" }));
    }

    #[test]
    fn raw_udp_payload_survives_import_for_review() {
        let out = osc(&json!({"plainTextString":"LX1\r"}), &Patches::default());
        assert_eq!(out["messages"][0]["raw"]["value"], "LX1\r");
        assert_eq!(out["is_disabled"], true);
        assert!(out["_conversion_note"].is_string());
    }

    #[test]
    fn an_empty_osc_string_yields_no_message() {
        let out = osc(&json!({ "oscString": "  " }), &Patches::default());
        assert_eq!(out["messages"], json!([]));
    }

    #[test]
    fn audio_and_video_effect_chains_require_review_before_arming() {
        let no_effects = audio(&json!({"effects":[]}), &std::collections::HashMap::new());
        assert_eq!(no_effects["is_disabled"], false);
        let source = json!({"effects":[{"__class__":"AudioEffect"}]});
        let filtered = audio(&source, &std::collections::HashMap::new());
        let soundtrack = video(&json!({"audioCue": source}));
        for imported in [filtered, soundtrack] {
            assert_eq!(imported["is_disabled"], true);
            assert!(imported["_conversion_note"].as_str().unwrap().contains("audio effect chain"));
            assert!(imported.get("audio_effects").is_none());
        }
    }

    #[test]
    fn audio_and_video_retain_archived_source_names_for_review() {
        let source = json!({"inputNames":{"3":"Radio"}});
        for imported in [audio(&source, &Default::default()), video(&json!({"audioCue":source}))] {
            assert!(imported["notes"].as_str().unwrap().contains(r#"{"3":"Radio"}"#));
            assert!(imported["_conversion_note"].as_str().unwrap().contains("channel order"));
            assert_eq!(imported["is_disabled"], false);
        }
    }

    #[test]
    fn a_midi_voice_message_maps_to_a_note() {
        let cue = json!({ "status": 1, "channel": 1, "byte1": 60, "byte2": 64 });
        let out = midi(&cue, &Patches::default());
        assert_eq!(out["is_disabled"], true);
        assert!(out["_conversion_note"].as_str().unwrap().contains("output port"));
        let message = &out["messages"][0];
        assert_eq!(message["message_type"], json!("note_on"));
        assert_eq!(message["data1"], json!(60));
        assert_eq!(message["data2"], json!(64));
    }

    #[test]
    fn midi_voice_import_encodes_pressure_and_pitch_in_wire_order() {
        use crate::cue::midi_cue::MidiMessage;
        for (fields, expected) in [
            (json!({"status":2,"byte1":64,"byte2":87}), vec![0xAF,64,87]),
            (json!({"status":5,"byte1":91}), vec![0xDF,91]),
            (json!({"status":6,"byteCombo":0}), vec![0xEF,0,0]),
            (json!({"status":6,"byteCombo":8192}), vec![0xEF,0,64]),
            (json!({"status":6,"byte1":3,"byte2":65}), vec![0xEF,3,65]),
            (json!({"status":6,"byteCombo":16383}), vec![0xEF,127,127]),
        ] {
            let mut cue = fields;
            cue["channel"] = json!(16);
            cue["messageType"] = json!(1);
            let out = midi(&cue, &Patches::default());
            let msg: MidiMessage = serde_json::from_value(out["messages"][0].clone()).unwrap();
            assert_eq!(msg.to_bytes().unwrap(), expected);
        }
    }

    #[test]
    fn qlab_msc_transport_is_exact_and_requires_operator_review() {
        use crate::cue::midi_cue::MidiMessage;
        let patches = Patches::from_settings(&json!({"MIDI":{"midiPatches":[{"data":{
            "uniqueID":"local","destinationName":"Monitor"}}]}}));
        for (code, command) in [(1,"go"), (2,"stop"), (3,"resume")] {
            let cue = json!({"messageType":2,"command":code,"commandFormat":1,"deviceID":7,
                "qNumber":"12.3","qList":"4","qPath":"5","midiPatchID":"local","status":1,"armed":true});
            let out = midi(&cue, &patches);
            assert_eq!(out["is_disabled"], true);
            assert!(out["_conversion_note"].is_string());
            assert_eq!(out["messages"][0]["msc"]["command"], command);
            let msg: MidiMessage = serde_json::from_value(out["messages"][0].clone()).unwrap();
            assert_eq!(msg.port_name, "Monitor");
            assert_eq!(msg.to_bytes().unwrap(), [0xf0,0x7f,7,2,1,code,49,50,46,51,0,52,0,53,0xf7]);
        }
    }

    #[test]
    fn invalid_or_broadcast_qlab_msc_retains_the_original_fields_without_output() {
        for changes in [json!({"deviceID":127}), json!({"deviceID":112}),
            json!({"deviceID":null}), json!({"commandFormat":0}), json!({"command":4}),
            json!({"qNumber":"A.1"}), json!({"qList":3}), json!({"qList":null,"qPath":"2"})] {
            let mut cue = json!({"messageType":2,"command":1,"commandFormat":16,"deviceID":1,"qNumber":"1"});
            cue.as_object_mut().unwrap().extend(changes.as_object().unwrap().clone());
            let out = midi(&cue, &Patches::default());
            assert_eq!(out["messages"], json!([]));
            assert_eq!(out["is_disabled"], true);
            assert!(out["notes"].as_str().unwrap().contains(&cue.to_string()));
        }
    }

    #[test]
    fn unconverted_midi_remains_disabled_with_source_fields_for_review() {
        for extra in [
            json!({"messageType":2,"command":37}), json!({"messageType":3,"rawString":"7E"}),
            json!({"status":21}), json!({"doFade":true,"endValue":120}),
            json!({"status":6,"byteCombo":16384}), json!({"channel":17}),
        ] {
            let mut cue = json!({"status":1,"channel":16,"byte1":64,"byte2":30});
            cue.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
            let out = midi(&cue, &Patches::default());
            assert_eq!(out["messages"], json!([]));
            assert_eq!(out["is_disabled"], true);
            assert!(out["_conversion_note"].is_string());
            assert!(out["notes"].as_str().unwrap().contains(&cue.to_string()));
        }
    }

    #[test]
    fn timecode_ticks_decode_through_the_pulldown() {
        // 172_972_800_000 ticks = 3603.6 s real = 01:00:00:00 at 29.97.
        let tc = json!({
            "F53TimecodeCommonTicks": 172_972_800_000i64,
            "F53TimecodeFramerate": {
                "F53FramerateFramesPerSecond": 30,
                "F53FramerateDropFrame": false,
                "F53FramerateVideoSpeed": true,
            },
        });
        let pos = timecode_position(Some(&tc));
        assert_eq!(pos, json!({ "h": 1, "m": 0, "s": 0, "f": 0, "rate": "29.97" }));
    }

    #[test]
    fn timecode_without_pulldown_is_read_at_face_value() {
        let tc = json!({
            "F53TimecodeCommonTicks": 48_000_000i64 * 90, // 90 s
            "F53TimecodeFramerate": {
                "F53FramerateFramesPerSecond": 25,
                "F53FramerateDropFrame": false,
                "F53FramerateVideoSpeed": false,
            },
        });
        let pos = timecode_position(Some(&tc));
        assert_eq!(pos, json!({ "h": 0, "m": 1, "s": 30, "f": 0, "rate": "25" }));
    }

    #[test]
    fn an_unconvertible_cue_names_itself_and_keeps_the_detail() {
        let out = unconvertible(&json!({ "name": "House" }), "QLab Light cue", "all = home");
        assert_eq!(out["cue_type"], json!("memo"));
        assert_eq!(out["name"], json!("[QLab Light cue] House"));
        assert!(out["memo_text"].as_str().unwrap().contains("all = home"));
    }

    #[test]
    fn wait_seconds_become_milliseconds() {
        assert_eq!(wait(&json!({ "duration": 1.5 }))["wait_duration_ms"], json!(1500));
    }
}
