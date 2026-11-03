//! 库内分库移动的命令层（A4-02）。
//!
//! 与 A4-01 的复制导出同构：选择快照 → 计划 → GUI 批准 → 提交 → 队列 → 逐文件收据。
//! 区别只在领域动作——这里搬的是总仓内的文件，素材 ID、标签与历史全部保留。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tauri::State;

use crate::contracts::agent::{
    OperationState, PrepareMoveInput, ProtocolVersion, ResourceRef, SubmitPlanInput,
};
use crate::contracts::caller::CallerContext;
use crate::db::libraries::{self, LibraryHealth, LibrarySnapshot};
use crate::db::move_journal::{self, MoveJournalEntry};
use crate::db::operations as operations_db;
use crate::error::{AppError, AppResult};
use crate::services::authorization::{self, TrustedAdapterIdentity};
use crate::services::collection_targets::{self, CollectionTarget};
use crate::services::library_move::{self, MoveExecution, MoveSummary};
use crate::services::{agent_tools, operations, plans, selections};
use crate::state::AppState;

/// 逐文件移动结果。`state` 取 `move_journal` 的五态，前端按码给文案。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveItemView {
    pub item_id: String,
    pub asset_id: i64,
    pub file_name: String,
    pub state: String,
    pub error_code: Option<String>,
    pub source_path: String,
    pub target_path: String,
}

/// 一次移动的完整结果：目标分库、逐文件计数与逐文件明细。
///
/// `unknown` 与 `failed` 分开：`unknown` 表示文件效果可能已发生但证据不完整，
/// 前端必须提示核对，不能当成普通失败把选择清空。
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveReport {
    pub operation_ref: String,
    pub collection_path: String,
    pub moved: i64,
    pub failed: i64,
    pub unknown: i64,
    pub skipped: i64,
    pub cancelled: bool,
    pub items: Vec<MoveItemView>,
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

fn now_unix_seconds() -> AppResult<i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::internal("系统时间早于 Unix 纪元"))?;
    i64::try_from(now.as_secs()).map_err(|_| AppError::internal("系统时间超出范围"))
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

/// 总仓内已有的目录（含根目录本身），登记成分库引用供前端选择。
///
/// 只读磁盘、不创建任何东西；真正的目标目录在移动执行时才按需创建。目录遍历、
/// 保留目录过滤、深度与数量上限、引用登记都在 `services::collection_targets`——
/// 它们是业务规则，不是命令层的职责。
#[tauri::command]
pub fn list_move_targets(state: State<AppState>) -> AppResult<Vec<CollectionTarget>> {
    let library = {
        let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        ready_library(&conn)?
    };
    collection_targets::list(&state.db, &library, now_unix_seconds()?)
}

/// 库内分库移动：选择快照 → 计划 → GUI 批准 → 提交 → 队列 → 逐文件改名与收据。
#[tauri::command]
pub async fn move_assets(
    state: State<'_, AppState>,
    asset_ids: Vec<i64>,
    collection_ref: String,
) -> AppResult<MoveReport> {
    if asset_ids.is_empty() {
        return Err(AppError::invalid_arg("未选择任何素材"));
    }
    if collection_ref.trim().is_empty() {
        return Err(AppError::invalid_arg("未选择目标分库"));
    }
    // A4-05：维护期间停止写入（移动会改素材定位路径）。
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "移动素材")?;
    let collection_ref = ResourceRef::from_trusted(collection_ref).map_err(AppError::from)?;

    let db = Arc::clone(&state.db);
    let queue = Arc::clone(&state.operation_queue);
    let caller = gui_caller(&state)?;

    let target = {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        let library = ready_library(&conn)?;
        library_move::resolve_target(&conn, &library, &collection_ref)?
    };

    let selection = selections::store(
        &db,
        &caller,
        "移动到分库",
        &asset_ids,
        asset_ids.len() as i64,
    )?;
    let preview = agent_tools::prepare_move(
        &db,
        &caller,
        &PrepareMoveInput {
            schema_version: ProtocolVersion::V1,
            selection_ref: selection.selection_ref.clone(),
            collection_ref: collection_ref.clone(),
        },
    )?;
    plans::approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref)?;
    let submission = agent_tools::submit_move(
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
    // 逐文件目标必须来自已批准的计划：重新推导会挑到另一个同名后缀。
    let items = agent_tools::move_plan_items(&db, &preview.plan_id)?;
    let step_ref = {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        let steps = operations_db::steps(&conn, &operation_id)?;
        let first = steps
            .first()
            .ok_or_else(|| AppError::internal("移动操作缺少步骤"))?;
        ResourceRef::from_trusted(first.step_id.clone()).map_err(AppError::from)?
    };

    let lease = queue
        .try_reserve_operation(&db, &operation_id, "assets.move.submit")?
        .ok_or_else(|| {
            AppError::conflict("其他长任务正在执行或移动操作尚未排到队首，请稍后重试")
        })?;
    let cancelled = AtomicBool::new(false);
    let collection_path = target.relative_path.clone();

    tauri::async_runtime::spawn_blocking(move || {
        let _lease = lease;
        let library = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            ready_library(&conn)?
        };
        let run = MoveExecution {
            operation_id: &operation_id,
            operation_ref: &operation_ref,
            step_ref: &step_ref,
            items: &items,
            cancelled: &cancelled,
        };
        let summary = match library_move::execute(&db, &caller, &library, run) {
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
                        "移动失败后结算操作状态失败"
                    );
                }
                return Err(error);
            }
        };
        finish_operation(&db, &caller, &operation_ref, &summary)?;
        let entries = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            move_journal::entries(&conn, &operation_id)?
        };
        Ok(MoveReport {
            operation_ref: operation_ref.as_str().to_string(),
            collection_path: collection_path.clone(),
            moved: summary.moved,
            failed: summary.failed,
            unknown: summary.unknown,
            skipped: summary.skipped,
            cancelled: summary.cancelled,
            items: entries.into_iter().map(item_view).collect(),
        })
    })
    .await
    .map_err(|error| AppError::msg(format!("移动线程异常: {error}")))?
}

/// 逐文件结果决定操作终态：部分失败必须如实报 `partially_succeeded`，
/// 未知效果优先落 `outcome_unknown`（与 dispatcher、清空回收站的判据保持一致）。
fn finish_operation(
    db: &Arc<crate::state::Database>,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    summary: &MoveSummary,
) -> AppResult<()> {
    if summary.cancelled {
        operations::finish_cancelled(db, caller, operation_ref)?;
        return Ok(());
    }
    let state = if summary.unknown > 0 {
        OperationState::OutcomeUnknown
    } else if summary.failed == 0 && summary.skipped == 0 {
        OperationState::Succeeded
    } else if summary.moved == 0 {
        OperationState::Failed
    } else {
        OperationState::PartiallySucceeded
    };
    operations::finish_operation(db, caller, operation_ref, state)?;
    Ok(())
}

fn item_view(entry: MoveJournalEntry) -> MoveItemView {
    MoveItemView {
        item_id: entry.item_id,
        asset_id: entry.asset_id,
        file_name: entry
            .target_path
            .rsplit('/')
            .next()
            .unwrap_or(&entry.target_path)
            .to_string(),
        state: entry.state,
        error_code: entry.error_code,
        source_path: entry.source_path,
        target_path: entry.target_path,
    }
}
