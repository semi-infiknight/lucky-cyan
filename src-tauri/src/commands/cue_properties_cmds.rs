//! Copy selected programming between existing compatible cues while transport is idle.

use std::{collections::HashSet, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::{Emitter, State};
use uuid::Uuid;

use crate::{
    cue::{
        curve::FadeShapes,
        decoded_audio::{apply_decoded_audio, collect_decoded_audio, DecodedAudio},
        point_mix::AudioMapPatch,
        registry::CueRegistry,
        traits::Cue,
        types::CueType,
    },
    engine::voice::{MATRIX_INPUTS, MATRIX_OUTPUTS},
    show::{cue_list::CueList, undo_stack::UndoHistory, Workspace},
    state::{AppState, ClipboardCue, ClipboardMatrixOrigin},
};

use super::undo_cmds::{playback_is_active, take_snapshot};

const COPY_SUPPORTED_CUE: &str = "Copy an Audio, Video, or Fade cue first.";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PropertyGroup {
    Color,
    Waits,
    AudioLevel,
    AudioMatrix,
    AudioFades,
    AudioClipWindow,
    Pan,
    Rate,
    PictureGeometry,
    PictureLayer,
    PictureFades,
    FadeDuration,
    FadeCurveShapes,
}

impl PropertyGroup {
    fn keys(self) -> &'static [&'static str] {
        match self {
            Self::Color => &["color"],
            Self::Waits => &["pre_wait_ms", "post_wait_ms"],
            Self::AudioLevel => &["volume_db"],
            Self::AudioMatrix => &["level_matrix", "matrix_mirror_mono"],
            Self::AudioFades => &[
                "fade_in_ms",
                "fade_in_curve",
                "fade_out_ms",
                "fade_out_curve",
            ],
            Self::AudioClipWindow => &["start_time_ms", "end_time_ms"],
            Self::Pan => &["pan"],
            Self::Rate => &["rate"],
            Self::PictureGeometry => &["geometry"],
            Self::PictureLayer => &["layer_style"],
            Self::PictureFades => &[
                "video_fade_in_ms",
                "video_fade_in_curve",
                "video_fade_out_ms",
                "video_fade_out_curve",
            ],
            Self::FadeDuration => &["fade_duration_ms"],
            Self::FadeCurveShapes => &["fade_shapes"],
        }
    }
}

#[derive(Serialize)]
pub struct PastePropertiesPreview {
    workspace_id: Uuid,
    cue_list_id: Uuid,
    clipboard_revision: Uuid,
    source_name: String,
    source_number: Option<String>,
    groups: Vec<PropertyGroup>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PastePropertiesRequest {
    workspace_id: Uuid,
    cue_list_id: Uuid,
    clipboard_revision: Uuid,
    targets: Vec<Uuid>,
    groups: Vec<PropertyGroup>,
}

/// Snapshot the logical columns and physical assignments visible at Copy or Paste.
pub(super) fn matrix_paste_patch(ws: &Workspace, cue: &dyn Cue) -> Option<ClipboardMatrixOrigin> {
    let id = cue.output_patch_id().or(ws.default_output_patch_id)?;
    let patch = ws.output_patches.iter().find(|patch| patch.id == id)?;
    let valid = !matches!(
        patch.device_id.as_str(),
        "" | "default" | "pipewire" | "pw:"
    ) && !patch.has_audio_effects()
        && (1..=MATRIX_OUTPUTS).contains(&patch.channels.len())
        && patch
            .channels
            .iter()
            .all(|&channel| usize::from(channel) < MATRIX_OUTPUTS);
    if !valid
        || (patch.require_named_device
            && (cue.cue_type() != CueType::Audio || !matches!(patch.named_output(), Ok(Some(_)))))
    {
        return None;
    }
    Some(ClipboardMatrixOrigin {
        workspace_id: ws.instance_id,
        patch: AudioMapPatch::from_output_patch(patch),
        require_named_device: patch.require_named_device,
    })
}

fn copied_matrix_layout<'a>(
    source: &'a ClipboardCue,
    ws: &Workspace,
) -> Option<(&'a ClipboardMatrixOrigin, u16)> {
    let origin = source.matrix_origin.as_ref()?;
    if origin.workspace_id != ws.instance_id
        || !source.json["point_mix"].is_null()
        || (origin.require_named_device && source.json["type"] != "audio")
    {
        return None;
    }
    let (_, inputs, _, _) = copied_audio(source)?;
    let width = usize::from(*inputs);
    if !(1..=MATRIX_INPUTS).contains(&width) {
        return None;
    }
    let mirror_mono = source.json["matrix_mirror_mono"].as_bool()?;
    let used_inputs = if width == 1 && mirror_mono { 2 } else { width };
    let rows = source.json["level_matrix"].as_array()?;
    let mut configured = false;
    if rows.len() > MATRIX_INPUTS {
        return None;
    }
    for (input, row) in rows.iter().enumerate() {
        let cells = row.as_array()?;
        configured |= !cells.is_empty();
        if cells.len() > origin.patch.channels.len()
            || cells.iter().any(|cell| {
                !cell.as_f64().is_some_and(|db| {
                    (-60.0..=12.0).contains(&db) && (input < used_inputs || db == -60.0)
                })
            })
        {
            return None;
        }
    }
    configured.then_some((origin, *inputs))
}

fn copied_audio(source: &ClipboardCue) -> Option<&DecodedAudio> {
    let id = source.json["id"].as_str()?.parse().ok()?;
    source.audio.get(&id)
}

#[derive(Clone, Copy)]
struct ClipWindow {
    start_ms: Option<u64>,
    end_ms: Option<u64>,
}

impl ClipWindow {
    fn copied(source: &ClipboardCue) -> Option<Self> {
        let marker = |key| match source.json.get(key)? {
            Value::Null => Some(None),
            marker => marker.as_u64().map(Some),
        };
        let window = Self {
            start_ms: marker("start_time_ms")?,
            end_ms: marker("end_time_ms")?,
        };
        let audio = copied_audio(source)?;
        window.fits(audio).then_some(window)
    }

    fn fits(self, (samples, channels, sample_rate, _): &DecodedAudio) -> bool {
        let Some(frames) = samples.len().checked_div(usize::from(*channels)) else {
            return false;
        };
        if *sample_rate == 0
            || frames == 0
            || [self.start_ms, self.end_ms]
                .into_iter()
                .flatten()
                .any(|ms| u128::from(ms) * u128::from(*sample_rate) > frames as u128 * 1000)
        {
            return false;
        }
        // Match the File Audio playback conversion as well as checking the
        // real bounds, so a sub-sample range cannot produce an empty cue.
        let frame_at =
            |ms| (Duration::from_millis(ms).as_secs_f64() * f64::from(*sample_rate)) as usize;
        let start = self.start_ms.map_or(0, frame_at);
        let end = self.end_ms.map_or(frames, frame_at).min(frames);
        start < end
    }
}

fn compatible_fade_groups(
    source: &ClipboardCue,
    list: &CueList,
    targets: &[Uuid],
) -> Result<Vec<PropertyGroup>, String> {
    let mut duration = source.json["fade_duration_ms"].as_u64();
    for id in targets {
        let cue = list
            .get_recursive(id)
            .ok_or("A selected cue is no longer in this cue list.")?;
        if cue.cue_type() != CueType::Fade {
            return Err("Select only Fade cues for this paste.".into());
        }
        if duration == Some(0)
            && cue.serialize()["translation_waypoints"]
                .as_array()
                .is_some_and(|points| !points.is_empty())
        {
            duration = None;
        }
    }

    let mut groups = Vec::new();
    if duration.is_some() {
        groups.push(PropertyGroup::FadeDuration);
    }
    if FadeShapes::deserialize(&source.json["fade_shapes"]).is_ok() {
        groups.push(PropertyGroup::FadeCurveShapes);
    }
    Ok(groups)
}

fn compatible_groups(
    source: &ClipboardCue,
    ws: &Workspace,
    list: &CueList,
    targets: &[Uuid],
) -> Result<Vec<PropertyGroup>, String> {
    use PropertyGroup::*;

    if targets.is_empty() || targets.iter().collect::<HashSet<_>>().len() != targets.len() {
        return Err("Select one or more different destination cues.".into());
    }
    let source_type = match source.json["type"].as_str() {
        Some("audio") => CueType::Audio,
        Some("video") => CueType::Video,
        Some("fade") => return compatible_fade_groups(source, list, targets),
        _ => return Err(COPY_SUPPORTED_CUE.into()),
    };
    let mut same_type = true;
    let mut matrix_layout = copied_matrix_layout(source, ws);
    let mut clip_window = if source_type == CueType::Audio {
        ClipWindow::copied(source)
    } else {
        None
    };
    for id in targets {
        let cue = list
            .get_recursive(id)
            .ok_or("A selected cue is no longer in this cue list.")?;
        let target_type = cue.cue_type();
        if !matches!(target_type, CueType::Audio | CueType::Video) {
            return Err("Select only Audio or Video cues for this paste.".into());
        }
        same_type &= target_type == source_type;
        matrix_layout = matrix_layout.filter(|(origin, inputs)| {
            cue.applied_point_mix().is_none()
                && matrix_paste_patch(ws, cue).as_ref() == Some(*origin)
                && cue
                    .extract_decoded_audio()
                    .is_some_and(|(_, width, _, _)| width == *inputs)
        });
        clip_window = clip_window.filter(|window| {
            target_type == CueType::Audio
                && !cue.uses_sliced_playback()
                && cue
                    .extract_decoded_audio()
                    .is_some_and(|audio| window.fits(&audio))
        });
    }
    let mut groups = vec![Color, Waits, AudioLevel, AudioFades];
    if matrix_layout.is_some() {
        groups.push(AudioMatrix);
    }
    if clip_window.is_some() {
        groups.push(AudioClipWindow);
    }
    if same_type {
        match source_type {
            CueType::Audio => groups.extend([Pan, Rate]),
            CueType::Video => groups.extend([PictureGeometry, PictureLayer, PictureFades]),
            _ => unreachable!("validated source type"),
        }
    }
    Ok(groups)
}

#[tauri::command]
pub fn preview_paste_cue_properties(
    workspace_id: Uuid,
    cue_list_id: Uuid,
    targets: Vec<Uuid>,
    state: State<'_, AppState>,
) -> Result<PastePropertiesPreview, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if workspace_id != ws.instance_id || cue_list_id != ws.active_cue_list_id {
        return Err("The workspace or cue list changed. Select the destination cues again.".into());
    }
    let clipboard = state.clipboard.lock().map_err(|e| e.to_string())?;
    let cue = clipboard.as_ref().ok_or(COPY_SUPPORTED_CUE)?;
    let list = ws.active_cue_list().ok_or("No active cue list.")?;
    Ok(PastePropertiesPreview {
        workspace_id: ws.instance_id,
        cue_list_id: list.id,
        clipboard_revision: cue.revision,
        source_name: cue.json["name"].as_str().unwrap_or("Copied cue").to_owned(),
        source_number: cue.json["number"].as_str().map(str::to_owned),
        groups: compatible_groups(cue, &ws, list, &targets)?,
    })
}

/// The caller holds the workspace and history locks through validation and commit.
pub fn apply_pasted_properties(
    ws: &mut Workspace,
    history: &mut UndoHistory,
    registry: &CueRegistry,
    source: &ClipboardCue,
    request: PastePropertiesRequest,
) -> Result<usize, String> {
    if request.workspace_id != ws.instance_id || request.cue_list_id != ws.active_cue_list_id {
        return Err("The workspace or cue list changed. Reopen Paste Cue Properties.".into());
    }
    if request.clipboard_revision != source.revision {
        return Err("The copied cue changed. Reopen Paste Cue Properties to review it.".into());
    }
    if playback_is_active(ws) {
        return Err("Finish or stop all running and paused cues, and unload any held outputs before pasting properties.".into());
    }
    if request.groups.is_empty() {
        return Ok(0);
    }
    let list = ws.active_cue_list().ok_or("No active cue list.")?;
    let allowed = compatible_groups(source, ws, list, &request.targets)?;
    if request.groups.contains(&PropertyGroup::AudioMatrix)
        && !allowed.contains(&PropertyGroup::AudioMatrix)
    {
        return Err("Crosspoint routing is no longer compatible. Review the output patch and its device requirement, the decoded channel counts and the point panner before copying again.".into());
    }
    if request.groups.contains(&PropertyGroup::AudioClipWindow)
        && !allowed.contains(&PropertyGroup::AudioClipWindow)
    {
        return Err("The clip window no longer fits the selection. Use loaded Audio files long enough for the copied markers and destinations without slices.".into());
    }
    if request.groups.contains(&PropertyGroup::FadeDuration)
        && !allowed.contains(&PropertyGroup::FadeDuration)
        && source.json["fade_duration_ms"].as_u64() == Some(0)
    {
        return Err("A Fade with position waypoints needs a duration greater than zero.".into());
    }
    if request.groups.iter().any(|group| !allowed.contains(group)) {
        return Err("A selected property is unavailable for this cue selection.".into());
    }
    let mut properties = serde_json::Map::new();
    for key in request.groups.iter().flat_map(|group| group.keys()) {
        let value = source
            .json
            .get(*key)
            .ok_or("The copied cue lacks a selected property.")?;
        properties.insert((*key).to_owned(), value.clone());
    }

    let list = ws.active_cue_list_mut().ok_or("No active cue list.")?;
    let mut staged = Vec::new();
    for id in request.targets {
        let original = list
            .get_recursive(&id)
            .ok_or("A selected cue disappeared.")?;
        let mut json = original.serialize();
        if properties
            .iter()
            .all(|(key, value)| json.get(key) == Some(value))
        {
            continue;
        }
        let fields = json.as_object_mut().ok_or("A cue could not be read.")?;
        fields.extend(properties.clone());
        let mut edited = registry
            .from_json(json)
            .map_err(|error| error.to_string())?;
        if edited.id() != id || edited.cue_type() != original.cue_type() {
            return Err("A property paste must retain each cue's identity and type.".into());
        }
        apply_decoded_audio(edited.as_mut(), &collect_decoded_audio(original));
        staged.push((id, edited));
    }
    let count = staged.len();
    if count != 0 {
        history.push_action(list.id, take_snapshot(list));
        for (id, cue) in staged {
            let replaced = list.replace_cue_recursive(&id, cue);
            debug_assert!(
                replaced,
                "the workspace lock keeps each staged destination present"
            );
        }
        ws.mark_modified();
    }
    Ok(count)
}

#[tauri::command]
pub fn paste_cue_properties(
    request: PastePropertiesRequest,
    state: State<'_, AppState>,
    handle: tauri::AppHandle,
) -> Result<usize, String> {
    // Hold registry → workspace → history → clipboard through the commit.
    let registry = state.registry.lock().map_err(|e| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let mut history = state.undo_stack.lock().map_err(|e| e.to_string())?;
    let clipboard = state.clipboard.lock().map_err(|e| e.to_string())?;
    let source = clipboard.as_ref().ok_or(COPY_SUPPORTED_CUE)?;
    let changed = apply_pasted_properties(&mut ws, &mut history, &registry, source, request)?;
    drop(clipboard);
    drop(history);
    drop(ws);
    drop(registry);
    if changed > 0 {
        let _ = handle.emit("workspace-modified", ());
    }
    Ok(changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        commands::undo_cmds::apply_history,
        cue::{
            audio_cue::AudioCueFactory,
            curve::{CurveKind, CurvePoint, CurveShape},
            fade_cue::{FadeCue, FadeCueFactory},
            video_cue::VideoCueFactory,
        },
        show::undo_stack::HistoryDirection,
    };
    use serde_json::json;

    fn copied_fade(cue: &FadeCue) -> ClipboardCue {
        ClipboardCue {
            flagged_cues: Default::default(),
            second_colors: Default::default(),
            revision: Uuid::new_v4(),
            matrix_origin: None,
            audio: Default::default(),
            json: cue.serialize(),
        }
    }

    #[test]
    fn fade_paste_copies_independent_shapes_and_duration_and_undoes_each_selection() {
        use PropertyGroup::*;

        let mut registry = CueRegistry::new();
        registry.register(CueType::Fade, Box::new(FadeCueFactory));
        let mut ws = Workspace::new("Practice");
        let workspace_id = ws.instance_id;
        let list = ws.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let mut copied = FadeCue::new();
        copied.fade_duration_ms = 4000;
        copied.shapes = FadeShapes {
            up: CurveShape {
                points: vec![CurvePoint::new(0.42, 0.29)],
                bends: vec![0.25, -0.22],
                ..CurveShape::of_kind(CurveKind::Linear)
            },
            down: CurveShape {
                intensity: 2.5,
                ..CurveShape::of_kind(CurveKind::Parametric)
            },
            mirrored: false,
        };
        copied.target_cue_ids = vec![Uuid::new_v4()];
        let source = copied_fade(&copied);

        let destinations = [800, 1700].map(|duration| {
            let cue = registry
                .from_json(json!({
                    "type": "fade",
                    "id": Uuid::new_v4(),
                    "name": format!("Fade for {duration} ms"),
                    "target_cue_ids": [Uuid::new_v4()],
                    "target_translation": { "x": 0.1, "y": -0.2 },
                    "translation_waypoints": [[0.0, 0.2]],
                    "target_volume_db": -21.5,
                    "stop_at_end": true,
                    "fade_duration_ms": duration,
                    "pre_wait_ms": 80,
                }))
                .unwrap();
            let original = (cue.id(), cue.serialize());
            list.push(cue);
            original
        });
        let targets: Vec<_> = destinations.iter().map(|(id, _)| *id).collect();
        let request = |group| PastePropertiesRequest {
            workspace_id,
            cue_list_id: list_id,
            clipboard_revision: source.revision,
            targets: targets.clone(),
            groups: vec![group],
        };
        let cue_json = |ws: &Workspace, id| {
            ws.active_cue_list()
                .unwrap()
                .get_recursive(&id)
                .unwrap()
                .serialize()
        };
        let mut history = UndoHistory::new();
        assert_eq!(
            compatible_groups(&source, &ws, ws.active_cue_list().unwrap(), &targets).unwrap(),
            vec![FadeDuration, FadeCurveShapes],
        );
        assert_eq!(
            apply_pasted_properties(
                &mut ws,
                &mut history,
                &registry,
                &source,
                request(FadeCurveShapes)
            )
            .unwrap(),
            2,
        );
        for (id, original) in &destinations {
            let mut expected = original.clone();
            expected["fade_shapes"] = source.json["fade_shapes"].clone();
            expected["fade_curve"] = source.json["fade_curve"].clone();
            assert_eq!(cue_json(&ws, *id), expected);
        }
        assert_eq!(
            apply_pasted_properties(
                &mut ws,
                &mut history,
                &registry,
                &source,
                request(FadeDuration)
            )
            .unwrap(),
            2,
        );
        for (id, _) in &destinations {
            let current = cue_json(&ws, *id);
            assert_eq!(current["fade_duration_ms"], 4000);
            assert_eq!(current["fade_shapes"], source.json["fade_shapes"]);
        }

        assert!(apply_history(&mut ws, &mut history, &registry, HistoryDirection::Undo).unwrap());
        for (id, original) in &destinations {
            let current = cue_json(&ws, *id);
            assert_eq!(current["fade_duration_ms"], original["fade_duration_ms"]);
            assert_eq!(current["fade_shapes"], source.json["fade_shapes"]);
        }
        assert!(apply_history(&mut ws, &mut history, &registry, HistoryDirection::Undo).unwrap());
        assert!(!history.can_undo(list_id));
        for (id, original) in &destinations {
            assert_eq!(cue_json(&ws, *id), *original);
        }
    }

    #[test]
    fn fade_paste_revalidates_zero_duration_across_the_whole_selection() {
        use PropertyGroup::*;

        let mut registry = CueRegistry::new();
        registry.register(CueType::Fade, Box::new(FadeCueFactory));
        registry.register(CueType::Audio, Box::new(AudioCueFactory));
        let mut ws = Workspace::new("Practice");
        let workspace_id = ws.instance_id;
        let list = ws.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let first = FadeCue::new();
        let first_json = first.serialize();
        let first_id = first.id();
        list.push(Box::new(first));
        let second = FadeCue::new();
        let second_id = second.id();
        let mut second_json = second.serialize();
        list.push(Box::new(second));
        let other = registry
            .from_json(json!({"type": "audio", "id": Uuid::new_v4()}))
            .unwrap();
        let other_id = other.id();
        list.push(other);
        let mut copied = FadeCue::new();
        copied.fade_duration_ms = 0;
        let source = copied_fade(&copied);
        let targets = vec![first_id, second_id];

        let list = ws.active_cue_list().unwrap();
        assert_eq!(
            compatible_groups(&source, &ws, list, &targets).unwrap(),
            vec![FadeDuration, FadeCurveShapes]
        );
        assert!(compatible_groups(&source, &ws, list, &[first_id, other_id]).is_err());
        second_json["target_translation"] = json!({"x": 0.4, "y": 0.2});
        second_json["translation_waypoints"] = json!([[0.3, 0.1]]);
        assert!(ws
            .active_cue_list_mut()
            .unwrap()
            .replace_cue_recursive(&second_id, registry.from_json(second_json).unwrap(),));
        let list = ws.active_cue_list().unwrap();
        assert_eq!(
            compatible_groups(&source, &ws, list, &targets).unwrap(),
            vec![FadeCurveShapes]
        );

        let mut history = UndoHistory::new();
        let error = apply_pasted_properties(
            &mut ws,
            &mut history,
            &registry,
            &source,
            PastePropertiesRequest {
                workspace_id,
                cue_list_id: list_id,
                clipboard_revision: source.revision,
                targets,
                groups: vec![FadeDuration],
            },
        )
        .unwrap_err();
        assert!(error.contains("position waypoints"));
        assert!(!history.can_undo(list_id));
        assert_eq!(
            ws.active_cue_list()
                .unwrap()
                .get_recursive(&first_id)
                .unwrap()
                .serialize(),
            first_json
        );
    }

    #[test]
    fn audio_clip_window_requires_a_nonempty_range_inside_decoded_samples() {
        let audio = (
            std::sync::Arc::new(vec![0.0; 441]),
            1,
            44_100,
            Duration::from_secs(10),
        );
        let window = |start_ms, end_ms| ClipWindow { start_ms, end_ms };
        assert!(window(None, None).fits(&audio));
        assert!(window(Some(9), Some(10)).fits(&audio));
        assert!(!window(Some(9), Some(11)).fits(&audio));
        assert!(!window(Some(10), None).fits(&audio));
        assert!(!window(Some(9), Some(9)).fits(&audio));
        assert!(!window(Some(u64::MAX), None).fits(&audio));
    }

    #[test]
    fn audio_clip_paste_rechecks_media_and_slices_and_undoes_only_the_window() {
        use std::sync::Arc;

        let mut registry = CueRegistry::new();
        registry.register(CueType::Audio, Box::new(AudioCueFactory));
        let mut ws = Workspace::new("Practice");
        let workspace_id = ws.instance_id;
        let id = Uuid::new_v4();
        let source_id = Uuid::new_v4();
        let patch_id = Uuid::new_v4();
        let original = json!({"type":"audio", "id":id, "file_path":"different.wav", "volume_db":-8.0,
            "output_patch_id":patch_id, "loop_count":4, "start_time_ms":50});
        let frames = Arc::new(vec![0.0; 8_000]);
        let source = ClipboardCue {
            flagged_cues: Default::default(),
            second_colors: Default::default(),
            revision: Uuid::new_v4(),
            matrix_origin: None,
            json: json!({"type":"audio", "id":source_id, "file_path":"source.wav", "volume_db":-20.0,
                        "start_time_ms":200, "end_time_ms":800}),
            audio: [(
                source_id,
                (Arc::clone(&frames), 1, 8000, Duration::from_secs(1)),
            )]
            .into(),
        };
        let cue_with_audio = |json, samples| {
            let mut cue = registry.from_json(json).unwrap();
            cue.accept_preloaded_audio(samples, 1, 8000, Duration::from_secs(1));
            cue
        };
        let list = ws.active_cue_list_mut().unwrap();
        let list_id = list.id;
        list.push(cue_with_audio(original.clone(), Arc::clone(&frames)));
        let preview = |ws: &Workspace| {
            compatible_groups(&source, ws, ws.active_cue_list().unwrap(), &[id]).unwrap()
        };
        let request = |groups| PastePropertiesRequest {
            workspace_id,
            cue_list_id: list_id,
            clipboard_revision: source.revision,
            targets: vec![id],
            groups,
        };
        let replace = |ws: &mut Workspace, json, samples| {
            assert!(ws
                .active_cue_list_mut()
                .unwrap()
                .replace_cue_recursive(&id, cue_with_audio(json, samples)));
        };
        let mut history = UndoHistory::new();
        assert!(preview(&ws).contains(&PropertyGroup::AudioClipWindow));
        replace(&mut ws, original.clone(), Arc::new(vec![0.0; 4_000]));
        assert!(apply_pasted_properties(
            &mut ws,
            &mut history,
            &registry,
            &source,
            request(vec![
                PropertyGroup::AudioClipWindow,
                PropertyGroup::AudioLevel
            ])
        )
        .is_err());
        assert!(!history.can_undo(list_id));
        assert_eq!(
            ws.active_cue_list()
                .unwrap()
                .get_recursive(&id)
                .unwrap()
                .serialize()["volume_db"],
            -8.0
        );

        let mut sliced = original.clone();
        sliced["slices"] = json!({"markers":[400], "play_counts":[1,2]});
        replace(&mut ws, sliced, Arc::clone(&frames));
        assert!(!preview(&ws).contains(&PropertyGroup::AudioClipWindow));
        replace(&mut ws, original, Arc::clone(&frames));
        assert_eq!(
            apply_pasted_properties(
                &mut ws,
                &mut history,
                &registry,
                &source,
                request(vec![PropertyGroup::AudioClipWindow])
            )
            .unwrap(),
            1
        );
        let cue = ws.active_cue_list().unwrap().get_recursive(&id).unwrap();
        assert!(Arc::ptr_eq(
            &cue.extract_decoded_audio().unwrap().0,
            &frames
        ));
        let pasted = cue.serialize();
        assert_eq!(pasted["start_time_ms"], 200);
        assert_eq!(pasted["end_time_ms"], 800);
        assert_eq!(pasted["loop_count"], 4);
        assert_eq!(pasted["file_path"], "different.wav");
        assert_eq!(pasted["output_patch_id"], json!(patch_id));
        assert!(apply_history(&mut ws, &mut history, &registry, HistoryDirection::Undo).unwrap());
        let restored = ws
            .active_cue_list()
            .unwrap()
            .get_recursive(&id)
            .unwrap()
            .serialize();
        assert_eq!(restored["start_time_ms"], 50);
        assert!(restored["end_time_ms"].is_null());
    }

    #[test]
    fn mixed_paste_preserves_destination_routes_and_creates_one_undo_step() {
        let mut registry = CueRegistry::new();
        registry.register(CueType::Audio, Box::new(AudioCueFactory));
        registry.register(CueType::Video, Box::new(VideoCueFactory));
        let mut ws = Workspace::new("Practice");
        let list = ws.active_cue_list_mut().unwrap();
        let audio_id = Uuid::new_v4();
        let video_id = Uuid::new_v4();
        let route = Uuid::new_v4();
        let output = Uuid::new_v4();
        list.push(registry.from_json(json!({"type":"audio", "id":audio_id, "number":"SC1", "file_path":"door.wav", "output_patch_id":route})).unwrap());
        list.push(registry.from_json(json!({"type":"video", "id":video_id, "name":"Lobby", "file_path":"lobby.mp4", "output_id":output})).unwrap());
        let list_id = list.id;
        let source = ClipboardCue {
            flagged_cues: Default::default(),
            second_colors: Default::default(),
            revision: Uuid::new_v4(),
            matrix_origin: None,
            audio: Default::default(),
            json: json!({
                "type":"audio", "file_path":"source.wav", "pre_wait_ms":400, "post_wait_ms":225,
                "volume_db":-12.0, "pan":0.7, "output_patch_id":Uuid::new_v4(),
            }),
        };
        let workspace_id = ws.instance_id;
        let request = || PastePropertiesRequest {
            workspace_id,
            cue_list_id: list_id,
            clipboard_revision: source.revision,
            targets: vec![audio_id, video_id],
            groups: vec![PropertyGroup::Waits, PropertyGroup::AudioLevel],
        };
        let mut history = UndoHistory::new();

        let mut wrong_source = request();
        wrong_source.clipboard_revision = Uuid::new_v4();
        assert!(
            apply_pasted_properties(&mut ws, &mut history, &registry, &source, wrong_source)
                .is_err()
        );
        let mut wrong_group = request();
        wrong_group.groups.push(PropertyGroup::Pan);
        assert!(
            apply_pasted_properties(&mut ws, &mut history, &registry, &source, wrong_group)
                .is_err()
        );
        assert!(!history.can_undo(list_id));

        assert_eq!(
            apply_pasted_properties(&mut ws, &mut history, &registry, &source, request()).unwrap(),
            2
        );
        let list = ws.active_cue_list().unwrap();
        let audio = list.get_recursive(&audio_id).unwrap().serialize();
        let video = list.get_recursive(&video_id).unwrap().serialize();
        assert_eq!(audio["output_patch_id"], json!(route));
        assert_eq!(audio["file_path"], "door.wav");
        assert_eq!(audio["number"], "SC1");
        assert_eq!(audio["pan"], 0.0);
        assert_eq!(video["output_id"], json!(output));
        assert_eq!(video["file_path"], "lobby.mp4");
        assert_eq!(video["name"], "Lobby");
        for cue in [audio, video] {
            assert_eq!(cue["volume_db"], -12.0);
            assert_eq!(cue["pre_wait_ms"], 400);
            assert_eq!(cue["post_wait_ms"], 225);
        }
        assert_eq!(
            apply_pasted_properties(&mut ws, &mut history, &registry, &source, request()).unwrap(),
            0
        );
        assert!(apply_history(&mut ws, &mut history, &registry, HistoryDirection::Undo).unwrap());
        assert!(!history.can_undo(list_id));
        assert_eq!(
            ws.active_cue_list()
                .unwrap()
                .get_recursive(&video_id)
                .unwrap()
                .serialize()["pre_wait_ms"],
            0
        );
    }

    #[test]
    fn failure_to_stage_the_second_target_leaves_the_first_and_undo_untouched() {
        use crate::cue::traits::Cue;
        let mut registry = CueRegistry::new();
        registry.register(CueType::Audio, Box::new(AudioCueFactory));
        let first = registry.create(&CueType::Audio).unwrap();
        let second = crate::cue::video_cue::VideoCue::new();
        let targets = vec![first.id(), second.id()];
        let mut ws = Workspace::new("Practice");
        let list = ws.active_cue_list_mut().unwrap();
        let list_id = list.id;
        let original = first.serialize();
        list.push(first);
        list.push(Box::new(second));
        let source = ClipboardCue {
            flagged_cues: Default::default(),
            second_colors: Default::default(),
            revision: Uuid::new_v4(),
            matrix_origin: None,
            audio: Default::default(),
            json: json!({"type":"audio", "volume_db":-12.0}),
        };
        let request = PastePropertiesRequest {
            workspace_id: ws.instance_id,
            cue_list_id: list_id,
            clipboard_revision: source.revision,
            targets: targets.clone(),
            groups: vec![PropertyGroup::AudioLevel],
        };
        let mut history = UndoHistory::new();
        assert!(
            apply_pasted_properties(&mut ws, &mut history, &registry, &source, request).is_err()
        );
        assert!(!history.can_undo(list_id));
        assert_eq!(
            ws.active_cue_list()
                .unwrap()
                .get_recursive(&targets[0])
                .unwrap()
                .serialize(),
            original
        );
    }

    #[test]
    fn crosspoints_preserve_destination_media_and_refuse_reordered_channels() {
        use crate::engine::device_manager::OutputPatch;
        use std::{sync::Arc, time::Duration};

        let mut registry = CueRegistry::new();
        registry.register(CueType::Video, Box::new(VideoCueFactory));
        let mut ws = Workspace::new("Practice");
        let patch = OutputPatch::new("Booth", "pw:booth", vec![2, 0]);
        ws.default_output_patch_id = Some(patch.id);
        ws.output_patches.push(patch.clone());
        let workspace_id = ws.instance_id;
        let input = Uuid::new_v4();
        let destination = Uuid::new_v4();
        let samples = Arc::new(vec![0.0; 16]);
        let source = ClipboardCue {
            flagged_cues: Default::default(),
            second_colors: Default::default(),
            revision: Uuid::new_v4(),
            matrix_origin: Some(ClipboardMatrixOrigin {
                workspace_id,
                patch: AudioMapPatch::from_output_patch(&patch),
                require_named_device: false,
            }),
            audio: [(
                input,
                (Arc::clone(&samples), 2, 48_000, Duration::from_millis(50)),
            )]
            .into(),
            json: json!({"type":"audio", "id":input, "volume_db":-20.0,
                "level_matrix":[[0.0,-60.0],[-60.0,-7.0]], "matrix_mirror_mono":false}),
        };
        let mut target = registry
            .from_json(json!({"type":"video", "id":destination,
            "file_path":"own.mp4", "volume_db":-8.0}))
            .unwrap();
        target.accept_preloaded_audio(Arc::clone(&samples), 2, 48_000, Duration::from_millis(50));
        target.set_runtime_duration(Duration::from_millis(50));
        let list = ws.active_cue_list_mut().unwrap();
        let list_id = list.id;
        list.push(target);

        let request = |groups| PastePropertiesRequest {
            workspace_id,
            cue_list_id: list_id,
            clipboard_revision: source.revision,
            targets: vec![destination],
            groups,
        };
        let preview = |ws: &Workspace| {
            compatible_groups(&source, ws, ws.active_cue_list().unwrap(), &[destination]).unwrap()
        };
        assert!(preview(&ws).contains(&PropertyGroup::AudioMatrix));
        ws.output_patches[0].channels.reverse();
        assert!(!preview(&ws).contains(&PropertyGroup::AudioMatrix));
        let mut history = UndoHistory::new();
        assert!(apply_pasted_properties(
            &mut ws,
            &mut history,
            &registry,
            &source,
            request(vec![PropertyGroup::AudioMatrix, PropertyGroup::AudioLevel])
        )
        .is_err());
        assert!(!history.can_undo(list_id));
        let original = ws
            .active_cue_list()
            .unwrap()
            .get_recursive(&destination)
            .unwrap()
            .serialize();
        assert_eq!(original["volume_db"], -8.0);
        assert!(original["level_matrix"].is_null());

        ws.output_patches[0].channels.reverse();
        ws.active_cue_list_mut()
            .unwrap()
            .get_mut_recursive(&destination)
            .unwrap()
            .accept_preloaded_audio(Arc::clone(&samples), 1, 48_000, Duration::from_millis(50));
        assert!(!preview(&ws).contains(&PropertyGroup::AudioMatrix));
        ws.active_cue_list_mut()
            .unwrap()
            .get_mut_recursive(&destination)
            .unwrap()
            .accept_preloaded_audio(Arc::clone(&samples), 2, 48_000, Duration::from_millis(50));
        assert_eq!(
            apply_pasted_properties(
                &mut ws,
                &mut history,
                &registry,
                &source,
                request(vec![PropertyGroup::AudioMatrix])
            )
            .unwrap(),
            1
        );
        let cue = ws
            .active_cue_list()
            .unwrap()
            .get_recursive(&destination)
            .unwrap();
        assert!(Arc::ptr_eq(
            &cue.extract_decoded_audio().unwrap().0,
            &samples
        ));
        let pasted = cue.serialize();
        assert_eq!(pasted["level_matrix"], source.json["level_matrix"]);
        assert_eq!(pasted["matrix_mirror_mono"], false);
        assert_eq!(pasted["volume_db"], -8.0);
        assert_eq!(pasted["file_path"], "own.mp4");
        assert!(
            pasted["output_patch_id"].is_null(),
            "the destination still uses its workspace default"
        );
        assert!(apply_history(&mut ws, &mut history, &registry, HistoryDirection::Undo).unwrap());
        assert!(!history.can_undo(list_id));
    }

    #[test]
    fn crosspoint_copy_pins_device_requirement_and_excludes_video_from_strict_routes() {
        use crate::engine::device_manager::OutputPatch;
        use std::{sync::Arc, time::Duration};

        let mut registry = CueRegistry::new();
        registry.register(CueType::Audio, Box::new(AudioCueFactory));
        registry.register(CueType::Video, Box::new(VideoCueFactory));
        let mut ws = Workspace::new("Practice");
        let mut patch = OutputPatch::new("Booth", "hw:CARD=Booth,DEV=0", vec![2, 0]);
        patch.require_named_device = true;
        ws.default_output_patch_id = Some(patch.id);
        ws.output_patches.push(patch);
        let workspace_id = ws.instance_id;
        let samples = Arc::new(vec![0.0; 16]);
        let mut copied = registry
            .from_json(json!({"type":"audio", "id":Uuid::new_v4(),
            "volume_db":-20.0, "level_matrix":[[0.0,-60.0],[-60.0,-7.0]],
            "matrix_mirror_mono":false}))
            .unwrap();
        copied.accept_preloaded_audio(Arc::clone(&samples), 2, 48_000, Duration::from_millis(50));
        let clipboard = ClipboardCue {
            flagged_cues: Default::default(),
            second_colors: Default::default(),
            revision: Uuid::new_v4(),
            matrix_origin: matrix_paste_patch(&ws, copied.as_ref()),
            json: copied.serialize(),
            audio: collect_decoded_audio(copied.as_ref()),
        };
        assert!(
            clipboard
                .matrix_origin
                .as_ref()
                .unwrap()
                .require_named_device
        );

        let mut audio = registry.create(&CueType::Audio).unwrap();
        audio.accept_preloaded_audio(Arc::clone(&samples), 2, 48_000, Duration::from_millis(50));
        let mut video = registry.create(&CueType::Video).unwrap();
        video.accept_preloaded_audio(samples, 2, 48_000, Duration::from_millis(50));
        video.set_runtime_duration(Duration::from_millis(50));
        assert!(matrix_paste_patch(&ws, video.as_ref()).is_none());
        let audio_id = audio.id();
        let video_id = video.id();
        let list = ws.active_cue_list_mut().unwrap();
        let list_id = list.id;
        list.push(audio);
        list.push(video);
        let offered = |ws: &Workspace, targets: &[Uuid]| {
            compatible_groups(&clipboard, ws, ws.active_cue_list().unwrap(), targets)
                .unwrap()
                .contains(&PropertyGroup::AudioMatrix)
        };
        assert!(offered(&ws, &[audio_id]));
        assert!(!offered(&ws, &[audio_id, video_id]));
        ws.output_patches[0].require_named_device = false;
        assert!(!offered(&ws, &[audio_id]));
        let mut history = UndoHistory::new();
        let request = PastePropertiesRequest {
            workspace_id,
            cue_list_id: list_id,
            clipboard_revision: clipboard.revision,
            targets: vec![audio_id],
            groups: vec![PropertyGroup::AudioLevel, PropertyGroup::AudioMatrix],
        };
        assert!(
            apply_pasted_properties(&mut ws, &mut history, &registry, &clipboard, request).is_err()
        );
        assert!(!history.can_undo(list_id));
        let original = ws
            .active_cue_list()
            .unwrap()
            .get_recursive(&audio_id)
            .unwrap()
            .serialize();
        assert!(original["level_matrix"].is_null());
        assert_eq!(original["volume_db"], 0.0);
    }
}
