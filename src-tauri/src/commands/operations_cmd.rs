//! GUI entry points for cancel/resume and startup recovery summaries.

use tauri::{AppHandle, State};

use crate::contracts::agent::{OperationPhase, OperationSnapshot, OperationState, ResourceRef};
use crate::contracts::caller::CallerContext;
use crate::error::{AppError, AppResult};
use crate::services::authorization::{self, DirectoryPurpose, TrustedAdapterIdentity};
use crate::services::operations::{self, RecoverySummaryView};
use crate::services::tagging;
use crate::state::AppState;
use tauri_plugin_dialog::DialogExt;

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
pub fn cancel_operation(
    state: State<AppState>,
    operation_ref: ResourceRef,
) -> AppResult<OperationSnapshot> {
    let caller = gui_caller(&state)?;
    let batch_id = tagging::batch_id_for_cancel(&state.db, &caller, &operation_ref)?;
    let snapshot = operations::cancel_operation(&state.db, &caller, &operation_ref)?;
    if matches!(snapshot.phase, OperationPhase::Cancelling)
        || matches!(snapshot.state, OperationState::Cancelled)
    {
        if let Some(batch_id) = batch_id {
            let active = state
                .ai_cancel
                .lock()
                .map_err(|_| AppError::msg("取消注册表锁中毒"))?;
            if let Some(cancel) = active.get(&batch_id) {
                cancel.store(true, std::sync::atomic::Ordering::Relaxed);
            }
        }
    }
    Ok(snapshot)
}

#[tauri::command]
pub async fn resume_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_ref: ResourceRef,
) -> AppResult<OperationSnapshot> {
    let caller = gui_caller(&state)?;
    let is_import = operations::is_import_operation(&state.db, &caller, &operation_ref)?;
    if is_import {
        crate::services::import_submission::import_ref(&state.db, &operation_ref)?;
    }
    let import_requires_source = if is_import {
        let db = std::sync::Arc::clone(&state.db);
        let caller = caller.clone();
        let operation_ref = operation_ref.clone();
        tauri::async_runtime::spawn_blocking(move || {
            operations::import_resume_requires_source(&db, &caller, &operation_ref)
        })
        .await
        .map_err(|error| AppError::msg(format!("导入恢复检查线程异常: {error}")))??
    } else {
        false
    };
    let snapshot = if is_import && import_requires_source {
        let picker_app = app.clone();
        let selected = tauri::async_runtime::spawn_blocking(move || {
            picker_app
                .dialog()
                .file()
                .set_title("重新授权原导入目录以继续")
                .blocking_pick_folder()
        })
        .await
        .map_err(|error| AppError::msg(format!("目录选择线程异常: {error}")))?;
        let Some(selected) = selected else {
            return Err(AppError::cancelled("已取消导入目录重新授权"));
        };
        let selected = selected
            .into_path()
            .map_err(|_| AppError::invalid_arg("所选目录不是本机文件系统路径"))?;
        let db = std::sync::Arc::clone(&state.db);
        let caller = caller.clone();
        let operation_ref = operation_ref.clone();
        tauri::async_runtime::spawn_blocking(move || {
            let grant = authorization::grant_selected_directory(
                &db,
                &caller,
                &selected,
                DirectoryPurpose::ImportSource,
                false,
            )?;
            let directory_ref = grant
                .directory_ref
                .ok_or_else(|| AppError::internal("导入目录授权缺少句柄"))?;
            operations::resume_import_operation(&db, &caller, &operation_ref, &directory_ref)
        })
        .await
        .map_err(|error| AppError::msg(format!("目录恢复线程异常: {error}")))??
    } else {
        operations::resume_operation(&state.db, &caller, &operation_ref)?
    };
    if is_import {
        super::import_cmd::wake_import_queue(app);
    }
    Ok(snapshot)
}

#[tauri::command]
pub fn list_operation_recovery(state: State<AppState>) -> AppResult<Vec<RecoverySummaryView>> {
    operations::recovery_summaries(&state.db)
}
