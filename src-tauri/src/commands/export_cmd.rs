use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tauri::{AppHandle, Emitter, State};

use crate::contracts::agent::{
    ExportLayout, OperationState, PrepareExportInput, ProtocolVersion, ResourceRef, SubmitPlanInput,
};
use crate::contracts::caller::CallerContext;
use crate::db::export::{self, ExportTask};
use crate::db::export_items::{self, ExportCounts, ExportItem};
use crate::db::operations as operations_db;
use crate::error::{AppError, AppResult};
use crate::services::authorization::{self, TrustedAdapterIdentity};
use crate::services::export_local::{self, ExportProgress, ExportRun};
use crate::services::{agent_tools, operations, plans, selections};
use crate::state::AppState;

/// 库外移动的固定拒绝文案。A4-01 决策：导出统一为「复制」，库内原文件、原路径、
/// 标签必须保留；旧调用方传 `mode="move"` 时返回这条明确错误，而不是静默按复制执行。
pub const OUT_OF_LIBRARY_MOVE_REJECTED: &str =
    "导出已统一为复制：不会移动或删除库内原文件。请改用「复制导出」，库内移动请使用移动功能。";

/// 旧前端只传 `destDir`、未走目录授权时的固定拒绝文案。目标目录必须先经
/// `pick_directory_grant` 取得一次性/记住的授权，命令层不接受任意裸路径。
pub const UNGRANTED_DESTINATION_REJECTED: &str =
    "导出目标目录需要先授权：请在导出对话框里选择目录，应用会记录本次授权。";

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

fn parse_layout(raw: Option<&str>) -> AppResult<(ExportLayout, &'static str)> {
    match raw.unwrap_or("flat") {
        "flat" => Ok((ExportLayout::Flat, "flat")),
        "by_tag" => Ok((ExportLayout::ByTag, "by_tag")),
        "by_date" => Ok((ExportLayout::ByDate, "by_date")),
        _ => Err(AppError::invalid_arg("非法目录组织")),
    }
}

/// 导出模式闸门：`move` 必须拿到明确错误，而不是被静默当成复制执行——
/// 静默降级会让旧调用方误以为库内记录已跟随文件移动（A4-01 第 1 条）。
fn ensure_copy_mode(mode: &str) -> AppResult<()> {
    if mode == "move" {
        return Err(AppError::unsupported(OUT_OF_LIBRARY_MOVE_REJECTED));
    }
    if mode != "copy" {
        return Err(AppError::invalid_arg("非法导出模式"));
    }
    Ok(())
}

/// 逐文件结果 → operation 收据的状态映射。跳过项由 `finish_cancelled` 统一结算，
/// 这里不再单独写一遍，避免同一 item 两条终态收据。
fn item_settlement(
    row: &export_items::NewExportItem,
) -> Option<(&'static str, &'static str, Option<&str>, &'static str)> {
    match row.state.as_str() {
        export_items::STATE_COPIED => Some(("succeeded", "published", None, "copied")),
        export_items::STATE_FAILED => {
            Some(("failed", "none", row.error_code.as_deref(), "copy_failed"))
        }
        _ => None,
    }
}

/// 本地「复制导出」：选择快照 → 计划 → GUI 批准 → 提交 → 队列 → 逐文件收据。
///
/// A4-01：目标目录必须是已授权的 `export_destination` 句柄（不接受裸路径），
/// `mode` 只接受 `copy`；库内原文件、原路径、标签全部保留。逐文件结果写入
/// `export_items` 与 operation 收据，完成后由 `export_report` 一次性取回。
#[tauri::command]
pub async fn export_local_files(
    app: AppHandle,
    state: State<'_, AppState>,
    asset_ids: Vec<i64>,
    directory_ref: String,
    mode: String,
    layout: Option<String>,
) -> AppResult<ExportTask> {
    ensure_copy_mode(&mode)?;
    let (export_layout, layout_key) = parse_layout(layout.as_deref())?;
    if asset_ids.is_empty() {
        return Err(AppError::invalid_arg("未选择任何素材"));
    }
    if directory_ref.trim().is_empty() {
        return Err(AppError::unsupported(UNGRANTED_DESTINATION_REJECTED));
    }
    // A4-05：维护期间停止写入。导出读的是总仓内文件，迁仓正把这些文件搬走，
    // 此时复制出的结果可能缺项。
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "复制导出")?;
    let directory_ref = ResourceRef::from_trusted(directory_ref).map_err(AppError::from)?;

    let db = std::sync::Arc::clone(&state.db);
    let registry = std::sync::Arc::clone(&state.export_cancel);
    let queue = std::sync::Arc::clone(&state.operation_queue);
    let caller = gui_caller(&state)?;

    // 1) 把本次选中的素材固定成快照；2) 生成并当场批准计划（GUI 自批准，
    // 与导入确认同一条路径）；3) 提交计划，注册 operation 与逐素材条目。
    let selection = selections::store(
        &db,
        &caller,
        "导出选中素材",
        &asset_ids,
        asset_ids.len() as i64,
    )?;
    let preview = agent_tools::prepare_export(
        &db,
        &caller,
        &PrepareExportInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: selection.selection_ref,
            destination_ref: directory_ref.clone(),
            layout: export_layout,
        },
    )?;
    plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)?;
    let submission = agent_tools::submit_export(
        &db,
        &caller,
        &SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: preview.plan_id,
            idempotency_key: ResourceRef::generated("idempotency"),
        },
    )?;
    let operation_id = submission.operation.operation_id.as_str().to_string();
    let operation_ref = ResourceRef::from_trusted(operation_id.clone()).map_err(AppError::from)?;
    let step_ref = {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        let steps = operations_db::steps(&conn, &operation_id)?;
        let first = steps
            .first()
            .ok_or_else(|| AppError::internal("导出操作缺少步骤"))?;
        ResourceRef::from_trusted(first.step_id.clone()).map_err(AppError::from)?
    };

    let lease = queue
        .try_reserve_operation(&db, &operation_id, "exports.submit")?
        .ok_or_else(|| {
            AppError::conflict("其他长任务正在执行或导出操作尚未排到队首，请稍后重试")
        })?;
    let cancel = Arc::new(AtomicBool::new(false));

    tauri::async_runtime::spawn_blocking(move || {
        let _lease = lease;
        // 目标根目录从授权句柄重新解析：不信任前端传来的裸路径，也顺带在
        // 复制前再校验一次授权代次与文件系统身份。
        let dest_dir = {
            let path = authorization::resolve_export_directory(&db, &caller, &directory_ref)?;
            crate::utils::path::encode_native_path(&path)?
        };
        // 建任务短锁即用即放；导出期间不持 DB 锁（文件 IO 在锁外）
        let task = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            export::create_task(
                &conn,
                "local",
                asset_ids.len() as i64,
                Some(&dest_dir),
                None,
            )?
        };
        registry
            .lock()
            .map_err(|_| AppError::msg("锁中毒"))?
            .insert(task.id, Arc::clone(&cancel));

        let mut on_item = |row: &export_items::NewExportItem| -> AppResult<()> {
            // 跳过项（取消时补齐）由 finish_cancelled 统一结算，避免同一 item
            // 出现两条互相矛盾的终态收据。
            let Some((item_state, effect_state, error_code, event)) = item_settlement(row) else {
                return Ok(());
            };
            let Some(asset_id) = row.asset_id else {
                return Ok(());
            };
            let item_ref = ResourceRef::from_trusted(agent_tools::export_item_id(asset_id))
                .map_err(AppError::from)?;
            operations::record_item_state(
                &db,
                &caller,
                &operation_ref,
                &step_ref,
                &item_ref,
                item_state,
                effect_state,
                error_code,
                &ResourceRef::generated("receipt"),
                event,
                row.dest_path.as_deref(),
            )
        };
        let run = ExportRun {
            task_id: task.id,
            asset_ids: &asset_ids,
            dest_dir: &dest_dir,
            layout: layout_key,
            cancel: &cancel,
        };
        let result = export_local::export_local(
            &db,
            run,
            |p: ExportProgress| {
                let _ = app.emit("export://progress", p);
            },
            &mut on_item,
        );

        // B12：收尾清理 flag——锁中毒不再静默吞
        match registry.lock() {
            Ok(mut m) => {
                m.remove(&task.id);
            }
            Err(_) => tracing::error!("取消注册表锁中毒，task {} 的 flag 未清理", task.id),
        }

        let summary = match result {
            Ok(summary) => summary,
            Err(error) => {
                // 中途硬失败（例如目标根目录不可写）：不能把 operation 留在
                // running，否则恢复链会一直等待一个已经不存在的执行者。
                if let Err(settle) = operations::finish_operation(
                    &db,
                    &caller,
                    &operation_ref,
                    OperationState::Failed,
                ) {
                    tracing::error!(
                        operation_id = %operation_id,
                        error = %settle,
                        "导出失败后结算操作状态失败"
                    );
                }
                return Err(error);
            }
        };
        // 只有已落盘且核验字节数成功的文件才计为复制成功；取消走统一的
        // cancelled 结算，把仍未开始的条目一并标成取消收据。
        if summary.cancelled {
            operations::finish_cancelled(&db, &caller, &operation_ref)?;
        } else {
            let state = if summary.failed == 0 && summary.skipped == 0 {
                OperationState::Succeeded
            } else if summary.copied == 0 {
                OperationState::Failed
            } else {
                OperationState::PartiallySucceeded
            };
            operations::finish_operation(&db, &caller, &operation_ref, state)?;
        }
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        export::get_task(&conn, task.id)
    })
    .await
    .map_err(|e| AppError::msg(format!("导出线程异常: {e}")))?
}

#[tauri::command]
pub fn list_export_tasks(state: State<AppState>) -> AppResult<Vec<ExportTask>> {
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    export::list_tasks(&conn)
}

/// 一次导出任务的完整结果：任务状态 + 逐文件计数 + 逐文件明细（A4-01）。
///
/// 完成摘要要同时给出「目标位置」和「失败项」，所以三部分一起返回，避免前端
/// 为了对齐计数再发两次请求、或在两次请求之间看到不一致的数字。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportReport {
    pub task: ExportTask,
    pub counts: ExportCounts,
    pub items: Vec<ExportItem>,
}

#[tauri::command]
pub fn export_report(state: State<AppState>, task_id: i64) -> AppResult<ExportReport> {
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    let task = export::get_task(&conn, task_id)?;
    let counts = export_items::counts(&conn, task_id)?;
    let items = export_items::items(&conn, task_id)?;
    Ok(ExportReport {
        task,
        counts,
        items,
    })
}

/// R-26 CSV 清单导出：短锁内完成（纯读库 + 单文件写入，毫秒级），返回清单路径。
///
/// A4-01：目标同样是已授权的 `export_destination` 句柄，命令层不接受裸路径，
/// 与文件复制共用同一次目录授权，避免出现「文件走授权、清单走任意路径」的缺口。
#[tauri::command]
pub fn export_csv_manifest(
    state: State<AppState>,
    asset_ids: Vec<i64>,
    directory_ref: String,
) -> AppResult<String> {
    if asset_ids.is_empty() {
        return Err(AppError::invalid_arg("未选择任何素材"));
    }
    if directory_ref.trim().is_empty() {
        return Err(AppError::unsupported(UNGRANTED_DESTINATION_REJECTED));
    }
    // A4-05：维护期间停止写入。导出读的是总仓内文件，迁仓正把这些文件搬走，
    // 此时复制出的结果可能缺项。
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "复制导出")?;
    let directory_ref = ResourceRef::from_trusted(directory_ref).map_err(AppError::from)?;
    let caller = gui_caller(&state)?;
    let dest_dir = authorization::resolve_export_directory(&state.db, &caller, &directory_ref)?;
    let dest_dir = crate::utils::path::encode_native_path(&dest_dir)?;
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    let out = export_local::write_csv_manifest(&conn, &asset_ids, &dest_dir)?;
    crate::utils::path::encode_native_path(&out)
}

#[tauri::command]
pub fn cancel_export(state: State<AppState>, task_id: i64) -> AppResult<()> {
    // B11：不再静默吞锁中毒
    let m = state
        .export_cancel
        .lock()
        .map_err(|_| AppError::msg("取消注册表锁中毒，无法取消任务"))?;
    if let Some(flag) = m.get(&task_id) {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // A4-01 第 1 条验收：「旧请求 move=true 指向库外时的明确错误；不能只隐藏界面按钮」。
    // 前端已经没有 move 入口（`src/api/export.ts` 的类型只有 "copy"），所以这条拒绝
    // 只能靠命令层闸门兑现——必须有一等证据证明旧调用方真的被后端挡住。
    #[test]
    fn move_mode_is_rejected_by_the_command_gate_not_silently_copied() {
        let error = ensure_copy_mode("move").expect_err("move 必须被拒绝");
        assert_eq!(
            error.code(),
            "UNSUPPORTED",
            "拒绝要用「不支持」而不是参数错误：这是产品决策变更，不是调用方笔误"
        );
        assert_eq!(
            error.to_string(),
            OUT_OF_LIBRARY_MOVE_REJECTED,
            "文案要明确「已统一为复制、库内文件保留」，并指向正确的替代入口"
        );
        // 关键：不能静默降级成复制——静默成功会让旧调用方以为库内记录已跟随文件移动
        assert!(ensure_copy_mode("move").is_err());
    }

    #[test]
    fn copy_mode_passes_and_unknown_modes_are_rejected() {
        assert!(ensure_copy_mode("copy").is_ok());
        for bad in ["", "COPY", "Move", "mv", "cut"] {
            let error = ensure_copy_mode(bad).expect_err("非法模式必须被拒绝");
            assert_eq!(error.code(), "INVALID_ARG", "模式 {bad:?}");
        }
    }

    // 未授权的裸路径同样必须由后端拒绝：前端不传 directory_ref 时不能退化成
    // 「写到任意路径」。这里只覆盖闸门的判定函数本身，命令体需要 AppState。
    #[test]
    fn layout_parsing_rejects_unknown_values() {
        assert!(parse_layout(None).is_ok());
        assert!(parse_layout(Some("flat")).is_ok());
        assert!(parse_layout(Some("by_tag")).is_ok());
        assert!(parse_layout(Some("by_date")).is_ok());
        let error = parse_layout(Some("by_album")).expect_err("未知目录组织必须被拒绝");
        assert_eq!(error.code(), "INVALID_ARG");
    }
}
