//! Tauri commands for the DMX lighting engine: output configuration, the live
//! monitor snapshot, blackout, and a debug single-channel poke.
//!
//! The live monitor is event-driven (`dmx-monitor`, emitted from `lib.rs`); the
//! `dmx_get_snapshot` command is only a one-shot fallback for initial paint.

use serde::Serialize;
use std::sync::MutexGuard;
use tauri::{Emitter, State};
use uuid::Uuid;

use crate::cue::light_cue::ParamTarget;
use crate::engine::dmx_engine::{ChannelWidth, DmxChannelLevel, DmxSnapshot};
use crate::engine::dmx_sink::UniverseOutput;
use crate::engine::fixture::{
    builtin_fixture_types, check_color_patch, find_conflicts, FixtureColorChannels,
    FixtureColorModel, FixtureConflict, FixtureGroup, FixtureType, PatchedChannel, PatchedFixture,
};
use crate::engine::DmxEngine;
use crate::show::Workspace;
use crate::state::AppState;

/// One universe's live output bytes, for the DMX monitor view.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DmxUniverseSnapshot {
    pub universe: u16,
    pub channels: Vec<u8>,
}

/// Build a sorted, serialisable snapshot of every active universe.
/// Shared by the `dmx_get_snapshot` command and the `dmx-monitor` event thread.
pub fn snapshot_dto(engine: &DmxEngine) -> Vec<DmxUniverseSnapshot> {
    let mut out: Vec<DmxUniverseSnapshot> = engine
        .snapshot()
        .into_iter()
        .map(|(universe, data)| DmxUniverseSnapshot { universe, channels: data.to_vec() })
        .collect();
    out.sort_by_key(|s| s.universe);
    out
}

/// Configure which universes are transmitted and how (sACN / Art-Net + destination).
///
/// Persists the mapping into the workspace (so it travels with the show) and
/// pushes it to the engine.  Called when the operator edits the outputs — not
/// on load, where the backend pushes the stored mapping itself.
#[tauri::command]
pub fn dmx_set_outputs(
    outputs: Vec<UniverseOutput>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    {
        let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
        ws.universe_outputs = outputs.clone();
        ws.mark_modified();
    }
    state.dmx_engine.set_outputs(outputs);
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Return the workspace's stored universe output mapping.
#[tauri::command]
pub fn dmx_get_outputs(state: State<'_, AppState>) -> Result<Vec<UniverseOutput>, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    Ok(ws.universe_outputs.clone())
}

/// Debug: set a single DMX channel (1-based `address`) to `value` (0–255), no fade.
#[tauri::command]
pub fn dmx_set_channel(
    workspace_id: Uuid,
    universe: u16,
    address: u16,
    value: u8,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let _workspace = lock_dashboard(&state, workspace_id)?;
    let channel = address.saturating_sub(1);
    state
        .dmx_engine
        .set_channel(universe, channel, ChannelWidth::Bit8, value as f64 / 255.0);
    Ok(())
}

/// Toggle the global blackout override.
#[tauri::command]
pub fn dmx_set_blackout(on: bool, state: State<'_, AppState>, app_handle: tauri::AppHandle) -> Result<(), String> {
    apply_blackout(on, &state, &app_handle);
    Ok(())
}

pub(super) fn apply_blackout(on: bool, state: &AppState, app: &tauri::AppHandle) {
    state.dmx_engine.set_blackout(on);
    let _ = app.emit("dmx-blackout-changed", ());
}

/// Whether blackout is currently active.
#[tauri::command]
pub fn dmx_get_blackout(state: State<'_, AppState>) -> Result<bool, String> {
    Ok(state.dmx_engine.is_blackout())
}

/// One-shot snapshot of every active universe (initial paint; live feed is the
/// `dmx-monitor` event).
#[tauri::command]
pub fn dmx_get_snapshot(state: State<'_, AppState>) -> Result<Vec<DmxUniverseSnapshot>, String> {
    Ok(snapshot_dto(&state.dmx_engine))
}

// ---------------------------------------------------------------------------
// Fixture patch
// ---------------------------------------------------------------------------

/// The built-in fixture templates offered when patching.
#[tauri::command]
pub fn list_builtin_fixture_types() -> Vec<FixtureType> {
    builtin_fixture_types()
}

/// Every patched fixture in the workspace.
#[tauri::command]
pub fn list_fixtures(state: State<'_, AppState>) -> Result<Vec<PatchedFixture>, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    Ok(ws.fixtures.clone())
}

/// Patch a new fixture from a type at the given address.  Returns the created fixture.
#[tauri::command]
pub fn add_fixture(
    label: String,
    universe: u16,
    base_address: u16,
    fixture_type: FixtureType,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<PatchedFixture, String> {
    let fixture = PatchedFixture::new(label, universe, base_address, fixture_type);
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    ws.fixtures.push(fixture.clone());
    ws.mark_modified();
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(fixture)
}

/// Replace an existing fixture (matched by ID).
#[tauri::command]
pub fn update_fixture(
    fixture: PatchedFixture,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if let Some(existing) = ws.fixtures.iter_mut().find(|f| f.id == fixture.id) {
        *existing = fixture;
        ws.mark_modified();
        let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
        Ok(())
    } else {
        Err(format!("Fixture {} not found", fixture.id))
    }
}

/// Remove a fixture by ID.
#[tauri::command]
pub fn remove_fixture(
    fixture_id: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = fixture_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let before = ws.fixtures.len();
    ws.fixtures.retain(|f| f.id != id);
    if ws.fixtures.len() == before {
        return Err(format!("Fixture {id} not found"));
    }
    ws.mark_modified();
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

/// Address clashes between patched fixtures, for the patch UI to warn about.
#[tauri::command]
pub fn get_fixture_conflicts(state: State<'_, AppState>) -> Result<Vec<FixtureConflict>, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    Ok(find_conflicts(&ws.fixtures))
}

/// Identify a fixture by driving its parameters to a visible value (`on = true`)
/// or back to zero (`on = false`), immediately (no fade).  Lets the operator
/// match a patched fixture to a physical instrument.
#[tauri::command]
pub fn dmx_test_fixture(
    fixture_id: String,
    on: bool,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let id: Uuid = fixture_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let fixture = ws.fixtures.iter().find(|f| f.id == id).ok_or("Fixture not found")?;
    for (i, param) in fixture.fixture_type.parameters.iter().enumerate() {
        if let Some((universe, channel, width)) = fixture.resolve_channel(i) {
            let value = if on { param.kind.identify_value() } else { 0.0 };
            state.dmx_engine.set_channel(universe, channel, width, value);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Live Dashboard — sculpt fixtures live, then capture the look into a cue
// ---------------------------------------------------------------------------

/// Read one channel's normalised value `[0, 1]` from a universe snapshot.
fn read_norm_from_snapshot(snap: &DmxSnapshot, universe: u16, channel: u16, width: ChannelWidth) -> f64 {
    let Some(buf) = snap.get(&universe) else { return 0.0 };
    let i = channel as usize;
    match width {
        ChannelWidth::Bit8 => *buf.get(i).unwrap_or(&0) as f64 / 255.0,
        ChannelWidth::Bit16 => {
            let hi = *buf.get(i).unwrap_or(&0) as u16;
            let lo = *buf.get(i + 1).unwrap_or(&0) as u16;
            ((hi << 8) | lo) as f64 / 65535.0
        }
    }
}

#[derive(Serialize)]
pub struct FixtureDashboardState {
    fixtures: Vec<PatchedFixture>,
    groups: Vec<FixtureGroup>,
    snapshot: Vec<DmxUniverseSnapshot>,
}

/// Read the patch and live levels for the workspace that opened the Dashboard.
#[tauri::command]
pub fn load_fixture_dashboard(
    workspace_id: Uuid,
    state: State<'_, AppState>,
) -> Result<FixtureDashboardState, String> {
    let ws = lock_dashboard(&state, workspace_id)?;
    Ok(FixtureDashboardState {
        fixtures: ws.fixtures.clone(),
        groups: ws.fixture_groups.clone(),
        snapshot: snapshot_dto(&state.dmx_engine),
    })
}

fn lock_dashboard(state: &AppState, workspace_id: Uuid) -> Result<MutexGuard<'_, Workspace>, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    check_dashboard_workspace(&ws, workspace_id)?;
    Ok(ws)
}

fn check_dashboard_workspace(ws: &Workspace, workspace_id: Uuid) -> Result<(), String> {
    if ws.instance_id != workspace_id {
        return Err("Workspace changed. Refresh the Dashboard to use the current show.".into());
    }
    Ok(())
}

/// Set one fixture parameter to `value` (0–1) immediately (no fade) — the live
/// editing path for the Dashboard sliders / colour picker.
#[tauri::command]
pub fn dmx_set_fixture_param(
    workspace_id: Uuid,
    fixture_id: Uuid,
    param_index: usize,
    value: f64,
    state: State<'_, AppState>,
) -> Result<(), String> {
    let ws = lock_dashboard(&state, workspace_id)?;
    let fixture = ws.fixtures.iter().find(|f| f.id == fixture_id).ok_or("Fixture not found")?;
    if let Some((universe, channel, width)) = fixture.resolve_channel(param_index) {
        state.dmx_engine.set_channel(universe, channel, width, value);
    }
    Ok(())
}

/// Apply a native CMY mix through the exact patch displayed in the Dashboard.
#[tauri::command]
pub fn dmx_set_fixture_cmy(
    workspace_id: Uuid,
    fixture_id: Uuid,
    expected: [PatchedChannel; 3],
    levels: [f64; 3],
    state: State<'_, AppState>,
) -> Result<(), String> {
    set_fixture_color(workspace_id, fixture_id, expected, levels, FixtureColorModel::Cmy, &state)
}

/// Apply explicit hue, saturation and HSI intensity together on the live engine.
#[tauri::command]
pub fn dmx_set_fixture_hsi(
    workspace_id: Uuid,
    fixture_id: Uuid,
    expected: [PatchedChannel; 3],
    levels: [f64; 3],
    state: State<'_, AppState>,
) -> Result<(), String> {
    set_fixture_color(workspace_id, fixture_id, expected, levels, FixtureColorModel::Hsi, &state)
}

/// Apply explicit hue, saturation and HSV value together on the live engine.
#[tauri::command]
pub fn dmx_set_fixture_hsv(
    workspace_id: Uuid,
    fixture_id: Uuid,
    expected: [PatchedChannel; 3],
    levels: [f64; 3],
    state: State<'_, AppState>,
) -> Result<(), String> {
    set_fixture_color(workspace_id, fixture_id, expected, levels, FixtureColorModel::Hsv, &state)
}

fn set_fixture_color(
    workspace_id: Uuid,
    fixture_id: Uuid,
    expected: [PatchedChannel; 3],
    levels: [f64; 3],
    model: FixtureColorModel,
    state: &AppState,
) -> Result<(), String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let channels = fixture_color_levels(&ws, workspace_id, fixture_id, expected, levels, model)?;
    state.dmx_engine.submit_fade_batch(channels, std::time::Duration::ZERO, crate::engine::ring_command::FadeCurve::Linear)
}

fn fixture_color_levels(
    ws: &Workspace,
    workspace_id: Uuid,
    fixture_id: Uuid,
    expected: [PatchedChannel; 3],
    levels: [f64; 3],
    model: FixtureColorModel,
) -> Result<Vec<DmxChannelLevel>, String> {
    check_dashboard_workspace(ws, workspace_id)?;
    check_color_levels(levels, model)?;
    let fixture = ws.fixtures.iter().find(|fixture| fixture.id == fixture_id).ok_or("Fixture not found")?;
    let current = fixture.color_channels(model)?;
    if current != expected {
        return Err("Fixture patch changed. Refresh the Dashboard before applying color".into());
    }
    check_color_patch(&ws.fixtures, &[FixtureColorChannels { id: fixture_id, channels: current }], model)?;
    Ok(current.into_iter().zip(levels).map(|(channel, level)| {
        (channel.universe, channel.channel, channel.width, level)
    }).collect())
}

/// Apply a group mix only when the exact ordered patch still matches the Dashboard.
#[tauri::command]
pub fn dmx_set_group_color(
    workspace_id: Uuid,
    group_id: Uuid,
    model: FixtureColorModel,
    expected: Vec<FixtureColorChannels>,
    levels: [f64; 3],
    state: State<'_, AppState>,
) -> Result<(), String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let channels = group_color_levels(&ws, workspace_id, group_id, model, &expected, levels)?;
    state.dmx_engine.submit_fade_batch(channels, std::time::Duration::ZERO, crate::engine::ring_command::FadeCurve::Linear)
}

fn group_color_levels(
    ws: &Workspace,
    workspace_id: Uuid,
    group_id: Uuid,
    model: FixtureColorModel,
    expected: &[FixtureColorChannels],
    levels: [f64; 3],
) -> Result<Vec<DmxChannelLevel>, String> {
    check_dashboard_workspace(ws, workspace_id)?;
    check_color_levels(levels, model)?;
    let group = ws.fixture_groups.iter().find(|group| group.id == group_id)
        .ok_or("Fixture group was removed. Refresh the Dashboard.")?;
    let current = group.color_channels(&ws.fixtures, model)
        .map_err(|error| format!("{error} Refresh the Dashboard."))?;
    if current.as_slice() != expected {
        return Err("Fixture group or patch changed. Refresh the Dashboard before applying color.".into());
    }
    Ok(current.into_iter().flat_map(|member| member.channels.into_iter().zip(levels))
        .map(|(channel, level)| (channel.universe, channel.channel, channel.width, level))
        .collect())
}

fn check_color_levels(levels: [f64; 3], model: FixtureColorModel) -> Result<(), String> {
    if levels.iter().any(|value| !(0.0..=1.0).contains(value)) {
        return Err(format!("{} levels are outside the fixture's normalized range", model.name()));
    }
    Ok(())
}

/// Drive every patched fixture's parameters to zero (no fade) — Dashboard "Clear".
#[tauri::command]
pub fn dmx_clear_fixtures(workspace_id: Uuid, state: State<'_, AppState>) -> Result<(), String> {
    let ws = lock_dashboard(&state, workspace_id)?;
    for fixture in &ws.fixtures {
        for i in 0..fixture.fixture_type.parameters.len() {
            if let Some((universe, channel, width)) = fixture.resolve_channel(i) {
                state.dmx_engine.set_channel(universe, channel, width, 0.0);
            }
        }
    }
    Ok(())
}

/// Capture the current live state of every patched fixture as a full set of
/// Light Cue targets ("record look").  Pure read — the caller applies these to
/// the cue through the normal `update_cue` path (single write + undo step).
#[tauri::command]
pub fn capture_live_targets(state: State<'_, AppState>) -> Result<Vec<ParamTarget>, String> {
    let snapshot = state.dmx_engine.snapshot();
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let mut targets = Vec::new();
    for fixture in &ws.fixtures {
        for i in 0..fixture.fixture_type.parameters.len() {
            if let Some((universe, channel, width)) = fixture.resolve_channel(i) {
                targets.push(ParamTarget::Fixture {
                    fixture_id: fixture.id.to_string(),
                    param_index: i,
                    value: read_norm_from_snapshot(&snapshot, universe, channel, width),
                });
            }
        }
    }
    Ok(targets)
}

// ---------------------------------------------------------------------------
// Fixture groups
// ---------------------------------------------------------------------------

/// Every fixture group in the workspace.
#[tauri::command]
pub fn list_fixture_groups(state: State<'_, AppState>) -> Result<Vec<FixtureGroup>, String> {
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    Ok(ws.fixture_groups.clone())
}

/// Create a fixture group from a label and member fixture IDs.
#[tauri::command]
pub fn add_fixture_group(
    label: String,
    fixture_ids: Vec<String>,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<FixtureGroup, String> {
    let ids: Vec<Uuid> = fixture_ids
        .iter()
        .filter_map(|s| s.parse().ok())
        .collect();
    let group = FixtureGroup::new(label, ids);
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    ws.fixture_groups.push(group.clone());
    ws.mark_modified();
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(group)
}

/// Replace an existing fixture group (matched by ID).
#[tauri::command]
pub fn update_fixture_group(
    group: FixtureGroup,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    if let Some(existing) = ws.fixture_groups.iter_mut().find(|g| g.id == group.id) {
        *existing = group;
        ws.mark_modified();
        let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
        Ok(())
    } else {
        Err(format!("Group {} not found", group.id))
    }
}

/// Remove a fixture group by ID.
#[tauri::command]
pub fn remove_fixture_group(
    group_id: String,
    state: State<'_, AppState>,
    app_handle: tauri::AppHandle,
) -> Result<(), String> {
    let id: Uuid = group_id.parse().map_err(|e: uuid::Error| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let before = ws.fixture_groups.len();
    ws.fixture_groups.retain(|g| g.id != id);
    if ws.fixture_groups.len() == before {
        return Err(format!("Group {id} not found"));
    }
    ws.mark_modified();
    let _ = app_handle.emit("workspace-modified", serde_json::json!({}));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_dashboard_cannot_apply_color_to_a_reopened_patch_with_identical_fixtures() {
        for (template, model) in [
            ("CMY (3ch)", FixtureColorModel::Cmy),
            ("HSI (3ch)", FixtureColorModel::Hsi),
            ("HSV (3ch)", FixtureColorModel::Hsv),
        ] {
            let fixture_type = builtin_fixture_types()
                .into_iter()
                .find(|f| f.name == template)
                .unwrap();
            let fixture = PatchedFixture::new("Wash", 5, 401, fixture_type);
            let id = fixture.id;
            let channels = fixture.color_channels(model).unwrap();
            let mut first = Workspace::new("Original show");
            first.fixtures.push(fixture.clone());
            let mut reopened = Workspace::new("Collected copy");
            reopened.fixtures.push(fixture);
            let levels = [0.25, 0.6, 0.4];

            assert!(fixture_color_levels(&first, first.instance_id, id, channels, levels, model).is_ok());
            let error = fixture_color_levels(&reopened, first.instance_id, id, channels, levels, model).unwrap_err();
            assert!(error.starts_with("Workspace changed"));
            let accepted =
                fixture_color_levels(&reopened, reopened.instance_id, id, channels, levels, model).unwrap();
            assert_eq!(accepted, vec![
                (5, 400, ChannelWidth::Bit8, 0.25),
                (5, 401, ChannelWidth::Bit8, 0.6),
                (5, 402, ChannelWidth::Bit8, 0.4),
            ]);

            for invalid in [f64::NAN, f64::INFINITY, -0.001, 1.001] {
                let result = fixture_color_levels(&reopened, reopened.instance_id, id, channels, [0.25, 0.6, invalid], model);
                assert!(result.unwrap_err().contains("normalized range"));
            }
        }
    }

    #[test]
    fn live_group_requires_the_same_workspace_members_order_and_physical_slots() {
        let model = FixtureColorModel::Hsv;
        let fixture_type = builtin_fixture_types().into_iter().find(|fixture| fixture.name == "HSV (3ch)").unwrap();
        let first = PatchedFixture::new("Left", 7, 101, fixture_type.clone());
        let mut second = PatchedFixture::new("Right", 7, 111, fixture_type);
        second.fixture_type.parameters[2].width = ChannelWidth::Bit16;
        let group = FixtureGroup::new("Window wash", vec![first.id, second.id]);
        let group_id = group.id;
        let mut ws = Workspace::new("Show");
        ws.fixtures = vec![first, second];
        ws.fixture_groups = vec![group];
        let expected = ws.fixture_groups[0].color_channels(&ws.fixtures, model).unwrap();
        let levels = [2.0 / 3.0, 0.5, 0.75];
        let resolve = |ws: &Workspace| group_color_levels(ws, ws.instance_id, group_id, model, &expected, levels);

        assert_eq!(resolve(&ws).unwrap(), [
            (7, 100, ChannelWidth::Bit8, levels[0]), (7, 101, ChannelWidth::Bit8, levels[1]),
            (7, 102, ChannelWidth::Bit8, levels[2]), (7, 110, ChannelWidth::Bit8, levels[0]),
            (7, 111, ChannelWidth::Bit8, levels[1]), (7, 112, ChannelWidth::Bit16, levels[2]),
        ]);
        assert!(group_color_levels(&ws, ws.instance_id, group_id, model, &expected, [0.0, 0.5, f64::NAN]).is_err());

        let mut reopened = Workspace::new("Copy");
        reopened.fixtures = ws.fixtures.clone();
        reopened.fixture_groups = ws.fixture_groups.clone();
        let error = group_color_levels(&reopened, ws.instance_id, group_id, model, &expected, levels).unwrap_err();
        assert!(error.starts_with("Workspace changed"));

        ws.fixture_groups[0].fixture_ids.swap(0, 1);
        assert!(resolve(&ws).unwrap_err().starts_with("Fixture group or patch changed"));
        ws.fixture_groups[0].fixture_ids.swap(0, 1);
        let original_id = ws.fixtures[1].id;
        ws.fixtures[1].id = Uuid::new_v4();
        ws.fixture_groups[0].fixture_ids[1] = ws.fixtures[1].id;
        assert!(resolve(&ws).unwrap_err().starts_with("Fixture group or patch changed"));
        ws.fixtures[1].id = original_id;
        ws.fixture_groups[0].fixture_ids[1] = original_id;
        ws.fixtures[1].base_address += 1;
        assert!(resolve(&ws).unwrap_err().starts_with("Fixture group or patch changed"));
        ws.fixtures[1].base_address -= 1;

        let dimmer_type = builtin_fixture_types().remove(0);
        ws.fixtures.push(PatchedFixture::new("Other dimmer", 7, 114, dimmer_type));
        assert!(resolve(&ws).unwrap_err().contains("Other dimmer"));
        ws.fixtures.pop();
        ws.fixtures.pop();
        assert!(resolve(&ws).unwrap_err().contains("no longer patched"));
        ws.fixture_groups.clear();
        assert!(resolve(&ws).unwrap_err().contains("group was removed"));
    }
}
