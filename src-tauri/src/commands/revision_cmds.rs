use crate::{revisions::{live_store, RevisionInfo}, state::AppState};
use std::path::PathBuf;
use tauri::State;
use uuid::Uuid;

#[tauri::command]
pub(crate) async fn list_revision_sources() -> Result<Vec<PathBuf>, String> {
    tauri::async_runtime::spawn_blocking(|| live_store().sources().map_err(|e| format!("{e:#}")))
        .await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn list_saved_revisions(source: PathBuf) -> Result<Vec<RevisionInfo>, String> {
    tauri::async_runtime::spawn_blocking(move || live_store().list(&source).map_err(|e| format!("{e:#}")))
        .await.map_err(|e| e.to_string())?
}

#[tauri::command]
pub(crate) async fn restore_revision_copy(source: PathBuf, revision: Uuid, destination: PathBuf,
    state: State<'_, AppState>) -> Result<(), String> {
    let active = state.workspace.lock().map_err(|e| e.to_string())?.file_path.clone();
    tauri::async_runtime::spawn_blocking(move || {
        live_store().restore(&source, revision, &destination, active.as_deref()).map_err(|e| format!("{e:#}"))
    }).await.map_err(|e| e.to_string())?
}
