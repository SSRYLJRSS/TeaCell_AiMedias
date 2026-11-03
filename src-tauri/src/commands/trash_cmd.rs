//! 回收站与恢复的命令层（A4-03）。
//!
//! 恢复与移动/导出同构：选择快照 → 计划 → GUI 批准 → 提交 → 队列 → 逐项收据。
//! 区别在领域动作：清 `deleted_at` 之前必须证明文件仍在原位置且归属未变。
//!
//! 永久删除**不在**这里：它仍然是手动界面的独立确认（A4-04 会补独立审批），
//! 也刻意不暴露成 Agent 工具。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tauri::State;

use crate::contracts::agent::{OperationState, ProtocolVersion, ResourceRef, SubmitPlanInput};
use crate::contracts::caller::CallerContext;
use crate::db::assets::{self, TrashRecord};
use crate::db::libraries::{self, LibraryHealth, LibrarySnapshot};
use crate::db::operations as operations_db;
use crate::db::settings;
use crate::error::{AppError, AppResult};
use crate::services::authorization::{self, TrustedAdapterIdentity};
use crate::services::trash::{self, RestoreExecution, RestoreSummary};
use crate::services::{agent_tools, operations, plans, selections};
use crate::state::AppState;

/// 回收站清单上限：界面是分页浏览，不是一次性导出全部。
const TRASH_LIST_LIMIT: usize = 500;

/// 逐项恢复结果。`state` 可能为 `succeeded` / `failed` / `outcome_unknown`。
///
/// `outcome_unknown` 表示收尾阶段失败，无法证明记录是否真的回到库中；它既不能
/// 当成功（会让用户以为已恢复），也不能当失败（会误导用户原样重试）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreItemView {
    pub item_id: String,
    pub asset_id: i64,
    pub file_name: String,
    pub state: String,
    pub error_code: Option<String>,
    pub detail: Option<String>,
}

/// 一次恢复的完整结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreReport {
    pub operation_ref: String,
    pub restored: i64,
    pub failed: i64,
    /// 效果不确定的项数：必须提示核对，不能当普通失败处理。
    pub unknown: i64,
    pub skipped: i64,
    pub cancelled: bool,
    pub items: Vec<RestoreItemView>,
}

/// 逐项永久删除结果。
///
/// `outcome_unknown` 单列：它表示「文件可能已删但记录没同步」，与「明确失败」
/// 处置不同（前者需要用户核对磁盘，后者可以重试）。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyItemView {
    pub item_id: String,
    pub asset_id: i64,
    pub file_name: String,
    pub state: String,
    pub error_code: Option<String>,
    pub detail: Option<String>,
}

/// 一次永久清空的完整结果。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmptyReport {
    pub operation_ref: String,
    pub removed: i64,
    pub failed: i64,
    pub missing: i64,
    pub unknown: i64,
    pub skipped: i64,
    pub cancelled: bool,
    pub items: Vec<EmptyItemView>,
}

/// 回收站条目：界面要显示「何时移入」与「何时会被自动清理」。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TrashEntryView {
    pub asset_id: i64,
    pub file_name: String,
    pub file_path: String,
    pub deleted_at: i64,
    pub expires_at: i64,
    pub retention_days: i64,
}

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

fn ready_library(conn: &rusqlite::Connection) -> AppResult<LibrarySnapshot> {
    let library = libraries::current(conn)?.ok_or(AppError::from(
        crate::contracts::agent::AgentErrorCode::LibraryOffline,
    ))?;
    if library.health != LibraryHealth::Ready {
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::LibraryOffline,
        ));
    }
    Ok(library)
}

fn retention_days(conn: &rusqlite::Connection) -> AppResult<i64> {
    Ok(settings::get_settings(conn)?.trash_retention_days)
}

fn entry_view(record: TrashRecord, retention_days: i64) -> TrashEntryView {
    TrashEntryView {
        asset_id: record.asset_id,
        file_name: record.file_name,
        file_path: record.file_path,
        deleted_at: record.deleted_at,
        expires_at: trash::expires_at_ms(record.deleted_at, retention_days),
        retention_days,
    }
}

/// 回收站清单：含移入时间与预计自动清理时间。
///
/// 预计清理时间必须与 `purge_expired_trash` 用同一公式算，否则界面承诺的
/// 清理日会和真正清理的那天不一致。
#[tauri::command]
pub fn list_trash(state: State<AppState>) -> AppResult<Vec<TrashEntryView>> {
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    let days = retention_days(&conn)?;
    let records = assets::list_trashed(&conn, TRASH_LIST_LIMIT)?;
    Ok(records
        .into_iter()
        .map(|record| entry_view(record, days))
        .collect())
}

/// 指定素材的回收站信息（界面在选中项旁显示预计清理时间）。
#[tauri::command]
pub fn trash_retention(state: State<AppState>, ids: Vec<i64>) -> AppResult<Vec<TrashEntryView>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    let days = retention_days(&conn)?;
    let records = assets::trash_records(&conn, &ids)?;
    Ok(records
        .into_iter()
        .map(|record| entry_view(record, days))
        .collect())
}

/// 从回收站恢复：计划 → GUI 批准 → 提交 → 逐项核验后清 `deleted_at`。
///
/// 文件缺失、被替换或已不属于当前总仓的项会单独报告，绝不假装恢复成功。
#[tauri::command]
pub async fn restore_assets(
    state: State<'_, AppState>,
    asset_ids: Vec<i64>,
) -> AppResult<RestoreReport> {
    if asset_ids.is_empty() {
        return Err(AppError::invalid_arg("未选择任何素材"));
    }
    // A4-05：维护期间停止写入（恢复会重新核验并写定位证据）。
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "恢复素材")?;
    let db = Arc::clone(&state.db);
    let queue = Arc::clone(&state.operation_queue);
    let caller = gui_caller(&state)?;

    let selection = selections::store(
        &db,
        &caller,
        "从回收站恢复",
        &asset_ids,
        asset_ids.len() as i64,
    )?;
    let preview = agent_tools::prepare_restore(
        &db,
        &caller,
        &crate::contracts::agent::PrepareSelectionInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: selection.selection_ref.clone(),
        },
    )?;
    plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)?;
    let submission = agent_tools::submit_restore(
        &db,
        &caller,
        &SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: preview.plan_id.clone(),
            idempotency_key: ResourceRef::generated("idempotency"),
        },
    )?;
    let operation_id = submission.operation.operation_id.as_str().to_string();
    let operation_ref = ResourceRef::from_trusted(operation_id.clone()).map_err(AppError::from)?;
    // 恢复清单必须来自已批准的计划，不能按前端再次传来的 ids 重算。
    let approved_ids = agent_tools::restore_plan_items(&db, &preview.plan_id)?;
    let step_ref = {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        let steps = operations_db::steps(&conn, &operation_id)?;
        let first = steps
            .first()
            .ok_or_else(|| AppError::internal("恢复操作缺少步骤"))?;
        ResourceRef::from_trusted(first.step_id.clone()).map_err(AppError::from)?
    };

    let lease = queue
        .try_reserve_operation(&db, &operation_id, "assets.restore.submit")?
        .ok_or_else(|| {
            AppError::conflict("其他长任务正在执行或恢复操作尚未排到队首，请稍后重试")
        })?;
    let cancelled = AtomicBool::new(false);

    tauri::async_runtime::spawn_blocking(move || {
        let _lease = lease;
        let library = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            ready_library(&conn)?
        };
        let run = RestoreExecution {
            operation_id: &operation_id,
            operation_ref: &operation_ref,
            step_ref: &step_ref,
            asset_ids: &approved_ids,
            cancelled: &cancelled,
        };
        let summary = match trash::execute(&db, &caller, &library, run) {
            Ok(summary) => summary,
            Err(error) => {
                if let Err(settle) = operations::finish_operation(
                    &db,
                    &caller,
                    &operation_ref,
                    OperationState::Failed,
                ) {
                    tracing::error!(
                        operation_id = %operation_id,
                        error = %settle,
                        "恢复失败后结算操作状态失败"
                    );
                }
                return Err(error);
            }
        };
        finish_operation(&db, &caller, &operation_ref, &summary)?;
        let items = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            collect_items(&conn, &operation_id)?
        };
        Ok(RestoreReport {
            operation_ref: operation_ref.as_str().to_string(),
            restored: summary.restored,
            failed: summary.failed,
            unknown: summary.unknown,
            skipped: summary.skipped,
            cancelled: summary.cancelled,
            items,
        })
    })
    .await
    .map_err(|error| AppError::msg(format!("恢复线程异常: {error}")))?
}

/// 逐项结果决定操作终态：部分失败必须如实报 `partially_succeeded`。
fn finish_operation(
    db: &Arc<crate::state::Database>,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    summary: &RestoreSummary,
) -> AppResult<()> {
    if summary.cancelled {
        operations::finish_cancelled(db, caller, operation_ref)?;
        return Ok(());
    }
    let state = if summary.unknown > 0 {
        // 不可证实的效果优先：可能已恢复的项不能被压成「失败」。
        OperationState::OutcomeUnknown
    } else if summary.failed == 0 && summary.skipped == 0 {
        OperationState::Succeeded
    } else if summary.restored == 0 {
        OperationState::Failed
    } else {
        OperationState::PartiallySucceeded
    };
    operations::finish_operation(db, caller, operation_ref, state)?;
    Ok(())
}

/// 逐项结果从操作条目与收据重建：收据的 `db_ref` 带着给用户看的原因。
fn collect_items(
    conn: &rusqlite::Connection,
    operation_id: &str,
) -> AppResult<Vec<RestoreItemView>> {
    let mut views = Vec::new();
    for step in operations_db::steps(conn, operation_id)? {
        let receipts = operations_db::receipts(conn, operation_id, &step.step_id)?;
        for item in operations_db::items(conn, operation_id, &step.step_id)? {
            let asset_id =
                trash::restore_item_asset_id(&ResourceRef::from_trusted(item.item_id.clone())?)?;
            let detail = receipts
                .iter()
                .filter(|receipt| receipt.item_id.as_deref() == Some(item.item_id.as_str()))
                .filter_map(|receipt| receipt.db_ref.clone())
                .next_back();
            // 恢复成功后记录已不在回收站，文件名要从收据或条目本身拿；这里优先
            // 用回收站里的真实名字，取不到时退回占位文本而不是伪造一个文件名。
            let file_name = {
                let record = assets::trash_records(conn, &[asset_id])?;
                record
                    .into_iter()
                    .next()
                    .map(|record| record.file_name)
                    .unwrap_or_else(|| format!("素材 {asset_id}"))
            };
            views.push(RestoreItemView {
                item_id: item.item_id,
                asset_id,
                file_name,
                state: item.state,
                error_code: item.error_code,
                detail,
            });
        }
    }
    Ok(views)
}

/// 永久清空回收站中选中的 N 项（A4-04）。
///
/// 与恢复的两处关键差别：
///
/// 1. 审核类型必须是 `PermanentTrash`——`submit_empty` 会再核一次，普通计划
///    确认不能替代这里的确认。前端因此必须先弹「永久清空这 N 项」的独立确认，
///    不能复用打标/移动那种「确认计划」的按钮。
/// 2. 结果分四档：`removed` / `failed` / `missing` / `unknown`。`missing`
///    （文件早已不在）与 `unknown`（无法证明删除对象正确、或文件已删但记录
///    未同步）必须分开报告，绝不能合并成「成功」。
#[tauri::command]
pub async fn empty_trash(
    state: State<'_, AppState>,
    asset_ids: Vec<i64>,
) -> AppResult<EmptyReport> {
    if asset_ids.is_empty() {
        return Err(AppError::invalid_arg("未选择任何素材"));
    }
    // A4-05：维护期间停止写入。永久删除在总仓搬家中途执行，等于删掉尚未
    // 复制到新根的源文件；而核验失败时源文件正是唯一的回退依据。
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "永久清空回收站")?;
    let db = Arc::clone(&state.db);
    let queue = Arc::clone(&state.operation_queue);
    let caller = gui_caller(&state)?;

    let selection = selections::store(
        &db,
        &caller,
        "永久清空回收站",
        &asset_ids,
        asset_ids.len() as i64,
    )?;
    let preview = agent_tools::prepare_empty(
        &db,
        &caller,
        &crate::contracts::agent::PrepareSelectionInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: selection.selection_ref.clone(),
        },
    )?;
    plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)?;
    let submission = agent_tools::submit_empty(
        &db,
        &caller,
        &SubmitPlanInput {
            schema_version: ProtocolVersion::V1,
            plan_id: preview.plan_id.clone(),
            idempotency_key: ResourceRef::generated("idempotency"),
        },
    )?;
    let operation_id = submission.operation.operation_id.as_str().to_string();
    let operation_ref = ResourceRef::from_trusted(operation_id.clone()).map_err(AppError::from)?;
    // 清空清单必须来自已批准的计划：冻结之后新进回收站的项不能被旧确认带走。
    let approved_ids = agent_tools::empty_plan_items(&db, &preview.plan_id)?;
    let step_ref = {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        let steps = operations_db::steps(&conn, &operation_id)?;
        let first = steps
            .first()
            .ok_or_else(|| AppError::internal("清空操作缺少步骤"))?;
        ResourceRef::from_trusted(first.step_id.clone()).map_err(AppError::from)?
    };

    let lease = queue
        .try_reserve_operation(&db, &operation_id, "trash.empty.submit")?
        .ok_or_else(|| {
            AppError::conflict("其他长任务正在执行或清空操作尚未排到队首，请稍后重试")
        })?;
    let cancelled = AtomicBool::new(false);

    tauri::async_runtime::spawn_blocking(move || {
        let _lease = lease;
        let library = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            ready_library(&conn)?
        };
        let run = trash::EmptyExecution {
            operation_id: &operation_id,
            operation_ref: &operation_ref,
            step_ref: &step_ref,
            asset_ids: &approved_ids,
            cancelled: &cancelled,
        };
        let summary = match trash::empty(&db, &caller, &library, run) {
            Ok(summary) => summary,
            Err(error) => {
                if let Err(settle) = operations::finish_operation(
                    &db,
                    &caller,
                    &operation_ref,
                    OperationState::Failed,
                ) {
                    tracing::error!(
                        operation_id = %operation_id,
                        error = %settle,
                        "清空失败后结算操作状态失败"
                    );
                }
                return Err(error);
            }
        };
        finish_empty_operation(&db, &caller, &operation_ref, &summary)?;
        let items = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            collect_empty_items(&conn, &operation_id)?
        };
        Ok(EmptyReport {
            operation_ref: operation_ref.as_str().to_string(),
            removed: summary.removed,
            failed: summary.failed,
            missing: summary.missing,
            unknown: summary.unknown,
            skipped: summary.skipped,
            cancelled: summary.cancelled,
            items,
        })
    })
    .await
    .map_err(|error| AppError::msg(format!("清空线程异常: {error}")))?
}

/// 清空的操作终态：有 `unknown` 项时**不能**报成功。
///
/// `outcome_unknown` 意味着文件可能已删、记录未同步，此时宣称
/// `partially_succeeded` 会误导用户以为剩下的可以重试就恢复。直接落
/// `outcome_unknown`，让恢复摘要把它列为需要人工核对的异常。
fn finish_empty_operation(
    db: &Arc<crate::state::Database>,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    summary: &trash::DeletionSummary,
) -> AppResult<()> {
    if summary.cancelled {
        operations::finish_cancelled(db, caller, operation_ref)?;
        return Ok(());
    }
    let state = if summary.unknown > 0 {
        OperationState::OutcomeUnknown
    } else if summary.failed == 0 && summary.missing == 0 && summary.skipped == 0 {
        OperationState::Succeeded
    } else if summary.removed == 0 {
        OperationState::Failed
    } else {
        OperationState::PartiallySucceeded
    };
    operations::finish_operation(db, caller, operation_ref, state)?;
    Ok(())
}

/// 逐项清空结果：文件名在成功项上已经不可从回收站查到，所以优先用收据里
/// 的原因文本，取不到就用条目 id 反解出的素材 id 做占位。
fn collect_empty_items(
    conn: &rusqlite::Connection,
    operation_id: &str,
) -> AppResult<Vec<EmptyItemView>> {
    let mut views = Vec::new();
    for step in operations_db::steps(conn, operation_id)? {
        let receipts = operations_db::receipts(conn, operation_id, &step.step_id)?;
        for item in operations_db::items(conn, operation_id, &step.step_id)? {
            let asset_id =
                trash::deletion_item_asset_id(&ResourceRef::from_trusted(item.item_id.clone())?)?;
            let receipt = receipts
                .iter()
                .rfind(|receipt| receipt.item_id.as_deref() == Some(item.item_id.as_str()));
            let detail = receipt.and_then(|receipt| receipt.db_ref.clone());
            // 成功项的行已删除，回收站查不到名字；从收据的「已永久删除: X」里
            // 还原文件名比伪造「素材 N」对用户更可读。
            let file_name = detail
                .as_deref()
                .and_then(|text| text.split_once(": "))
                .map(|(_, name)| name.to_string())
                .unwrap_or_else(|| format!("素材 {asset_id}"));
            views.push(EmptyItemView {
                item_id: item.item_id,
                asset_id,
                file_name,
                state: item.state,
                error_code: item.error_code,
                detail,
            });
        }
    }
    Ok(views)
}
