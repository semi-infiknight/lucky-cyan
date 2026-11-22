//! Video outputs — the windows a show's visual cues play on.
//!
//! The **main output** is configured by Preferences → Display (screen,
//! alignment) and always exists.  A show can add **extra outputs** (a façade
//! projector, a return monitor…), stored in the workspace; each Video / Image /
//! Camera / Text cue picks one by id (none = main).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use uuid::Uuid;

use crate::cue::traits::Cue;
use crate::engine::output_engine::confidence::{PreviewOwner, StageSnapshot, CAPTURE_TIMEOUT};
use crate::engine::output_engine::{
    OutputConfig, OutputId, OutputRegions, OutputSplitDirection, OutputTransform, MAIN_OUTPUT,
};
use crate::show::Workspace;
use crate::state::AppState;

/// One output as the UI lists it: the main output first, then the extras.
#[derive(Debug, Clone, Serialize)]
pub struct VideoOutputInfo {
    /// Output id; the main output's is the nil UUID.
    pub id: String,
    pub name: String,
    pub is_main: bool,
    /// Monitor index (0 = primary); `None` = a floating window.
    pub screen: Option<u32>,
    pub transform: OutputTransform,
}

/// Parse the optional output id a command receives.  Absent, empty and the
/// nil UUID all mean the main output (`None`).
pub(crate) fn parse_output_id(id: Option<&str>) -> Result<Option<OutputId>, String> {
    match id.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => {
            let id: Uuid = s.parse().map_err(|e: uuid::Error| format!("Invalid output id: {e}"))?;
            Ok((id != MAIN_OUTPUT).then_some(id))
        }
    }
}

fn list(ws: &Workspace) -> Vec<VideoOutputInfo> {
    let mut all = vec![VideoOutputInfo {
        id: MAIN_OUTPUT.to_string(),
        name: "Main".to_owned(),
        is_main: true,
        screen: ws.preferences.display.output_screen,
        transform: ws.preferences.display.output_transform,
    }];
    all.extend(ws.video_outputs.iter().map(|c| VideoOutputInfo {
        id: c.id.to_string(),
        name: c.name.clone(),
        is_main: false,
        screen: c.screen,
        transform: c.transform,
    }));
    all
}

/// Push the workspace's outputs to the engine and tell the UI.
fn publish(state: &AppState, app_handle: &tauri::AppHandle) {
    if let Ok(ws) = state.workspace.lock() {
        state.output_engine.sync_outputs_config(&ws.outputs_config());
    }
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
}

fn unknown_output() -> String {
    "Output not found".to_owned()
}

/// The main output followed by the show's extra outputs.
#[tauri::command]
pub fn list_video_outputs(state: State<'_, AppState>) -> Result<Vec<VideoOutputInfo>, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    Ok(list(&ws))
}

fn check_capture_owner(ws: &Workspace, workspace_id: Uuid, output_id: OutputId) -> Result<(), String> {
    if ws.instance_id != workspace_id {
        return Err("The open workspace changed".into());
    }
    if output_id != MAIN_OUTPUT && !ws.video_outputs.iter().any(|output| output.id == output_id) {
        return Err(unknown_output());
    }
    Ok(())
}

/// Read a single finished native output frame. Leave the output route, visibility
/// and transport in the state chosen by the operator.
#[tauri::command]
pub(crate) async fn capture_stage_snapshot(
    workspace_id: Uuid, output_id: OutputId, state: State<'_, AppState>,
) -> Result<StageSnapshot, String> {
    let receiver = {
        let ws = state.workspace.lock().map_err(|error| error.to_string())?;
        check_capture_owner(&ws, workspace_id, output_id)?;
        state.output_engine.request_stage_capture(output_id)?
    };
    let shot = tauri::async_runtime::spawn_blocking(move || {
        let frame = receiver.recv_timeout(CAPTURE_TIMEOUT)
            .map_err(|_| "No stage frame arrived. Check that this output is showing.".to_owned())??;
        frame.encode()
    }).await.map_err(|error| error.to_string())??;
    let ws = state.workspace.lock().map_err(|error| error.to_string())?;
    check_capture_owner(&ws, workspace_id, output_id)?;
    Ok(shot)
}

#[tauri::command]
pub(crate) fn set_stage_preview(
    workspace_id: Uuid, output_id: OutputId, session_id: Uuid, enabled: bool, state: State<'_, AppState>,
) -> Result<(), String> {
    let owner = PreviewOwner { workspace: workspace_id, session: session_id };
    if enabled {
        let ws = state.workspace.lock().map_err(|error| error.to_string())?;
        check_capture_owner(&ws, workspace_id, output_id)?;
        state.output_engine.start_stage_preview(output_id, owner)
    } else {
        state.output_engine.stop_stage_preview(output_id, owner);
        Ok(())
    }
}

#[tauri::command]
pub(crate) async fn read_stage_preview(
    workspace_id: Uuid, output_id: OutputId, session_id: Uuid, after: u32, state: State<'_, AppState>,
) -> Result<tauri::ipc::Response, String> {
    let owner = PreviewOwner { workspace: workspace_id, session: session_id };
    let frame = {
        let ws = match state.workspace.try_lock() {
            Ok(workspace) => workspace,
            Err(std::sync::TryLockError::WouldBlock) => return Ok(tauri::ipc::Response::new(Vec::new())),
            Err(error) => return Err(error.to_string()),
        };
        check_capture_owner(&ws, workspace_id, output_id)?;
        state.output_engine.read_stage_preview(output_id, owner, after)?
    };
    let bytes = frame.map(|(serial, frame)| frame.preview_bytes(serial)).unwrap_or_default();
    Ok(tauri::ipc::Response::new(bytes))
}

#[derive(Serialize)]
pub struct OutputSplitSetup {
    document: Uuid,
    outputs: Vec<VideoOutputInfo>,
}

#[tauri::command]
pub fn get_output_split_setup(state: State<'_, AppState>) -> Result<OutputSplitSetup, String> {
    let ws = state.workspace.lock().map_err(|err| err.to_string())?;
    Ok(OutputSplitSetup { document: ws.instance_id, outputs: list(&ws) })
}

#[derive(Deserialize)]
pub struct OutputSplitSide {
    id: OutputId,
    expected_regions: OutputRegions,
}

/// Map two overlapping parts of a picture into two whole projector rasters.
/// Both sides are checked and saved under the same workspace lock.
#[tauri::command]
pub fn apply_output_split(
    document: Uuid,
    sides: [OutputSplitSide; 2],
    direction: OutputSplitDirection,
    overlap: u16,
    seam_gamma: Option<u16>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<Vec<VideoOutputInfo>, String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let changed = save_output_split(&mut ws, document, sides, direction, overlap, seam_gamma)?;
    let outputs = list(&ws);
    if changed {
        state.output_engine.sync_outputs_config(&ws.outputs_config());
        drop(ws);
        let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    }
    Ok(outputs)
}

fn save_output_split(
    ws: &mut Workspace,
    document: Uuid,
    sides: [OutputSplitSide; 2],
    direction: OutputSplitDirection,
    overlap: u16,
    seam_gamma: Option<u16>,
) -> Result<bool, String> {
    if ws.instance_id != document {
        return Err("Workspace changed. Reopen the paired projection editor.".into());
    }
    if sides[0].id == sides[1].id {
        return Err("Choose two different outputs".into());
    }
    let layouts = match seam_gamma {
        Some(gamma) => direction.feathered_regions(overlap, gamma, sides.each_ref().map(|side| side.id))?,
        None => direction.regions(overlap)?,
    };
    let outputs = list(ws);
    let lookup = |id: OutputId| {
        let id = id.to_string();
        outputs.iter().find(|output| output.id == id)
    };
    let first = lookup(sides[0].id).ok_or_else(unknown_output)?;
    let second = lookup(sides[1].id).ok_or_else(unknown_output)?;
    if first.screen.is_some() && first.screen == second.screen {
        return Err("Assign the outputs to different screens or use floating windows".into());
    }
    let current = [first.transform.regions, second.transform.regions];
    if current != sides.each_ref().map(|side| side.expected_regions) {
        return Err("A region layout changed. Reload the paired projection editor.".into());
    }
    if current == layouts {
        return Ok(false);
    }
    if ws.cue_lists.iter().any(|list| list.cues.iter().any(|cue| active_visual(cue.as_ref()))) {
        return Err("Stop the active visual cues before applying paired crops".into());
    }

    for (side, regions) in sides.into_iter().zip(layouts) {
        if side.id == MAIN_OUTPUT {
            ws.preferences.display.output_transform.regions = regions;
        } else {
            let output = ws.video_outputs.iter_mut().find(|output| output.id == side.id)
                .expect("both outputs were checked under the workspace lock");
            output.transform.regions = regions;
        }
    }
    ws.mark_modified();
    Ok(true)
}

fn active_visual(cue: &dyn Cue) -> bool {
    (cue.is_visual() && (cue.is_running() || cue.is_paused() || cue.playing_voice_id().is_some()))
        || cue.child_cues().is_some_and(|cues| cues.iter().any(|cue| active_visual(cue.as_ref())))
}

/// Save an original still chart for aligning the two saved source crops.
#[tauri::command]
pub async fn export_projection_chart(path: PathBuf, direction: OutputSplitDirection, overlap: u16) -> Result<(), String> {
    if !path.extension().and_then(|ext| ext.to_str()).is_some_and(|ext| ext.eq_ignore_ascii_case("png")) {
        return Err("Choose a PNG file for the alignment chart".into());
    }
    tauri::async_runtime::spawn_blocking(move || {
        let (w, h, pixels) = projection_chart(direction, overlap)?;
        let directory = path.parent().filter(|parent| !parent.as_os_str().is_empty())
            .ok_or("Choose a folder for the alignment chart")?;
        let mut pending = tempfile::NamedTempFile::new_in(directory).map_err(|err| err.to_string())?;
        {
            let mut encoder = png::Encoder::new(pending.as_file_mut(), w, h);
            encoder.set_color(png::ColorType::Rgb);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().map_err(|err| err.to_string())?;
            writer.write_image_data(&pixels).map_err(|err| err.to_string())?;
            writer.finish().map_err(|err| err.to_string())?;
        }
        pending.persist(path).map_err(|err| err.to_string())?;
        Ok(())
    }).await.map_err(|err| err.to_string())?
}

fn projection_chart(direction: OutputSplitDirection, overlap: u16) -> Result<(u32, u32, Vec<u8>), String> {
    let (start, end) = OutputSplitDirection::overlap_bounds(overlap)?;
    // Scale a 960×540 reference projector up to the shared canvas. Keep its
    // projected aspect ratio after cropping, including when the beams overlap.
    let cropped = 10_000 + u32::from(overlap);
    let expand = |size: u32| (size * 20_000 + cropped / 2) / cropped;
    let (w, h) = match direction {
        OutputSplitDirection::Horizontal => (expand(960), 540),
        OutputSplitDirection::Vertical => (960, expand(540)),
    };
    let mut pixels = Vec::with_capacity((w * h * 3) as usize);
    for y in 0..h {
        for x in 0..w {
            let (normal, tangent, span, cross_span) = match direction {
                OutputSplitDirection::Horizontal => (x, y, w, h),
                OutputSplitDirection::Vertical => (y, x, h, w),
            };
            let at_seam = [start, end].into_iter().any(|p| normal.abs_diff(span * u32::from(p) / 10_000) < 2);
            let outer = x < 3 || y < 3 || x >= w - 3 || y >= h - 3;
            let diagonal = y.abs_diff(x * h / w) < 2 || y.abs_diff((w - x) * h / w) < 2;
            let grid = x % (w / 16) == 0 || y % (h / 9) == 0;
            // These bands cross the entire shared seam for venue meter checks.
            let reference = match tangent * 100 / cross_span {
                38..=43 => Some([128; 3]),
                56..=61 => Some([235; 3]),
                70..=74 => Some([0; 3]),
                _ => None,
            };
            let color = if outer {
                [236, 236, 236]
            } else if let Some(gray) = reference {
                gray
            } else if diagonal {
                [236, 236, 236]
            } else if at_seam && tangent / 12 % 2 == 0 {
                [244, 175, 65]
            } else if grid {
                [124, 134, 143]
            } else if normal < span / 2 {
                [29, 55, 79]
            } else {
                [78, 53, 30]
            };
            pixels.extend_from_slice(&color);
        }
    }
    Ok((w, h, pixels))
}

#[derive(Serialize)]
pub struct ImageMaskState {
    document: Uuid,
    image: serde_json::Value,
}

#[tauri::command]
pub fn get_output_mask_image(
    output_id: Option<String>,
    state: State<'_, AppState>,
) -> Result<ImageMaskState, String> {
    let id = parse_output_id(output_id.as_deref())?.unwrap_or(MAIN_OUTPUT);
    let (document, image) = {
        let ws = state.workspace.lock().map_err(|e| e.to_string())?;
        (ws.instance_id, ws.output_mask_images.get(&id).cloned())
    };
    let image = serde_json::to_value(image.as_ref().map(|mask| mask.preview())).map_err(|e| e.to_string())?;
    Ok(ImageMaskState { document, image })
}

#[tauri::command]
pub async fn set_output_mask_image(
    document: Uuid,
    output_id: Option<String>,
    path: Option<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    let id = parse_output_id(output_id.as_deref())?.unwrap_or(MAIN_OUTPUT);
    let image = match path {
        Some(path) => Some(tauri::async_runtime::spawn_blocking(move ||
            crate::engine::output_engine::MaskImage::import_file(std::path::Path::new(&path))
        ).await.map_err(|e| e.to_string())?.map_err(|e| format!("{e:#}"))?),
        None => None,
    };
    {
        let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
        if ws.instance_id != document { return Err("Workspace changed. Reopen the projector settings.".into()); }
        if id != MAIN_OUTPUT && !ws.video_outputs.iter().any(|out| out.id == id) { return Err(unknown_output()); }
        match image.as_ref() {
            Some(mask) => { ws.output_mask_images.insert(id, mask.clone()); }
            None => { ws.output_mask_images.remove(&id); }
        }
        ws.mark_modified();
    }
    publish(&state, &app_handle);
    serde_json::to_value(image.as_ref().map(|mask| mask.preview())).map_err(|e| e.to_string())
}

/// Add an extra output (a floating window until a screen is assigned).
#[tauri::command]
pub fn add_video_output(
    name: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<VideoOutputInfo, String> {
    let name = name.trim();
    if name.is_empty() {
        return Err("An output needs a name".to_owned());
    }
    let created = OutputConfig::new(name);
    {
        let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
        if ws.video_outputs.iter().any(|c| c.name.eq_ignore_ascii_case(name)) {
            return Err(format!("An output named '{name}' already exists"));
        }
        ws.video_outputs.push(created.clone());
        ws.mark_modified();
    }
    publish(&state, &app_handle);
    Ok(VideoOutputInfo {
        id: created.id.to_string(),
        name: created.name,
        is_main: false,
        screen: created.screen,
        transform: created.transform,
    })
}

/// Rename an extra output.
#[tauri::command]
pub fn rename_video_output(
    output_id: String,
    name: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id = parse_output_id(Some(&output_id))?.ok_or("The main output cannot be renamed")?;
    let name = name.trim();
    if name.is_empty() {
        return Err("An output needs a name".to_owned());
    }
    {
        let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
        if ws.video_outputs.iter().any(|c| c.id != id && c.name.eq_ignore_ascii_case(name)) {
            return Err(format!("An output named '{name}' already exists"));
        }
        let output = ws.video_outputs.iter_mut().find(|c| c.id == id).ok_or_else(unknown_output)?;
        output.name = name.to_owned();
        ws.mark_modified();
    }
    publish(&state, &app_handle);
    Ok(())
}

/// Put an output on a monitor (`None` = a floating window) and show it there.
/// The main output is addressed with an empty or nil id.
#[tauri::command]
pub fn set_video_output_screen(
    output_id: Option<String>,
    screen: Option<u32>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id = parse_output_id(output_id.as_deref())?;
    {
        let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
        match id {
            None => ws.preferences.display.output_screen = screen,
            Some(id) => {
                let output =
                    ws.video_outputs.iter_mut().find(|c| c.id == id).ok_or_else(unknown_output)?;
                output.screen = screen;
            }
        }
        ws.mark_modified();
    }
    // Place it first: the engine then already knows the new screen, and the
    // sync that `publish` triggers has nothing left to move.
    state.output_engine.apply_output_screen_of(id, screen);
    publish(&state, &app_handle);
    Ok(())
}

/// Delete an extra output: its window closes and whatever played on it stops.
/// Cues that pointed at it report that their output no longer exists (Check
/// Workspace lists them).
#[tauri::command]
pub fn delete_video_output(
    output_id: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id = parse_output_id(Some(&output_id))?.ok_or("The main output cannot be deleted")?;
    {
        let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
        let before = ws.video_outputs.len();
        ws.video_outputs.retain(|c| c.id != id);
        ws.output_mask_images.remove(&id);
        if ws.video_outputs.len() == before {
            return Err(unknown_output());
        }
        ws.mark_modified();
    }
    publish(&state, &app_handle);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_capture_checks_the_open_workspace_and_declared_output() {
        let mut ws = Workspace::new("Stage test");
        let extra = OutputConfig::new("Upper screen");
        let id = extra.id;
        ws.video_outputs.push(extra);
        assert!(check_capture_owner(&ws, ws.instance_id, MAIN_OUTPUT).is_ok());
        assert!(check_capture_owner(&ws, ws.instance_id, id).is_ok());
        assert!(check_capture_owner(&ws, Uuid::new_v4(), id).is_err());
        ws.video_outputs.clear();
        assert!(check_capture_owner(&ws, ws.instance_id, id).is_err());
    }

    #[test]
    fn absent_empty_and_nil_ids_all_mean_the_main_output() {
        assert_eq!(parse_output_id(None), Ok(None));
        assert_eq!(parse_output_id(Some("")), Ok(None));
        assert_eq!(parse_output_id(Some("  ")), Ok(None));
        assert_eq!(parse_output_id(Some(&MAIN_OUTPUT.to_string())), Ok(None));
    }

    #[test]
    fn a_real_id_parses() {
        let id = Uuid::new_v4();
        assert_eq!(parse_output_id(Some(&id.to_string())), Ok(Some(id)));
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(parse_output_id(Some("not-a-uuid")).is_err());
    }

    #[test]
    fn the_listing_starts_with_the_main_output_then_the_extras() {
        let mut ws = Workspace::new("T");
        ws.preferences.display.output_screen = Some(1);
        let extra = OutputConfig::new("Façade");
        ws.video_outputs.push(extra.clone());

        let all = list(&ws);
        assert_eq!(all.len(), 2);
        assert!(all[0].is_main);
        assert_eq!(all[0].screen, Some(1), "the main output mirrors Preferences → Display");
        assert_eq!(all[1].id, extra.id.to_string());
        assert!(!all[1].is_main);
    }

    fn pair(extra: OutputId) -> [OutputSplitSide; 2] {
        [MAIN_OUTPUT, extra].map(|id| OutputSplitSide { id, expected_regions: OutputRegions::default() })
    }

    #[test]
    fn paired_regions_survive_save_without_overwriting_projector_alignment() {
        let mut ws = Workspace::new("Panorama");
        let document = ws.instance_id;
        let mut extra = OutputConfig::new("Upper balcony");
        extra.screen = Some(1);
        extra.transform.rotation = 0.5;
        let extra_id = extra.id;
        ws.video_outputs.push(extra);
        ws.preferences.display.output_screen = Some(0);
        ws.preferences.display.output_transform.pan_x = 0.1;

        assert_eq!(save_output_split(&mut ws, document, pair(extra_id), OutputSplitDirection::Vertical, 1_000, Some(220)), Ok(true));
        assert_eq!(ws.revision, 1);
        assert_eq!(ws.preferences.display.output_transform.pan_x, 0.1);
        assert_eq!(ws.video_outputs[0].transform.rotation, 0.5);
        assert_eq!(ws.video_outputs[0].screen, Some(1));
        let layouts = OutputSplitDirection::Vertical.feathered_regions(1_000, 220, [MAIN_OUTPUT, extra_id]).unwrap();
        let mut unchanged = pair(extra_id);
        for (side, regions) in unchanged.iter_mut().zip(layouts) {
            side.expected_regions = regions;
        }
        assert_eq!(save_output_split(&mut ws, document, unchanged, OutputSplitDirection::Vertical, 1_000, Some(220)), Ok(false));
        assert_eq!(ws.revision, 1);

        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("Paired.inkue");
        ws.save(Some(path.clone())).unwrap();
        let loaded = Workspace::load(path, &crate::cue::registry::CueRegistry::new()).unwrap();
        assert_eq!(loaded.preferences.display.output_transform.regions, layouts[0]);
        assert_eq!(loaded.video_outputs[0].transform.regions, layouts[1]);
    }

    #[test]
    fn rejecting_stale_or_invalid_sides_leaves_both_unchanged() {
        let mut ws = Workspace::new("Paired");
        let document = ws.instance_id;
        let mut extra = OutputConfig::new("Right");
        extra.screen = Some(1);
        let extra_id = extra.id;
        ws.video_outputs.push(extra);
        ws.preferences.display.output_screen = Some(1);
        assert!(save_output_split(&mut ws, document, pair(extra_id), OutputSplitDirection::Horizontal, 0, None).is_err());
        ws.preferences.display.output_screen = Some(0);
        assert!(save_output_split(&mut ws, Uuid::new_v4(), pair(extra_id), OutputSplitDirection::Horizontal, 0, None).is_err());
        assert!(save_output_split(&mut ws, document, pair(MAIN_OUTPUT), OutputSplitDirection::Horizontal, 0, None).is_err());
        assert!(save_output_split(&mut ws, document, pair(Uuid::new_v4()), OutputSplitDirection::Horizontal, 0, None).is_err());
        assert!(save_output_split(&mut ws, document, pair(extra_id), OutputSplitDirection::Horizontal, 2_001, None).is_err());

        let [_, right] = OutputSplitDirection::Horizontal.regions(0).unwrap();
        ws.video_outputs[0].transform.regions = right;
        assert!(save_output_split(&mut ws, document, pair(extra_id), OutputSplitDirection::Vertical, 100, None).is_err());
        assert!(ws.preferences.display.output_transform.regions.is_empty());
        assert_eq!(ws.video_outputs[0].transform.regions, right);
        assert_eq!(ws.revision, 0);
    }

    #[test]
    fn alignment_chart_uses_common_canvas_proportions_and_marks_both_halves() {
        let (w, h, pixels) = projection_chart(OutputSplitDirection::Horizontal, 2_000).unwrap();
        assert_eq!((w, h, pixels.len()), (1600, 540, 1600 * 540 * 3));
        let pixel = |x: u32, y: u32| {
            let start = ((y * w + x) * 3) as usize;
            &pixels[start..start + 3]
        };
        assert_eq!(pixel(25, 122), [29, 55, 79]);
        assert_eq!(pixel(1501, 122), [78, 53, 30]);
        assert_eq!(pixel(640, 122), [244, 175, 65]);
        assert_eq!(pixel(0, 0), [236, 236, 236]);
        for (row, shade) in [(216, 128), (324, 235), (389, 0)] {
            for column in [360, 800, 1240] { assert_eq!(pixel(column, row), [shade; 3]); }
        }
        let (w, h, _) = projection_chart(OutputSplitDirection::Vertical, 0).unwrap();
        assert_eq!((w, h), (960, 1080));
    }
}
