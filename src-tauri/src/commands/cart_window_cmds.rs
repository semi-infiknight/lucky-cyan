//! Performance-only cart windows. The calling webview label supplies the binding.
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, Manager, State, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use uuid::Uuid;

use super::cue_cmds::{list_summaries, CueSummary};
use super::transport_cmds::{go_cue_in_list, workspace_context};
use crate::show::{cart_layout::CartLayout, cart_window::CartWindowTarget, transport::Transport};
use crate::state::AppState;

#[derive(Serialize)]
pub struct CartWindowSnapshot {
    name: String,
    layout: Option<CartLayout>,
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
    if window.label() != "main" {
        return Err("Open carts from the main desk".into());
    }
    let (label, title) = {
        let ws = state.workspace.lock().map_err(|e| e.to_string())?;
        let target = CartWindowTarget::bind(&ws, list_id).map_err(|e| e.to_string())?;
        let list = target.list(&ws).map_err(|e| e.to_string())?;
        (target.label(), format!("Inkue · {}", list.name))
    };
    // Build on an asynchronous command thread. No workspace lock is held while
    // the native window manager creates or focuses a window.
    let cart = if let Some(existing) = app.get_webview_window(&label) {
        existing
    } else {
        WebviewWindowBuilder::new(&app, label.clone(), WebviewUrl::App("index.html".into()))
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
) -> Result<Option<CartWindowSnapshot>, String> {
    let target = CartWindowTarget::from_label(window.label()).map_err(|e| e.to_string())?;
    let ws = state.workspace.lock().map_err(|e| e.to_string())?;
    let Ok(list) = target.list(&ws) else {
        return Ok(None);
    };
    let app = window.app_handle().clone();
    let loading = state.loading_cues.lock().map_err(|e| e.to_string())?;
    Ok(Some(CartWindowSnapshot {
        name: list.name.clone(),
        layout: list.cart_layout.clone(),
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
    let target = CartWindowTarget::from_label(window.label()).map_err(|e| e.to_string())?;
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
