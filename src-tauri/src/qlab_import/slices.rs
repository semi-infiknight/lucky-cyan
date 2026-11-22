//! QLab stores the play count for the segment ending at each marker.
use serde_json::{json, Map, Value};

use super::cues::add_note;
use crate::cue::types::{SliceList, PLAY_COUNT_INFINITE};

fn count(slice: &Value) -> Result<u32, &'static str> {
    if !slice.is_object() {
        return Err("unrecognized slice");
    }
    if let Some(infinite) = slice.get("infiniteLoop") {
        match infinite.as_bool() {
            Some(true) => return Ok(PLAY_COUNT_INFINITE),
            Some(false) => {}
            None => return Err("unrecognized infinite-loop flag"),
        }
    }
    let n = slice.get("playCount").map_or(Some(1), Value::as_u64);
    match n.and_then(|n| u32::try_from(n).ok()) {
        Some(0) => Err("a zero-play segment is skipped in QLab"),
        Some(n) if n < PLAY_COUNT_INFINITE => Ok(n),
        _ => Err("unrecognized finite play count"),
    }
}

fn read(cue: &Value) -> Result<Option<SliceList>, &'static str> {
    let tail = match cue.get("lastSlice").filter(|v| !v.is_null()) {
        Some(slice) => count(slice)?,
        None => 1,
    };
    let entries = match cue.get("slices").filter(|v| !v.is_null()) {
        Some(v) => v
            .as_array()
            .ok_or("unrecognized slice collection")?
            .as_slice(),
        None => &[],
    };
    if entries.is_empty() {
        return if tail == 1 {
            Ok(None)
        } else {
            Err("a repeated final segment has no slice marker")
        };
    }
    let mut plan = SliceList::default();
    for entry in entries {
        let seconds = entry["time"].as_f64().ok_or("missing marker time")?;
        let millis = (seconds * 1000.0).round();
        if !millis.is_finite() || millis <= 0.0 || millis >= u64::MAX as f64 {
            return Err("invalid marker time");
        }
        let millis = millis as u64;
        if plan
            .markers
            .last()
            .is_some_and(|previous| *previous >= millis)
        {
            return Err("markers overlap or are out of sequence at millisecond resolution");
        }
        plan.markers.push(millis);
        plan.play_counts.push(count(entry)?);
    }
    plan.play_counts.push(tail);
    Ok(Some(plan))
}

fn source(cue: &Value) -> Value {
    json!({"slices": cue.get("slices"), "lastSlice": cue.get("lastSlice")})
}

fn plan(cue: &Value, mapped: &Map<String, Value>) -> Result<Option<SliceList>, &'static str> {
    let outer = read(cue)?;
    let audio = cue.get("audioCue").map(read).transpose()?.flatten();
    if audio.is_some() && outer != audio {
        return Err("picture and soundtrack have different slice programs");
    }
    let Some(plan) = outer else {
        return Ok(None);
    };
    if mapped
        .get("loop_count")
        .and_then(Value::as_u64)
        .unwrap_or(0)
        != 0
    {
        return Err("whole-cue repeats are combined with slice repeats");
    }
    let start = mapped
        .get("start_time_ms")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let end = mapped
        .get("end_time_ms")
        .and_then(Value::as_u64)
        .unwrap_or(u64::MAX);
    if plan.markers.iter().any(|&m| m <= start || m >= end) {
        return Err("a marker falls outside the trimmed playback window");
    }
    Ok(Some(plan))
}

pub(super) fn apply(cue: &Value, mapped: &mut Map<String, Value>) {
    match plan(cue, mapped) {
        Ok(Some(slices)) => {
            mapped.insert("slices".into(), json!(slices));
        }
        Ok(None) => {}
        Err(reason) => {
            mapped.insert("is_disabled".into(), json!(true));
            let mut original = source(cue);
            if let Some(audio) = cue.get("audioCue") {
                original["audioCue"] = source(audio);
            }
            add_note(mapped, &format!(
                "[Slice program disarmed: {reason}. Compare these source times (seconds) and play counts in the clip editor. Rebuild and rehearse each hold and exit before arming. Source: {original}]"
            ));
        }
    }
}
