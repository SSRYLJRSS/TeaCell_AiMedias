//! 回收站服务（A4-03）：软删除计划、恢复前的文件与归属核验、逐项收据。
//!
//! 两条不变量：
//!
//! 1. 移入回收站只是软删除。计划必须能列出具体素材与预计自动清理时间，
//!    永久删除仍然只存在于手动界面的独立确认里，不暴露成 Agent 工具。
//! 2. 恢复不是「把 `deleted_at` 清空就算完」：文件必须仍在本机原位置、
//!    仍属于当前总仓、身份未变。任一条件不满足就报可修复异常，绝不把
//!    已经失效的定位重新当成在库素材。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::contracts::agent::{AgentErrorCode, ResourceRef};
use crate::contracts::caller::CallerContext;
use crate::db::assets::{self, TrashRecord};
use crate::db::deletion_journal;
use crate::db::libraries::{self, LibrarySnapshot};
use crate::db::operations as operations_db;
use crate::db::plans as plans_db;
use crate::error::{AppError, AppResult};
use crate::services::managed_library;
use crate::services::operations;
use crate::state::Database;
use crate::utils::file_identity::file_identity;

/// 恢复逐项失败码。恢复不是文件移动，所以不复用移动日志的那套码。
pub const CODE_FILE_MISSING: &str = "FILE_MISSING";
pub const CODE_FILE_CHANGED: &str = "FILE_CHANGED";
pub const CODE_OUTSIDE_LIBRARY: &str = "OUTSIDE_LIBRARY";
pub const CODE_LIBRARY_CHANGED: &str = "LIBRARY_CHANGED";
pub const CODE_NOT_IN_TRASH: &str = "NOT_IN_TRASH";
pub const CODE_PERMISSION_DENIED: &str = "PERMISSION_DENIED";
pub const CODE_INTERRUPTED: &str = "INTERRUPTED";
/// 永久删除专用：证明不了这条路径仍属于该素材时，不删只报告。
pub const CODE_OWNERSHIP_UNVERIFIED: &str = "OWNERSHIP_UNVERIFIED";
pub const CODE_IO_ERROR: &str = "IO_ERROR";

/// 恢复项的操作条目 id 前缀。提交时登记、执行时按同一 id 写收据。
pub fn restore_item_id(asset_id: i64) -> String {
    format!("restore_{asset_id:016}")
}

pub fn restore_item_asset_id(item_ref: &ResourceRef) -> AppResult<i64> {
    item_ref
        .as_str()
        .strip_prefix("restore_")
        .and_then(|rest| rest.parse::<i64>().ok())
        .ok_or_else(|| AppError::internal("恢复项引用格式无效"))
}

/// 预计自动清理时间（毫秒）：软删除时间 + 保留天数。
///
/// 与 `commands::purge_expired_trash` 用同一条公式，避免界面显示的时间与
/// 后台真正清理的时间对不上。
pub fn expires_at_ms(deleted_at_ms: i64, retention_days: i64) -> i64 {
    deleted_at_ms.saturating_add(retention_days.saturating_mul(86_400_000))
}

/// 把毫秒时间戳格式成本地日期，给计划摘要与界面提示用。
pub fn format_day(millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(millis)
        .map(|timestamp| {
            timestamp
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        })
        .unwrap_or_else(|| "未知日期".to_string())
}

/// 一次恢复执行的输入。
pub struct RestoreExecution<'a> {
    pub operation_id: &'a str,
    pub operation_ref: &'a ResourceRef,
    pub step_ref: &'a ResourceRef,
    pub asset_ids: &'a [i64],
    pub cancelled: &'a AtomicBool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RestoreSummary {
    pub restored: i64,
    pub failed: i64,
    /// 效果不确定：提交阶段失败，无法证明 `deleted_at` 是否真的被清空。
    ///
    /// 必须与 `failed` 分开。`failed` 的语义是「记录确定还在回收站，可以原样重试」，
    /// 而这一项可能已经恢复成功，当成失败会让用户重试后看到「已被其他操作处理」。
    pub unknown: i64,
    pub skipped: i64,
    pub cancelled: bool,
}

impl RestoreSummary {
    fn empty() -> Self {
        Self::default()
    }
}

/// 恢复前的核验结论。只有 `Restorable` 才允许清 `deleted_at`。
enum RestoreCheck {
    Restorable,
    Failed(&'static str, String),
}

/// 逐项恢复：先核验文件与归属，再在一个事务里清 `deleted_at` 并写收据。
pub fn execute(
    db: &Database,
    caller: &CallerContext,
    library: &LibrarySnapshot,
    run: RestoreExecution<'_>,
) -> AppResult<RestoreSummary> {
    let mut summary = RestoreSummary::empty();
    let root = managed_library::verified_root_path(library)?;
    for &asset_id in run.asset_ids {
        if run.cancelled.load(Ordering::Relaxed) || cancel_requested(db, run.operation_id)? {
            summary.cancelled = true;
            summary.skipped += 1;
            continue;
        }
        let item_ref = ResourceRef::from_trusted(restore_item_id(asset_id))?;

        // 记录必须在回收站里，且归属证据仍然完好。
        let evidence = {
            let conn = db.lock()?;
            let record = assets::trash_records(&conn, &[asset_id])?
                .into_iter()
                .next()
                .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired));
            match record {
                Ok(record) => {
                    let location = libraries::verified_location(&conn, asset_id)?;
                    Some((record, location))
                }
                Err(_) => None,
            }
        };
        let Some((record, location)) = evidence else {
            record_item(
                db,
                caller,
                &run,
                &item_ref,
                "failed",
                CODE_NOT_IN_TRASH,
                "记录已不在回收站，可能已被其他操作恢复或清理",
            )?;
            summary.failed += 1;
            continue;
        };
        let check = match location {
            Some(location) => check_restorable(
                library,
                &root,
                &location.relative_path,
                &location.file_identity,
                &record.file_name,
            ),
            None => RestoreCheck::Failed(
                CODE_LIBRARY_CHANGED,
                "该素材的定位证据已失效，请先在素材库中重新核验总仓".to_string(),
            ),
        };
        if let RestoreCheck::Failed(code, message) = check {
            record_item(
                db,
                caller,
                &run,
                &item_ref,
                "failed",
                code,
                message.as_str(),
            )?;
            summary.failed += 1;
            continue;
        }

        // 清 `deleted_at` 与写成功收据必须落在同一个事务里。
        //
        // 分成两个事务会留一个致命窗口：收据写失败时记录已经恢复，但操作项还是
        // pending，重启对账只能把「已恢复」报成失败，用户重试后又会看到
        // 「记录已被其他操作处理」。合并后要么两者都成立，要么都回滚。
        let mut commit_attempted = false;
        let restored_detail = format!("已恢复到原位置: {}", record.file_name);
        let outcome = (|| -> AppResult<()> {
            let conn = db.lock()?;
            let transaction = conn.unchecked_transaction()?;
            let restored = assets::restore(&transaction, &[asset_id])?;
            if restored != 1 {
                // 另一个入口先恢复了它：本次不重复计账，也不用假成功掩盖竞争。
                return Err(AppError::from(AgentErrorCode::PlanChanged));
            }
            operations::record_item_state_in(
                &transaction,
                run.operation_ref,
                run.step_ref,
                &item_ref,
                "succeeded",
                "registered",
                None,
                &ResourceRef::generated("receipt"),
                "restored",
                Some(restored_detail.as_str()),
            )?;
            // 到这里所有写入都已成功，只剩提交。此后失败就无法再证明
            // `deleted_at` 到底有没有落盘，只能记 outcome_unknown。
            commit_attempted = true;
            transaction.commit()?;
            Ok(())
        })();

        match outcome {
            Ok(()) => {
                summary.restored += 1;
                continue;
            }
            Err(error) if commit_attempted => {
                record_item(
                    db,
                    caller,
                    &run,
                    &item_ref,
                    "outcome_unknown",
                    CODE_INTERRUPTED,
                    &format!("恢复收尾失败，无法确认该记录是否已回到库中: {error}"),
                )?;
                summary.unknown += 1;
                continue;
            }
            Err(error) => {
                // 事务已回滚：`deleted_at` 一定没变，可以放心说「仍在回收站」。
                let (code, message) = if error.code() == AgentErrorCode::PlanChanged.as_str() {
                    (
                        CODE_NOT_IN_TRASH,
                        "记录已被其他操作处理（可能已恢复或已被清理）".to_string(),
                    )
                } else {
                    (
                        CODE_PERMISSION_DENIED,
                        format!("恢复失败，记录仍留在回收站: {error}"),
                    )
                };
                record_item(
                    db,
                    caller,
                    &run,
                    &item_ref,
                    "failed",
                    code,
                    message.as_str(),
                )?;
                summary.failed += 1;
                continue;
            }
        }
    }
    Ok(summary)
}

/// 文件必须仍在本机原位置、是普通文件、身份与核验证据一致、且在本总仓内。
fn check_restorable(
    library: &LibrarySnapshot,
    root: &Path,
    relative_path: &str,
    expected_identity: &str,
    file_name: &str,
) -> RestoreCheck {
    let path = root.join(relative_path.replace('/', std::path::MAIN_SEPARATOR_STR));
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_dir() => {
            return RestoreCheck::Failed(
                CODE_FILE_CHANGED,
                format!("「{file_name}」的原位置已不是普通文件，请重新核验后再恢复"),
            );
        }
        Ok(_) => {}
        Err(_) => {
            return RestoreCheck::Failed(
                CODE_FILE_MISSING,
                format!(
                    "「{file_name}」的文件已不在原位置：可把文件放回原位再恢复，或直接从素材库移出"
                ),
            );
        }
    }
    // 校验的是文件自身而不是它的父目录：父目录可能就是总仓根，而空相对路径
    // 在 `validate_import_target` 里是非法的（它按「待登记的文件」设计）。
    if let Err(error) = managed_library::validate_import_target(library, &path) {
        return RestoreCheck::Failed(
            CODE_OUTSIDE_LIBRARY,
            format!("「{file_name}」的恢复位置不在当前总仓内: {error}"),
        );
    }
    match std::fs::File::open(&path) {
        Ok(opened) => match file_identity(&opened) {
            Ok(identity) if identity == expected_identity => RestoreCheck::Restorable,
            Ok(_) => RestoreCheck::Failed(
                CODE_FILE_CHANGED,
                format!("「{file_name}」的原位置已被同名文件替换，请重新核验后再恢复"),
            ),
            Err(error) => RestoreCheck::Failed(
                CODE_PERMISSION_DENIED,
                format!("无法读取「{file_name}」: {error}"),
            ),
        },
        Err(error) => RestoreCheck::Failed(
            CODE_PERMISSION_DENIED,
            format!("无法打开「{file_name}」: {error}"),
        ),
    }
}

fn record_item(
    db: &Database,
    caller: &CallerContext,
    run: &RestoreExecution<'_>,
    item_ref: &ResourceRef,
    state: &str,
    error_code: &str,
    detail: &str,
) -> AppResult<()> {
    let (effect_state, code, event) = match state {
        "succeeded" => ("registered", None, "restored"),
        // 效果不确定：`deleted_at` 可能已经被清空。必须留成 unknown，
        // 不能压成 failed 让用户以为「确定还在回收站」。
        "outcome_unknown" => ("unknown", Some(CODE_INTERRUPTED), "restore_outcome_unknown"),
        _ => ("none", Some(error_code), "restore_failed"),
    };
    operations::record_item_state(
        db,
        caller,
        run.operation_ref,
        run.step_ref,
        item_ref,
        state,
        effect_state,
        code,
        &ResourceRef::generated("receipt"),
        event,
        if detail.is_empty() {
            None
        } else {
            Some(detail)
        },
    )
}

/// 操作记录上是否已经收到取消请求（与移动执行同一条判据）。
fn cancel_requested(db: &Database, operation_id: &str) -> AppResult<bool> {
    let conn = db.lock()?;
    let Some(record) = plans_db::operation(&conn, operation_id)? else {
        return Ok(false);
    };
    Ok(record.phase == "cancelling" || record.state == "cancelled")
}

/// 启动对账：按 `deleted_at` 的真实取值给中断的恢复操作收束结论。
///
/// 早先这里假定「恢复是一条 `UPDATE`，所以残留项一定还在回收站」，因此把未结项
/// 一律记成 failed。这个前提不成立：清 `deleted_at` 与写收据曾是两个事务，收据
/// 写失败时记录已经恢复，却被报成「未恢复」，用户重试还会撞上「已被其他操作
/// 处理」。现在按证据分派：
///
/// - `deleted_at` 仍是时间戳 → 确定没恢复，记 `failed`（可原样重试）。
/// - `deleted_at` 已为 NULL → 确定已回到库中，记 `succeeded`（不重复计账）。
/// - 记录本身查不到（行的没了，无法读 `deleted_at`）→ `outcome_unknown`。
pub fn reconcile_interrupted_restores(db: &Database) -> AppResult<Vec<String>> {
    let operation_ids = {
        let conn = db.lock()?;
        operations_db::interrupted_operation_ids_for_capability(&conn, "assets.restore.submit")?
    };
    let mut reconciled = Vec::new();
    for operation_id in operation_ids {
        let now = now_unix_seconds()?;
        // 一个短临界区里读完待结算项，锁外做判断，再进第二个短临界区写收据。
        // `complete_operation` 会自开事务，`Database::lock()` 是不可重入的 Mutex，
        // 所以这里绝不能在一个 lock 里调用它。
        let mut pending = Vec::new();
        {
            let conn = db.lock()?;
            for step in operations_db::steps(&conn, &operation_id)? {
                for item in operations_db::items(&conn, &operation_id, &step.step_id)? {
                    if matches!(
                        item.state.as_str(),
                        "succeeded" | "failed" | "cancelled" | "outcome_unknown"
                    ) {
                        continue;
                    }
                    let asset_id = restore_item_asset_id(
                        &ResourceRef::from_trusted(item.item_id.clone()).map_err(AppError::from)?,
                    )?;
                    pending.push((step.step_id.clone(), item.item_id.clone(), asset_id));
                }
            }
        }
        let mut settled_succeeded = 0_i64;
        let mut settled_failed = 0_i64;
        let mut settled_unknown = 0_i64;
        for (step_id, item_id, asset_id) in pending {
            let state = {
                let conn = db.lock()?;
                let still_trashed = assets::trash_states(&conn, &[asset_id])?;
                drop(conn);
                match still_trashed.first() {
                    Some((_, true)) => RestoreReconcile::StillTrashed,
                    Some((_, false)) => RestoreReconcile::Restored,
                    // 行已不在：读不到 `deleted_at`，不能猜。
                    None => RestoreReconcile::Unknown,
                }
            };
            let (item_state, effect_state, code, event, detail) = match state {
                RestoreReconcile::Restored => (
                    "succeeded",
                    "registered",
                    None,
                    "restore_item_completed_after_restart",
                    "重启对账确认记录已回到素材库",
                ),
                RestoreReconcile::StillTrashed => (
                    "failed",
                    "none",
                    Some(CODE_INTERRUPTED),
                    "restore_item_interrupted",
                    "上次恢复被打断，记录仍在回收站",
                ),
                RestoreReconcile::Unknown => (
                    "outcome_unknown",
                    "unknown",
                    Some(CODE_INTERRUPTED),
                    "restore_item_outcome_unknown",
                    "上次恢复被打断，且已找不到该素材记录，无法判定是否恢复",
                ),
            };
            match state {
                RestoreReconcile::Restored => settled_succeeded += 1,
                RestoreReconcile::StillTrashed => settled_failed += 1,
                RestoreReconcile::Unknown => settled_unknown += 1,
            }
            {
                let conn = db.lock()?;
                let transaction = conn.unchecked_transaction()?;
                let updated = operations_db::update_item(
                    &transaction,
                    &operation_id,
                    &step_id,
                    &item_id,
                    item_state,
                    effect_state,
                    code,
                    now,
                )?;
                let receipt_id = format!(
                    "system:item:{}:{}:{}:r{}",
                    operation_id, step_id, item_id, updated.revision
                );
                if !operations_db::receipt_exists(&transaction, &receipt_id)? {
                    operations_db::insert_receipt(
                        &transaction,
                        &operations_db::NewOperationReceipt {
                            receipt_id,
                            operation_id: operation_id.clone(),
                            step_id: step_id.clone(),
                            item_id: Some(item_id.clone()),
                            revision: updated.revision,
                            event: event.to_string(),
                            input_fingerprint: updated.input_fingerprint,
                            target_ref: updated.target_ref,
                            child_task_kind: None,
                            child_task_id: None,
                            db_ref: Some(detail.to_string()),
                            effect_state: effect_state.to_string(),
                            error_code: code.map(str::to_string),
                            recorded_at: now,
                        },
                    )?;
                }
                transaction.commit()?;
            }
        }
        // 结算判据与 `finish_operation` 同形：不可证实的效果优先。
        let final_state = if settled_unknown > 0 {
            "outcome_unknown"
        } else if settled_failed == 0 {
            "succeeded"
        } else if settled_succeeded == 0 {
            "failed"
        } else {
            "partially_succeeded"
        };
        {
            // `complete_operation` 自开事务并需要 `&Connection`，用同一把锁但不嵌套。
            let conn = db.lock()?;
            operations_db::complete_operation(&conn, &operation_id, final_state, now)?;
            operations_db::refresh_recovery_unknown_items(&conn, &operation_id)?;
        }
        reconciled.push(operation_id);
    }
    Ok(reconciled)
}

/// 重启对账时单个恢复项的真实效果判定。
enum RestoreReconcile {
    /// `deleted_at` 已为 NULL：确定已回到库中。
    Restored,
    /// `deleted_at` 仍是时间戳：确定还在回收站。
    StillTrashed,
    /// 读不到该行：无法判定。
    Unknown,
}

/// 重启对账：把上次运行被打断的「永久清空」收束成确定结论。
///
/// 三种日志状态各有唯一正确处理：
///
/// - `intent` + 文件仍在 → 我们没删（或已恢复），可以重试：记 `failed`。
/// - `intent` + 文件已不在 → 分不清是本次删的还是本来就缺的，只能
///   `outcome_unknown`：不猜、不删数据库行。
/// - `file_deleted` + 文件确实不在 → 我们删了文件但没来得及删行，可以安全
///   向前完成：删行 + `committed` + 记 `succeeded`。`file_deleted` 之后文件
///   又出现说明有人放回了同名文件，那就退回 `unknown` 而不是删它。
pub fn reconcile_interrupted_deletions(db: &Database) -> AppResult<Vec<String>> {
    let operation_ids = {
        let conn = db.lock()?;
        operations_db::interrupted_operation_ids_for_capability(&conn, "trash.empty.submit")?
    };
    let mut reconciled = Vec::new();
    for operation_id in operation_ids {
        let now = now_unix_seconds()?;
        // 与恢复对账同样的三段短临界区：`Database::lock()` 是不可重入 Mutex，
        // 而 `assets::delete`/`complete_operation` 都可能自开事务。
        let mut pending: Vec<(String, String, i64)> = Vec::new();
        {
            let conn = db.lock()?;
            for step in operations_db::steps(&conn, &operation_id)? {
                for item in operations_db::items(&conn, &operation_id, &step.step_id)? {
                    if matches!(item.state.as_str(), "succeeded" | "failed" | "cancelled") {
                        continue;
                    }
                    let asset_id = deletion_item_asset_id(
                        &ResourceRef::from_trusted(item.item_id.clone()).map_err(AppError::from)?,
                    )?;
                    pending.push((step.step_id.clone(), item.item_id.clone(), asset_id));
                }
            }
        }
        for (step_id, item_id, asset_id) in pending {
            let (state, code, detail, effect_state, should_delete_row) = {
                let conn = db.lock()?;
                let entry = deletion_journal::entry(&conn, &operation_id, &item_id)?;
                let file_path = entry.as_ref().map(|entry| entry.source_path.clone());
                drop(conn);
                let exists = file_path
                    .as_deref()
                    .map(|path| std::path::Path::new(path).exists())
                    .unwrap_or(false);
                match (entry.map(|entry| entry.state), exists) {
                    (Some(state), false) if state == deletion_journal::STATE_FILE_DELETED => (
                        "succeeded",
                        "",
                        "文件已删除，重启后完成记录清理".to_string(),
                        "removed",
                        true,
                    ),
                    (Some(state), true) if state == deletion_journal::STATE_FILE_DELETED => (
                        "outcome_unknown",
                        CODE_FILE_CHANGED,
                        "文件被删除后又被放回同名文件，未继续删除".to_string(),
                        "unknown",
                        false,
                    ),
                    (Some(state), false) if state == deletion_journal::STATE_INTENT => (
                        "outcome_unknown",
                        CODE_INTERRUPTED,
                        "上次清空被打断，无法判定文件是否由本应用删除".to_string(),
                        "unknown",
                        false,
                    ),
                    _ => (
                        "failed",
                        CODE_INTERRUPTED,
                        "上次清空被打断，文件仍在原位置".to_string(),
                        "none",
                        false,
                    ),
                }
            };

            let conn = db.lock()?;
            if should_delete_row {
                // 只有「有 file_deleted 证据 + 文件确实不在」才向前完成，
                // 这时删行是确定的，不是猜测。
                let transaction = conn.unchecked_transaction()?;
                assets::delete(&transaction, &[asset_id])?;
                if let Some(entry) = deletion_journal::entry(&transaction, &operation_id, &item_id)?
                {
                    if entry.state == deletion_journal::STATE_FILE_DELETED {
                        deletion_journal::record_committed(
                            &transaction,
                            &operation_id,
                            &item_id,
                            now,
                        )?;
                    }
                }
                let updated = operations_db::update_item(
                    &transaction,
                    &operation_id,
                    &step_id,
                    &item_id,
                    state,
                    effect_state,
                    if code.is_empty() { None } else { Some(code) },
                    now,
                )?;
                let receipt_id = format!(
                    "system:item:{}:{}:{}:r{}",
                    operation_id, step_id, item_id, updated.revision
                );
                if !operations_db::receipt_exists(&transaction, &receipt_id)? {
                    operations_db::insert_receipt(
                        &transaction,
                        &operations_db::NewOperationReceipt {
                            receipt_id,
                            operation_id: operation_id.clone(),
                            step_id: step_id.clone(),
                            item_id: Some(item_id.clone()),
                            revision: updated.revision,
                            event: "trash_empty_committed_after_restart".to_string(),
                            input_fingerprint: updated.input_fingerprint,
                            target_ref: updated.target_ref,
                            child_task_kind: None,
                            child_task_id: None,
                            db_ref: Some(detail),
                            effect_state: effect_state.to_string(),
                            error_code: None,
                            recorded_at: now,
                        },
                    )?;
                }
                transaction.commit()?;
            } else {
                let updated = operations_db::update_item(
                    &conn,
                    &operation_id,
                    &step_id,
                    &item_id,
                    state,
                    effect_state,
                    if code.is_empty() { None } else { Some(code) },
                    now,
                )?;
                let receipt_id = format!(
                    "system:item:{}:{}:{}:r{}",
                    operation_id, step_id, item_id, updated.revision
                );
                if !operations_db::receipt_exists(&conn, &receipt_id)? {
                    operations_db::insert_receipt(
                        &conn,
                        &operations_db::NewOperationReceipt {
                            receipt_id,
                            operation_id: operation_id.clone(),
                            step_id: step_id.clone(),
                            item_id: Some(item_id.clone()),
                            revision: updated.revision,
                            event: "trash_empty_interrupted".to_string(),
                            input_fingerprint: updated.input_fingerprint,
                            target_ref: updated.target_ref,
                            child_task_kind: None,
                            child_task_id: None,
                            db_ref: Some(detail),
                            effect_state: effect_state.to_string(),
                            error_code: if code.is_empty() {
                                None
                            } else {
                                Some(code.to_string())
                            },
                            recorded_at: now,
                        },
                    )?;
                }
            }
        }
        {
            let conn = db.lock()?;
            operations_db::complete_operation(&conn, &operation_id, "outcome_unknown", now)?;
            operations_db::refresh_recovery_unknown_items(&conn, &operation_id)?;
        }
        reconciled.push(operation_id);
    }
    Ok(reconciled)
}

// ---------------------------------------------------------------------------
// A4-04：永久删除（独立清空 + 受控到期清理）
//
// 两个入口共用同一个删除核验与同一份持久日志：
//
// - 「永久清空这 N 项」只来自可信界面，走 `trash.empty.prepare/submit`
//   的 *独立* 审批（`ReviewKind::PermanentTrash`），普通计划确认不能替代。
// - 到期清理由宿主维护线程直接调用本服务，不构造 `CallerContext`、不走
//   能力目录，模型没有任何参数可以伪造这个身份或改动到期条件。
//
// 逐项顺序固定为：日志 intent → 复核归属与身份 → 删文件 → file_deleted
// → 同一事务删行 + committed。任何一步证明不了「我删的就是计划冻结的那个
// 文件」，就只报告不扩大范围。
// ---------------------------------------------------------------------------

/// 永久删除项的操作条目 id 前缀。
pub fn deletion_item_id(asset_id: i64) -> String {
    format!("delete_{asset_id:016}")
}

pub fn deletion_item_asset_id(item_ref: &ResourceRef) -> AppResult<i64> {
    item_ref
        .as_str()
        .strip_prefix("delete_")
        .and_then(|rest| rest.parse::<i64>().ok())
        .ok_or_else(|| AppError::internal("清空项引用格式无效"))
}

/// 一次「永久清空」执行的输入。
pub struct EmptyExecution<'a> {
    pub operation_id: &'a str,
    pub operation_ref: &'a ResourceRef,
    pub step_ref: &'a ResourceRef,
    pub asset_ids: &'a [i64],
    pub cancelled: &'a AtomicBool,
}

/// 清空/到期清理的逐项结果计数。
///
/// `removed` 只有「文件确实删除且数据库行确实删除」才计；`missing` 与
/// `unknown` 分开报告，因为前者是文件早已不在，后者是我们无法证明删除的
/// 对象正确，二者的处置不同。
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeletionSummary {
    pub removed: i64,
    pub failed: i64,
    pub missing: i64,
    pub unknown: i64,
    pub skipped: i64,
    pub cancelled: bool,
}

/// 单项删除的判定结果。
enum DeletionOutcome {
    Removed,
    Missing,
    Failed(&'static str, String),
    Unknown(&'static str, String),
}

/// 删除一项前需要的全部纯数据。
struct DeletionTarget {
    asset_id: i64,
    file_path: String,
    file_name: String,
    identity: String,
    deleted_at: i64,
}

fn load_target(db: &Database, asset_id: i64) -> AppResult<Option<DeletionTarget>> {
    let conn = db.lock()?;
    let Some(record) = assets::trash_records(&conn, &[asset_id])?
        .into_iter()
        .next()
    else {
        return Ok(None);
    };
    let location = libraries::verified_location(&conn, asset_id)?;
    Ok(Some(DeletionTarget {
        asset_id,
        file_path: record.file_path,
        file_name: record.file_name,
        // 没有核验证据就证明不了归属，交给调用方按 unknown 报告。
        identity: location
            .map(|value| value.file_identity)
            .unwrap_or_default(),
        deleted_at: record.deleted_at,
    }))
}

/// 删除前的归属复检：当前总仓根 + 父目录链。
///
/// 只核验叶子节点的文件身份不够。父目录被换成 junction/符号链接，或总仓被
/// 离线换根之后，`file_identity` 仍可能与旧证据一致，于是会顺着一层链接把
/// 总仓之外的文件删掉。这里补两道独立证据：
///
/// 1. 文件真实父目录 canonical 之后必须仍落在当前总仓 canonical 根之内；
/// 2. 从父目录向上走到总仓根，中间不允许出现链接或重解析点。这一条不能只看
///    canonical 结果：指向总仓内部的 junction 解析后仍在根内，却仍是用户可见
///    的另一条路径，删除它等于删了计划之外的位置。
///
/// 返回的错误码统一用 `OWNERSHIP_UNVERIFIED`：语义都是「证明不了这条路径仍属于
/// 该素材」，与「文件已变了」（`FILE_CHANGED`）分开，界面文案也不同。
fn verify_ownership(
    library: &LibrarySnapshot,
    target: &DeletionTarget,
) -> Result<(), (&'static str, String)> {
    let file_name = target.file_name.as_str();
    let root = managed_library::verified_root_path(library).map_err(|error| {
        (
            CODE_OWNERSHIP_UNVERIFIED,
            format!("「{file_name}」所在的总仓未能通过身份核验，未删除文件: {error}"),
        )
    })?;
    let path = Path::new(&target.file_path);
    let parent = path.parent().ok_or_else(|| {
        (
            CODE_OWNERSHIP_UNVERIFIED,
            format!("「{file_name}」没有可核验的父目录，未删除文件"),
        )
    })?;
    let canonical_parent = parent.canonicalize().map_err(|error| {
        (
            CODE_OWNERSHIP_UNVERIFIED,
            format!("无法定位「{file_name}」的父目录，未删除文件: {error}"),
        )
    })?;
    if !canonical_parent.starts_with(&root) {
        return Err((
            CODE_OWNERSHIP_UNVERIFIED,
            format!("「{file_name}」的当前位置已不在当前总仓内，未删除文件"),
        ));
    }
    let mut current = parent.to_path_buf();
    loop {
        let metadata = std::fs::symlink_metadata(&current).map_err(|error| {
            (
                CODE_OWNERSHIP_UNVERIFIED,
                format!("无法核验「{file_name}」的父目录，未删除文件: {error}"),
            )
        })?;
        if crate::utils::path::is_link_or_reparse(&metadata) {
            return Err((
                CODE_OWNERSHIP_UNVERIFIED,
                format!("「{file_name}」的父目录含链接或重解析点，未删除文件"),
            ));
        }
        // 走到总仓根就停：根以上的目录不属于本应用的归属证据范围。
        if current
            .canonicalize()
            .map(|value| value == root)
            .unwrap_or(false)
        {
            return Ok(());
        }
        let Some(next) = current.parent() else {
            return Err((
                CODE_OWNERSHIP_UNVERIFIED,
                format!("「{file_name}」的父目录链未能连到当前总仓根，未删除文件"),
            ));
        };
        current = next.to_path_buf();
    }
}

/// 删除一项的正文：文件系统动作 + 持久日志，不写操作收据。
fn delete_target(
    db: &Database,
    library: &LibrarySnapshot,
    scope_id: &str,
    purpose: &str,
    target: &DeletionTarget,
    now: i64,
) -> AppResult<DeletionOutcome> {
    let item_id = deletion_item_id(target.asset_id);
    if target.identity.is_empty() {
        // 无法证明这个路径仍属于该素材：不删，只报告（A4-04 停下条件）。
        return Ok(DeletionOutcome::Unknown(
            CODE_OWNERSHIP_UNVERIFIED,
            format!(
                "「{}」的定位证据已失效，未删除文件，请先在素材库中重新核验",
                target.file_name
            ),
        ));
    }
    {
        let conn = db.lock()?;
        deletion_journal::record_intent(
            &conn,
            &deletion_journal::NewDeletionIntent {
                scope_id,
                item_id: &item_id,
                asset_id: target.asset_id,
                library_id: &library.library_id,
                library_epoch: &library.library_epoch,
                purpose,
                source_path: &target.file_path,
                source_identity: Some(&target.identity),
                updated_at: now,
            },
        )?;
    }

    // 日志先落，再复核归属：中途崩溃时 journal 停在 intent，靠它区分
    // 「真实执行」与「不明缺失」。复检放在 intent 之后、任何文件动作之前。
    if let Err((code, message)) = verify_ownership(library, target) {
        let conn = db.lock()?;
        deletion_journal::record_unknown(&conn, scope_id, &item_id, code, now)?;
        return Ok(DeletionOutcome::Unknown(code, message));
    }

    let path = Path::new(&target.file_path);
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            // 文件早已不在：不是我们删的，只报告；数据库记录保留，
            // 让用户看到「文件缺失」后自己决定移出库。
            let conn = db.lock()?;
            deletion_journal::record_missing(&conn, scope_id, &item_id, now)?;
            return Ok(DeletionOutcome::Missing);
        }
        Err(error) => {
            let conn = db.lock()?;
            deletion_journal::record_failed(
                &conn,
                scope_id,
                &item_id,
                CODE_PERMISSION_DENIED,
                now,
            )?;
            return Ok(DeletionOutcome::Failed(
                CODE_PERMISSION_DENIED,
                format!("无法访问「{}」: {error}", target.file_name),
            ));
        }
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_dir() => {
            let conn = db.lock()?;
            deletion_journal::record_unknown(&conn, scope_id, &item_id, CODE_FILE_CHANGED, now)?;
            return Ok(DeletionOutcome::Unknown(
                CODE_FILE_CHANGED,
                format!("「{}」的原位置已不是普通文件，未删除", target.file_name),
            ));
        }
        Ok(_) => {}
    }

    // 删之前再核一次身份：同名不同文件绝不能删。
    match std::fs::File::open(path) {
        Ok(opened) => match file_identity(&opened) {
            Ok(identity) if identity == target.identity => {}
            Ok(_) => {
                let conn = db.lock()?;
                deletion_journal::record_unknown(
                    &conn,
                    scope_id,
                    &item_id,
                    CODE_FILE_CHANGED,
                    now,
                )?;
                return Ok(DeletionOutcome::Unknown(
                    CODE_FILE_CHANGED,
                    format!("「{}」的原位置已被同名文件替换，未删除", target.file_name),
                ));
            }
            Err(error) => {
                let conn = db.lock()?;
                deletion_journal::record_failed(
                    &conn,
                    scope_id,
                    &item_id,
                    CODE_PERMISSION_DENIED,
                    now,
                )?;
                return Ok(DeletionOutcome::Failed(
                    CODE_PERMISSION_DENIED,
                    format!("无法读取「{}」: {error}", target.file_name),
                ));
            }
        },
        Err(error) => {
            let conn = db.lock()?;
            deletion_journal::record_failed(
                &conn,
                scope_id,
                &item_id,
                CODE_PERMISSION_DENIED,
                now,
            )?;
            return Ok(DeletionOutcome::Failed(
                CODE_PERMISSION_DENIED,
                format!("无法打开「{}」: {error}", target.file_name),
            ));
        }
    }

    if let Err(error) = std::fs::remove_file(path) {
        let code = if error.kind() == std::io::ErrorKind::PermissionDenied {
            CODE_PERMISSION_DENIED
        } else {
            CODE_IO_ERROR
        };
        let conn = db.lock()?;
        deletion_journal::record_failed(&conn, scope_id, &item_id, code, now)?;
        return Ok(DeletionOutcome::Failed(
            code,
            format!("删除文件失败「{}」: {error}", target.file_name),
        ));
    }

    {
        let conn = db.lock()?;
        deletion_journal::record_file_deleted(&conn, scope_id, &item_id, now)?;
    }

    // 数据库行和 committed 证据必须同事务提交：否则崩溃会留下「文件没了
    // 但记录还在」的不明状态。
    let committed = {
        let conn = db.lock()?;
        let transaction = conn.unchecked_transaction()?;
        let result = (|| -> AppResult<()> {
            let deleted = assets::delete(&transaction, &[target.asset_id])?;
            if deleted != 1 {
                return Err(AppError::from(AgentErrorCode::PlanChanged));
            }
            deletion_journal::record_committed(&transaction, scope_id, &item_id, now)?;
            Ok(())
        })();
        match result {
            Ok(()) => {
                transaction.commit()?;
                Ok(())
            }
            Err(error) => Err(error),
        }
    };
    match committed {
        Ok(()) => Ok(DeletionOutcome::Removed),
        Err(error) => Ok(DeletionOutcome::Unknown(
            CODE_INTERRUPTED,
            format!(
                "「{}」的文件已删除，但数据库记录未能同步更新: {error}",
                target.file_name
            ),
        )),
    }
}

/// 可信界面发起的「永久清空这 N 项」：逐项核验、逐项收据。
pub fn empty(
    db: &Database,
    caller: &CallerContext,
    library: &LibrarySnapshot,
    run: EmptyExecution<'_>,
) -> AppResult<DeletionSummary> {
    let mut summary = DeletionSummary::default();
    for &asset_id in run.asset_ids {
        if run.cancelled.load(Ordering::Relaxed) || cancel_requested(db, run.operation_id)? {
            summary.cancelled = true;
            summary.skipped += 1;
            continue;
        }
        let item_ref = ResourceRef::from_trusted(deletion_item_id(asset_id))?;
        let Some(target) = load_target(db, asset_id)? else {
            // 计划冻结之后被其他入口恢复或清理：不能凭旧确认删除任何东西。
            record_deletion_item(
                db,
                caller,
                &run,
                &item_ref,
                "failed",
                CODE_NOT_IN_TRASH,
                "记录已不在回收站，可能已被其他操作恢复或清理",
            )?;
            summary.failed += 1;
            continue;
        };
        let outcome = delete_target(
            db,
            library,
            run.operation_id,
            deletion_journal::PURPOSE_EMPTY,
            &target,
            now_unix_seconds()?,
        )?;
        let receipt = classify_outcome(&target, outcome);
        tally(&mut summary, &receipt);
        record_deletion_item(
            db,
            caller,
            &run,
            &item_ref,
            receipt.state,
            receipt.error_code,
            &receipt.detail,
        )?;
    }
    Ok(summary)
}

/// 到期清理的清单：只返回归属明确、确实超期的回收站项。
///
/// 单独暴露成函数是为了让界面与测试能看到「这次到底会清哪些」，而不是只能
/// 从清理结果反推。`cutoff` 由调用方按当前保留天数算出。
pub fn expired_candidates(db: &Database, cutoff_ms: i64) -> AppResult<Vec<(i64, String)>> {
    let conn = db.lock()?;
    assets::list_expired_trash(&conn, cutoff_ms)
}

/// 到期清理：宿主维护线程调用，没有 `CallerContext`，也没有操作收据。
///
/// 与 `empty` 共用 `delete_target`，所以核验与日志完全一致；差别只有
/// 「谁来调用」和「要不要写 operations 收据」。`on_removed` 让调用方在
/// 每一项真正落盘删除后立刻清缩略图，不需要把 id 列表攒在内存里再回头查。
pub fn purge_expired(
    db: &Database,
    library: &LibrarySnapshot,
    retention_days: i64,
    on_removed: &mut dyn FnMut(i64),
) -> AppResult<DeletionSummary> {
    let cutoff = chrono::Utc::now().timestamp_millis() - retention_days * 86_400_000;
    let scope_id = format!("expiry_{}", now_unix_seconds()?);
    let expired = expired_candidates(db, cutoff)?;
    let mut summary = DeletionSummary::default();
    for (asset_id, _) in expired {
        let Some(target) = load_target(db, asset_id)? else {
            summary.skipped += 1;
            continue;
        };
        // 逐项再算一次到期：本次运行期间新进入回收站的项不能被旧 cutoff 带走。
        if target.deleted_at >= cutoff {
            summary.skipped += 1;
            continue;
        }
        let outcome = delete_target(
            db,
            library,
            &scope_id,
            deletion_journal::PURPOSE_EXPIRY,
            &target,
            now_unix_seconds()?,
        )?;
        if matches!(outcome, DeletionOutcome::Removed) {
            on_removed(asset_id);
        }
        let receipt = classify_outcome(&target, outcome);
        tally(&mut summary, &receipt);
    }
    Ok(summary)
}

/// 单项结果的收据内容与计数归属。
struct OutcomeReceipt<'a> {
    state: &'a str,
    error_code: &'a str,
    detail: String,
}

fn classify_outcome(target: &DeletionTarget, outcome: DeletionOutcome) -> OutcomeReceipt<'static> {
    match outcome {
        DeletionOutcome::Removed => OutcomeReceipt {
            state: "succeeded",
            error_code: "",
            detail: format!("已永久删除: {}", target.file_name),
        },
        DeletionOutcome::Missing => OutcomeReceipt {
            state: "failed",
            error_code: CODE_FILE_MISSING,
            detail: format!(
                "「{}」的文件已不在原位置，记录保留在回收站",
                target.file_name
            ),
        },
        DeletionOutcome::Unknown(code, message) => OutcomeReceipt {
            state: "outcome_unknown",
            error_code: code,
            detail: message,
        },
        DeletionOutcome::Failed(code, message) => OutcomeReceipt {
            state: "failed",
            error_code: code,
            detail: message,
        },
    }
}

fn tally(summary: &mut DeletionSummary, receipt: &OutcomeReceipt<'_>) {
    match receipt.state {
        "succeeded" => summary.removed += 1,
        "outcome_unknown" => summary.unknown += 1,
        _ if receipt.error_code == CODE_FILE_MISSING => summary.missing += 1,
        _ => summary.failed += 1,
    }
}

fn record_deletion_item(
    db: &Database,
    caller: &CallerContext,
    run: &EmptyExecution<'_>,
    item_ref: &ResourceRef,
    state: &str,
    error_code: &str,
    detail: &str,
) -> AppResult<()> {
    let (effect_state, event) = match state {
        "succeeded" => ("removed", "trash_empty_removed"),
        "outcome_unknown" => ("unknown", "trash_empty_outcome_unknown"),
        _ => ("none", "trash_empty_failed"),
    };
    let code = if error_code.is_empty() {
        None
    } else {
        Some(error_code)
    };
    operations::record_item_state(
        db,
        caller,
        run.operation_ref,
        run.step_ref,
        item_ref,
        state,
        effect_state,
        code,
        &ResourceRef::generated("receipt"),
        event,
        if detail.is_empty() {
            None
        } else {
            Some(detail)
        },
    )
}

/// 回收站清单：给界面显示「已移入时间 / 预计自动清理时间」。
pub fn trash_page(db: &Database, limit: usize) -> AppResult<Vec<TrashRecord>> {
    let conn = db.lock()?;
    assets::list_trashed(&conn, limit.clamp(1, 500))
}

fn now_unix_seconds() -> AppResult<i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::internal("系统时间早于 Unix 纪元"))?;
    i64::try_from(now.as_secs()).map_err(|_| AppError::internal("系统时间超出范围"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_uses_the_same_formula_as_the_purge_job() {
        // 2024-01-01T00:00:00Z，保留 30 天
        let deleted_at = 1_704_067_200_000;
        assert_eq!(
            expires_at_ms(deleted_at, 30),
            deleted_at + 30 * 86_400_000,
            "界面显示的时间必须等于后台真正清理的时间"
        );
        // 极值不 panic（saturating）
        assert_eq!(expires_at_ms(i64::MAX, 30), i64::MAX);
        assert!(expires_at_ms(0, 0) == 0);
    }

    #[test]
    fn format_day_is_stable_and_never_panics_on_out_of_range() {
        // 折成本地日期后只校验形状：不同时区可能落在不同日，但格式固定
        let text = format_day(1_704_067_200_000);
        assert_eq!(text.len(), 10, "{text}");
        assert_eq!(text.chars().filter(|c| *c == '-').count(), 2, "{text}");
        assert_eq!(format_day(i64::MAX), "未知日期");
    }

    #[test]
    fn item_references_round_trip_through_the_asset_id() {
        let item_ref = ResourceRef::from_trusted(restore_item_id(42)).unwrap();
        assert_eq!(restore_item_asset_id(&item_ref).unwrap(), 42);
        let foreign = ResourceRef::from_trusted("move_0000000000000042").unwrap();
        assert!(restore_item_asset_id(&foreign).is_err(), "前缀不同必须拒绝");
        let broken = ResourceRef::from_trusted("restore_notanumber").unwrap();
        assert!(
            restore_item_asset_id(&broken).is_err(),
            "非数字 id 必须拒绝"
        );
    }
}
