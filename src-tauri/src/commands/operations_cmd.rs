//! GUI entry points for cancel/resume and startup recovery summaries.

use tauri::{AppHandle, Emitter, State};

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
    // A4-05/A3-03：维护门必须在执行前判，而不是只在排队时判。
    // 排队时未维护、点「继续」时总仓已在搬家的情形，排队那一刻的检查挡不住。
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "继续任务")?;
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

/// 用户明确点击「开始执行」时，真正把已批准并已提交的操作跑掉。
///
/// 为什么需要这一层：提交（`submit_*`）只写库不碰文件系统，而 GUI 的五个命令
/// 各自在自己内部完成文件效果。Agent 提交的操作没有任何调用方替它继续，于是
/// 永远停在 `queued`。这里补上同一件事，且保持「批准 → 提交 → 显式启动」三段式：
/// 模型提交后必须有人点这个按钮才会有文件效果。
#[tauri::command]
pub async fn start_operation(
    app: AppHandle,
    state: State<'_, AppState>,
    operation_ref: ResourceRef,
) -> AppResult<OperationSnapshot> {
    // A4-05/A3-03：入口处统一判维护门。这个命令会对总仓产生真实文件效果，
    // 排队与批准都发生在更早的时刻，不能代表此刻允许写入。
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "开始任务")?;
    let db = std::sync::Arc::clone(&state.db);
    let queue = std::sync::Arc::clone(&state.operation_queue);
    let export_cancel = std::sync::Arc::clone(&state.export_cancel);
    tauri::async_runtime::spawn_blocking(move || {
        let emit_app = app.clone();
        let on_progress = move |progress: crate::services::export_local::ExportProgress| {
            let _ = emit_app.emit("export://progress", progress);
        };
        crate::services::operation_dispatch::start(
            &db,
            &operation_ref,
            &queue,
            &export_cancel,
            &on_progress,
        )
    })
    .await
    .map_err(|error| AppError::msg(format!("执行线程异常: {error}")))?
}
