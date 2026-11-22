//! Operator cue-list and cart windows bound by the calling webview label.
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use uuid::Uuid;

use super::cue_cmds::{list_summaries, CueSummary};
use super::transport_cmds::{accept_chained_go, go_cue_in_list, workspace_context};
use crate::{cue::types::CueType, show::{
    cart_layout::CartLayout, cue_list_window::{set_list_paused, CueListWindowKind, CueListWindowTarget}, transport::Transport,
}};
use crate::state::AppState;

#[derive(Serialize)]
pub struct CueListWindowSnapshot {
    workspace_id: Uuid,
    name: String,
    number: Option<String>,
    layout: Option<CartLayout>,
    standby_id: Option<Uuid>,
    standby_child_id: Option<Uuid>,
    cues: Vec<CueSummary>,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CartPadAction {
    Fire,
    Stop,
}

#[tauri::command]
pub async fn open_cart_window(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, AppState>,
    list_id: Uuid,
) -> Result<(), String> {
    open_bound_list(&window, &app, &state, list_id, CueListWindowKind::Cart)
}

#[tauri::command]
pub async fn open_sequential_window(
    window: WebviewWindow, app: AppHandle, state: State<'_, AppState>, list_id: Uuid,
) -> Result<(), String> {
    open_bound_list(&window, &app, &state, list_id, CueListWindowKind::Sequential)
}

fn open_bound_list(
    window: &WebviewWindow, app: &AppHandle, state: &AppState, list_id: Uuid, kind: CueListWindowKind,
) -> Result<(), String> {
    if window.label() != "main" {
        return Err("Open operator windows from the main desk".into());
    }
    let (label, title) = {
        let ws = state.workspace.lock().map_err(|e| e.to_string())?;
        let target = CueListWindowTarget::bind(&ws, list_id, kind).map_err(|e| e.to_string())?;
        let list = target.list(&ws).map_err(|e| e.to_string())?;
        (target.label(), format!("Inkue · {}", list.name))
    };
    // Build on an asynchronous command thread. No workspace lock is held while
    // the native window manager creates or focuses a window.
    let cart = if let Some(existing) = app.get_webview_window(&label) {
        existing
    } else {
        WebviewWindowBuilder::new(app, label.clone(), WebviewUrl::App("index.html".into()))
            .title(title)
            .inner_size(720.0, 520.0)
            .min_inner_size(260.0, 180.0)
            .build()
            .or_else(|error| app.get_webview_window(&label).ok_or(error))
            .map_err(|e| e.to_string())?
    };
    cart.unminimize().map_err(|e| e.to_string())?;
    cart.show().map_err(|e| e.to_string())?;
    cart.set_focus().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn read_cart_window(
    window: WebviewWindow,
    state: State<'_, AppState>,
) -> Result<Option<CueListWindowSnapshot>, String> {
    read_bound_list(&window, &state, CueListWindowKind::Cart)
}

#[tauri::command]
pub fn read_sequential_window(
    window: WebviewWindow, state: State<'_, AppState>,
) -> Result<Option<CueListWindowSnapshot>, String> {
    read_bound_list(&window, &state, CueListWindowKind::Sequential)
}

fn read_bound_list(
    window: &WebviewWindow, state: &AppState, kind: CueListWindowKind,
) -> Result<Option<CueListWindowSnapshot>, String> {
    let target = CueListWindowTarget::from_label(window.label(), kind).map_err(|e| e.to_string())?;
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let Ok(list) = target.list(&ws) else {
        return Ok(None);
    };
    let app = window.app_handle().clone();
    let loading = state.loading_cues.lock().map_err(|e| e.to_string())?;
    Ok(Some(CueListWindowSnapshot {
        workspace_id: ws.instance_id(),
        name: list.name.clone(),
        number: list.number.clone(),
        layout: list.cart_layout.clone(),
        standby_id: list.playhead_cue_id,
        standby_child_id: list.enter_standby_child_id(),
        cues: state.media_presence.collect(
            |missing| list_summaries(&ws, list, &loading, missing),
            move || { let _ = app.emit("cue-list-refresh", ()); },
        ),
    }))
}

#[tauri::command]
pub fn cart_window_pad(
    window: WebviewWindow,
    app: AppHandle,
    state: State<'_, AppState>,
    cue_id: Uuid,
    action: CartPadAction,
) -> Result<(), String> {
    let target = CueListWindowTarget::from_label(window.label(), CueListWindowKind::Cart).map_err(|e| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let fire = matches!(action, CartPadAction::Fire);
    if fire
        && state
            .loading_cues
            .lock()
            .map_err(|e| e.to_string())?
            .contains(&cue_id)
    {
        return Err("This pad is still loading".into());
    }
    let mut transport = Transport::new(workspace_context(&state, &ws));
    let list = target
        .pad_list(&mut ws, cue_id, fire)
        .map_err(|e| e.to_string())?;
    match action {
        CartPadAction::Fire => go_cue_in_list(&mut transport, list, cue_id, false, &app),
        CartPadAction::Stop => {
            transport
                .stop_cue(list, &cue_id)
                .map_err(|e| e.to_string())?;
            let _ = app.emit("cue-list-refresh", ());
            Ok(())
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SequentialWindowAction {
    Go { cue_id: Uuid, child_id: Option<Uuid> },
    SetStandby { cue_id: Uuid },
    PauseList,
    ResumeList,
    StopList,
}

#[tauri::command]
pub fn sequential_window_action(
    window: WebviewWindow, app: AppHandle, state: State<'_, AppState>, action: SequentialWindowAction,
) -> Result<(), String> {
    let target = CueListWindowTarget::from_label(window.label(), CueListWindowKind::Sequential)
        .map_err(|e| e.to_string())?;
    let mut ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let list = target.list(&ws).map_err(|e| e.to_string())?;
    match action {
        SequentialWindowAction::Go { cue_id, child_id } => {
            let next = target.standby(&ws, cue_id, child_id).map_err(|e| e.to_string())?;
            let waiting = state.loading_cues.lock().map_err(|e| e.to_string())?.contains(&next)
                || list.get_recursive(&next).is_some_and(|cue| cue.cue_type() == CueType::Audio
                    && cue.file_duration().is_none() && cue.media_file_path().is_some_and(|path| !path.as_os_str().is_empty()));
            if waiting { return Err("The standby cue is still loading".into()); }
            if !accept_chained_go(&state, ws.preferences.general.double_go_protection_ms) { return Ok(()); }
        }
        SequentialWindowAction::SetStandby { cue_id } => {
            if !list.get(&cue_id).is_some_and(|cue| !cue.is_disabled()) {
                return Err("Choose an armed top-level cue in this list".into());
            }
        }
        SequentialWindowAction::PauseList | SequentialWindowAction::ResumeList | SequentialWindowAction::StopList => {}
    }
    let selected_on_desk = ws.active_cue_list_id == target.list_id;
    let mut transport = Transport::new(workspace_context(&state, &ws));
    let list = ws.cue_list_by_id_mut(target.list_id).ok_or("This cue list was removed")?;
    let result = match action {
        SequentialWindowAction::Go { .. } => transport.go(list).map(|_| ()),
        SequentialWindowAction::SetStandby { cue_id } => list.set_playhead(Some(cue_id)),
        SequentialWindowAction::PauseList => set_list_paused(list, &transport.context, true),
        SequentialWindowAction::ResumeList => set_list_paused(list, &transport.context, false),
        SequentialWindowAction::StopList => transport.stop_all(list),
    };
    let standby = list.playhead_cue_id;
    if selected_on_desk && matches!(action, SequentialWindowAction::Go { .. } | SequentialWindowAction::SetStandby { .. }) {
        let _ = app.emit("playhead-moved", serde_json::json!({ "cue_id": standby }));
    }
    let _ = app.emit("cue-list-refresh", ());
    result.map_err(|error| error.to_string())
}
