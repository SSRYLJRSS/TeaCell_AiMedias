//! GUI-only directory selection and authorization management commands.
//!
//! The selected path never enters this module as command input. The native
//! picker runs inside the Rust host, and the resulting grant is bound to the
//! host-created GUI caller context.

use tauri::{AppHandle, State};
use tauri_plugin_dialog::DialogExt;

use crate::contracts::agent::ResourceRef;
use crate::contracts::caller::CallerContext;
use crate::error::{AppError, AppResult};
use crate::services::authorization::{
    self, DirectoryGrantView, DirectoryPage, DirectoryPurpose, TrustedAdapterIdentity,
};
use crate::state::AppState;

fn gui_caller(state: &AppState) -> AppResult<CallerContext> {
    let conn = state.db.lock()?;
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Gui {
            session_id: state.authorization_session_id.clone(),
            instance_id: state.authorization_instance_id.clone(),
        },
        ResourceRef::generated("request"),
        Vec::new(),
    )
}

#[tauri::command]
pub async fn pick_directory_grant(
    app: AppHandle,
    state: State<'_, AppState>,
    purpose: String,
    remembered: bool,
) -> AppResult<Option<DirectoryGrantView>> {
    let purpose = DirectoryPurpose::parse(&purpose)?;
    let picker_app = app.clone();
    let selected = tauri::async_runtime::spawn_blocking(move || {
        picker_app
            .dialog()
            .file()
            .set_title("选择授权目录")
            .blocking_pick_folder()
    })
    .await
    .map_err(|error| AppError::msg(format!("目录选择线程异常: {error}")))?;
    let Some(selected) = selected else {
        return Ok(None);
    };
    let selected = selected
        .into_path()
        .map_err(|_| AppError::invalid_arg("所选目录不是本机文件系统路径"))?;
    let caller = gui_caller(&state)?;
    Ok(Some(authorization::grant_selected_directory(
        &state.db, &caller, &selected, purpose, remembered,
    )?))
}

#[tauri::command]
pub fn list_directory_grants(state: State<AppState>) -> AppResult<Vec<DirectoryGrantView>> {
    let caller = gui_caller(&state)?;
    authorization::list_directory_grants(&state.db, &caller)
}

#[tauri::command]
pub fn reissue_directory_grant(
    state: State<AppState>,
    grant_ref: ResourceRef,
) -> AppResult<DirectoryGrantView> {
    let caller = gui_caller(&state)?;
    authorization::reissue_directory_handle(&state.db, &caller, &grant_ref)
}

#[tauri::command]
pub fn revoke_directory_grant(state: State<AppState>, grant_ref: ResourceRef) -> AppResult<()> {
    let caller = gui_caller(&state)?;
    authorization::revoke_directory_grant(&state.db, &caller, &grant_ref)
}

#[tauri::command]
pub fn revoke_all_directory_grants(state: State<AppState>) -> AppResult<()> {
    let caller = gui_caller(&state)?;
    authorization::revoke_all_directory_grants(&state.db, &caller)
}

#[tauri::command]
pub fn page_authorized_directory(
    state: State<AppState>,
    directory_ref: ResourceRef,
    cursor: Option<u32>,
    limit: Option<u32>,
) -> AppResult<DirectoryPage> {
    let caller = gui_caller(&state)?;
    authorization::page_authorized_directory(
        &state.db,
        &caller,
        &directory_ref,
        cursor,
        limit.unwrap_or(100),
    )
}
