//! Operation queue behavior above the persistence layer.
//!
//! Domain workers own their actual file/network/database effects. This module
//! owns fixed operation/step identity, ownership checks, cancel requests,
//! recovery summaries, child-task binding, and snapshot projection.

use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use crate::contracts::agent::{
    AgentErrorCode, CapabilityId, OperationSnapshot, OperationState, ResourceRef,
};
use crate::contracts::caller::{check_capability_entry, CallerContext};
use crate::db::authorization;
use crate::db::operations::{self, NewOperationItem, NewOperationReceipt};
use crate::db::{import_plans, libraries, plans, tagging};
use crate::error::{AppError, AppResult};
use crate::services::plans as plan_service;
use crate::state::Database;

#[derive(Debug, Default)]
pub struct OperationQueue {
    active: Mutex<Option<String>>,
}

impl OperationQueue {
    pub fn claim_next(&self, db: &Database) -> AppResult<Option<String>> {
        self.claim_next_for_capability(db, None)
    }

    pub fn claim_next_for_capability(
        &self,
        db: &Database,
        capability: Option<&str>,
    ) -> AppResult<Option<String>> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| AppError::msg("操作队列锁中毒"))?;
        if active.is_some() {
            return Ok(None);
        }
        let operation_id = {
            let conn = db.lock()?;
            operations::claim_next_for_capability(&conn, now_unix_seconds()?, capability)?
        };
        if let Some(operation_id) = &operation_id {
            *active = Some(operation_id.clone());
        }
        Ok(operation_id)
    }

    pub fn release(&self, operation_id: &str) -> AppResult<()> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| AppError::msg("操作队列锁中毒"))?;
        if active.as_deref() == Some(operation_id) {
            *active = None;
        }
        Ok(())
    }

    pub fn try_reserve_operation(
        self: &Arc<Self>,
        db: &Database,
        operation_id: &str,
        capability: &str,
    ) -> AppResult<Option<OperationQueueLease>> {
        let mut active = self
            .active
            .lock()
            .map_err(|_| AppError::msg("操作队列锁中毒"))?;
        if active.is_some() {
            return Ok(None);
        }
        let eligible = {
            let conn = db.lock()?;
            operations::is_queued_for_dispatch(&conn, operation_id, capability)?
        };
        if !eligible {
            return Ok(None);
        }
        *active = Some(operation_id.to_string());
        Ok(Some(OperationQueueLease {
            queue: Arc::clone(self),
            operation_id: operation_id.to_string(),
        }))
    }
}

pub struct OperationQueueLease {
    queue: Arc<OperationQueue>,
    operation_id: String,
}

impl Drop for OperationQueueLease {
    fn drop(&mut self) {
        if let Err(error) = self.queue.release(&self.operation_id) {
            tracing::error!(operation_id=%self.operation_id, error_code=%error.code(), "操作队列槽位释放失败");
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecoverySummaryView {
    pub operation_ref: ResourceRef,
    pub pending_steps: u32,
    pub running_steps: u32,
    pub unknown_items: u32,
    pub requires_resume: bool,
}

pub fn reconcile_startup(db: &Database) -> AppResult<Vec<RecoverySummaryView>> {
    let newly_interrupted = {
        let conn = db.lock()?;
        let now = now_unix_seconds()?;
        let summaries = operations::reconcile_startup(&conn, now)?;
        tagging::recover_interrupted(&conn, now)?;
        for recovery in operations::recovery_summaries(&conn)? {
            operations::refresh_recovery_unknown_items(&conn, &recovery.operation_id)?;
        }
        summaries
    };

    // Domain reconciliation runs after the generic state transition. Import
    // artifacts are inspected off-lock, then their durable result is folded
    // back into each operation's receipts and recovery summary.
    crate::services::import_execute::reconcile_interrupted_imports(db)?;
    // A move leaves per-file intents in `move_journal`. A rename that already
    // happened cannot be rolled back by a database transaction, so the journal is
    // what decides whether each item still needs its path evidence written.
    crate::services::library_move::reconcile_interrupted_moves(db)?;
    crate::services::trash::reconcile_interrupted_restores(db)?;
    // A permanent deletion may have removed bytes without committing its row.
    // `deletion_journal` is the only evidence of which half completed, so the
    // sweep runs here before the expiry job is allowed to touch the trash.
    crate::services::trash::reconcile_interrupted_deletions(db)?;
    // A library migration that reached `switched` has already moved the locator
    // to the new root; its evidence rows are the only way back to a consistent
    // state. This runs before the expiry job for the same reason as above: the
    // trash sweep must never be the first thing to touch a half-migrated library.
    crate::services::library_migration::reconcile_interrupted_migration(db)?;

    let current_recovery = {
        let conn = db.lock()?;
        operations::recovery_summaries(&conn)?
            .into_iter()
            .map(|summary| (summary.operation_id.clone(), summary))
            .collect::<std::collections::HashMap<_, _>>()
    };
    newly_interrupted
        .into_iter()
        .filter_map(|summary| current_recovery.get(&summary.operation_id))
        .map(|latest| {
            Ok(RecoverySummaryView {
                operation_ref: ResourceRef::from_trusted(latest.operation_id.clone())?,
                pending_steps: u32_of(latest.pending_steps)?,
                running_steps: u32_of(latest.running_steps)?,
                unknown_items: u32_of(latest.unknown_items)?,
                requires_resume: latest.requires_resume,
            })
        })
        .collect()
}

pub fn recovery_summaries(db: &Database) -> AppResult<Vec<RecoverySummaryView>> {
    let conn = db.lock()?;
    operations::recovery_summaries(&conn)?
        .into_iter()
        .map(|summary| {
            Ok(RecoverySummaryView {
                operation_ref: ResourceRef::from_trusted(summary.operation_id)?,
                pending_steps: u32_of(summary.pending_steps)?,
                running_steps: u32_of(summary.running_steps)?,
                unknown_items: u32_of(summary.unknown_items)?,
                requires_resume: summary.requires_resume,
            })
        })
        .collect()
}

pub fn cancel_operation(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
) -> AppResult<OperationSnapshot> {
    check_capability_entry(CapabilityId::OperationsCancel, caller).map_err(AppError::from)?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    require_owner(&conn, caller, operation_ref.as_str())?;
    operations::request_cancel(&conn, operation_ref.as_str(), now_unix_seconds()?)?;
    let record = plans::operation(&conn, operation_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if record.state == "queued" {
        operations::finish_cancel(&conn, operation_ref.as_str(), now_unix_seconds()?)?;
        let cancelled = plans::operation(&conn, operation_ref.as_str())?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        return plan_service::operation_snapshot(&conn, caller, &cancelled);
    }
    plan_service::operation_snapshot(&conn, caller, &record)
}

/// Cancel a tagging operation selected by its legacy AI batch key. The AI
/// page still addresses the batch, but once it is attached to an approved
/// operation the durable operation state must be cancelled before the
/// in-memory worker flag is signalled by the command layer.
pub fn cancel_tagging_batch(
    db: &Database,
    caller: &CallerContext,
    batch_id: i64,
) -> AppResult<Option<OperationSnapshot>> {
    check_capability_entry(CapabilityId::OperationsCancel, caller).map_err(AppError::from)?;
    if caller.kind != crate::contracts::agent::CallerKind::Gui {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let operation_id = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        let Some(link) = tagging::by_batch(&conn, batch_id)? else {
            return Ok(None);
        };
        let owner = operations::operation_owner(&conn, &link.operation_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        if owner.principal_id != caller.principal_id.as_str()
            || owner.library_id != caller.library.library_id.as_str()
            || owner.library_epoch != caller.library.library_epoch.as_str()
        {
            return Err(AppError::from(AgentErrorCode::Unauthorized));
        }
        link.operation_id
    };
    let operation_ref = ResourceRef::from_trusted(operation_id)?;
    cancel_operation(db, caller, &operation_ref).map(Some)
}

pub fn resume_operation(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
) -> AppResult<OperationSnapshot> {
    check_capability_entry(CapabilityId::OperationsResume, caller).map_err(AppError::from)?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let owner = require_library_owner(&conn, caller, operation_ref.as_str())?;
    if owner.library_id != caller.library.library_id.as_str()
        || owner.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    if owner.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    operations::mark_resumed(
        &conn,
        operation_ref.as_str(),
        &operations::ResumeOwner {
            principal_id: caller.principal_id.as_str(),
            session_id: caller.session_id.as_ref().map(ResourceRef::as_str),
            instance_id: caller.instance_id.as_str(),
            grant_epoch: caller.grant_epoch.as_str(),
        },
        now_unix_seconds()?,
    )?;
    let record = plans::operation(&conn, operation_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    plan_service::operation_snapshot(&conn, caller, &record)
}

/// Classify an owned operation for GUI routing without exposing operation SQL
/// to the command layer.
pub fn is_import_operation(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
) -> AppResult<bool> {
    check_capability_entry(CapabilityId::OperationsResume, caller).map_err(AppError::from)?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let owner = require_library_owner(&conn, caller, operation_ref.as_str())?;
    if owner.library_id != caller.library.library_id.as_str()
        || owner.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    if owner.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let operation = plans::operation(&conn, operation_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    Ok(operation.capability == "imports.submit")
}

/// Whether resuming this import requires a fresh directory selection. File
/// handles are session-bound, so unfinished directory-backed items that still
/// need source bytes cannot resume in a new session until their frozen source
/// identity is reverified. A durable published target can be reconciled alone.
pub fn import_resume_requires_source(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
) -> AppResult<bool> {
    check_capability_entry(CapabilityId::OperationsResume, caller).map_err(AppError::from)?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let owner = require_library_owner(&conn, caller, operation_ref.as_str())?;
    if owner.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let operation = plans::operation(&conn, operation_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if operation.capability != "imports.submit" {
        return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
    }
    if operation.state != "interrupted" || operation.phase != "reconciling" {
        return Err(AppError::from(AgentErrorCode::ResourceConflict));
    }
    let plan = plans::plan(&conn, &operation.plan_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let import_plan = import_plans::plan(&conn, &plan.scope_ref)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if import_plan.source_ref.is_none() {
        return Ok(false);
    }
    let items = import_plans::items(&conn, &plan.scope_ref)?;
    let Some(step) = operations::steps(&conn, operation_ref.as_str())?
        .into_iter()
        .find(|step| step.capability == "imports.submit")
    else {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    };
    let completed: std::collections::HashSet<_> =
        operations::items(&conn, operation_ref.as_str(), &step.step_id)?
            .into_iter()
            .filter(|item| item.state == "succeeded")
            .map(|item| item.item_id)
            .collect();
    let source_items: Vec<_> = items
        .into_iter()
        .filter(|item| !item.excluded && !completed.contains(&item.item_id))
        .collect();
    drop(conn);
    for item in &source_items {
        if crate::services::import_execute::import_item_requires_source_rebinding(
            db,
            operation_ref,
            item,
        )? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Reauthorize only unfinished directory-backed items that still need source
/// bytes, verify them against the frozen plan, then rebind handles and queue
/// the operation in a single database transaction. Published items recover
/// from their durable target intent without reopening their source.
pub fn resume_import_operation(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    new_directory_ref: &ResourceRef,
) -> AppResult<OperationSnapshot> {
    check_capability_entry(CapabilityId::OperationsResume, caller).map_err(AppError::from)?;
    let (scope_ref, pending_items, owner) = {
        let conn = db.lock()?;
        validate_caller(&conn, caller)?;
        let owner = require_library_owner(&conn, caller, operation_ref.as_str())?;
        if owner.principal_id != caller.principal_id.as_str() {
            return Err(AppError::from(AgentErrorCode::Unauthorized));
        }
        let operation = plans::operation(&conn, operation_ref.as_str())?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        if operation.capability != "imports.submit" {
            return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
        }
        if operation.state != "interrupted" || operation.phase != "reconciling" {
            return Err(AppError::from(AgentErrorCode::ResourceConflict));
        }
        let plan = plans::plan(&conn, &operation.plan_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        let import_plan = import_plans::plan(&conn, &plan.scope_ref)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        if import_plan.source_ref.is_none() {
            return Err(AppError::from(AgentErrorCode::ResourceConflict));
        }
        let items = import_plans::items(&conn, &plan.scope_ref)?;
        let step = operations::steps(&conn, operation_ref.as_str())?
            .into_iter()
            .find(|step| step.capability == "imports.submit")
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        let completed: std::collections::HashSet<_> =
            operations::items(&conn, operation_ref.as_str(), &step.step_id)?
                .into_iter()
                .filter(|item| item.state == "succeeded")
                .map(|item| item.item_id)
                .collect();
        let pending_items: Vec<_> = items
            .into_iter()
            .filter(|item| !item.excluded && !completed.contains(&item.item_id))
            .collect();
        if pending_items.is_empty() {
            return Err(AppError::from(AgentErrorCode::ResourceConflict));
        }
        (plan.scope_ref, pending_items, owner)
    };

    let mut source_items = Vec::with_capacity(pending_items.len());
    for item in pending_items {
        if crate::services::import_execute::import_item_requires_source_rebinding(
            db,
            operation_ref,
            &item,
        )? {
            source_items.push(item);
        }
    }
    if source_items.is_empty() {
        return Err(AppError::from(AgentErrorCode::ResourceConflict));
    }
    let rebound = crate::services::authorization::prepare_import_file_rebinding(
        db,
        caller,
        new_directory_ref,
        &source_items,
    )?;
    let file_refs: Vec<_> = rebound
        .iter()
        .map(|(item_id, handle)| (item_id.clone(), handle.handle_id.clone()))
        .collect();
    let now = now_unix_seconds()?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let current_owner = require_library_owner(&conn, caller, operation_ref.as_str())?;
    if current_owner.principal_id != owner.principal_id
        || current_owner.library_id != owner.library_id
        || current_owner.library_epoch != owner.library_epoch
    {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    let transaction = conn.unchecked_transaction()?;
    for (_, handle) in &rebound {
        authorization::insert_handle(&transaction, handle)?;
    }
    import_plans::rebind_file_handles(&transaction, &scope_ref, &file_refs)?;
    operations::mark_resumed_in_transaction(
        &transaction,
        operation_ref.as_str(),
        &operations::ResumeOwner {
            principal_id: caller.principal_id.as_str(),
            session_id: caller.session_id.as_ref().map(ResourceRef::as_str),
            instance_id: caller.instance_id.as_str(),
            grant_epoch: caller.grant_epoch.as_str(),
        },
        now,
    )?;
    transaction.commit()?;
    let record = plans::operation(&conn, operation_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    plan_service::operation_snapshot(&conn, caller, &record)
}

#[allow(clippy::too_many_arguments)]
pub fn register_item(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    step_ref: &ResourceRef,
    item_id: &str,
    ordinal: u32,
    input_fingerprint: Option<&str>,
    target_ref: Option<&str>,
) -> AppResult<ResourceRef> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    require_owner(&conn, caller, operation_ref.as_str())?;
    operations::insert_item(
        &conn,
        &NewOperationItem {
            operation_id: operation_ref.as_str().to_string(),
            step_id: step_ref.as_str().to_string(),
            item_id: item_id.to_string(),
            ordinal: ordinal as i64,
            input_fingerprint: input_fingerprint.map(str::to_string),
            target_ref: target_ref.map(str::to_string),
            created_at: now_unix_seconds()?,
        },
    )?;
    ResourceRef::from_trusted(item_id.to_string()).map_err(AppError::from)
}

#[allow(clippy::too_many_arguments)]
pub fn record_item_state(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    step_ref: &ResourceRef,
    item_ref: &ResourceRef,
    state: &str,
    effect_state: &str,
    error_code: Option<&str>,
    receipt_id: &ResourceRef,
    event: &str,
    db_ref: Option<&str>,
) -> AppResult<()> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    require_owner(&conn, caller, operation_ref.as_str())?;
    let transaction = conn.unchecked_transaction()?;
    record_item_state_in(
        &transaction,
        operation_ref,
        step_ref,
        item_ref,
        state,
        effect_state,
        error_code,
        receipt_id,
        event,
        db_ref,
    )?;
    transaction.commit()?;
    Ok(())
}

/// 在调用方已开启的事务里落「操作项终态 + 收据 + 进度」。
///
/// 领域服务需要把「业务效果」和「收据」写成同一个原子提交时用这个变体。典型
/// 例子是回收站恢复：清 `deleted_at` 与写成功收据必须同生共死，否则中间失败
/// 会让对账把「已恢复」误报成失败。授权与归属校验由调用方在更外层完成
/// （`execute` 的 caller 来自已校验的 plan/operation 上下文），所以这里不
/// 重复校验，只保证三张表的写入落在同一个事务里。
#[allow(clippy::too_many_arguments)]
pub fn record_item_state_in(
    conn: &rusqlite::Connection,
    operation_ref: &ResourceRef,
    step_ref: &ResourceRef,
    item_ref: &ResourceRef,
    state: &str,
    effect_state: &str,
    error_code: Option<&str>,
    receipt_id: &ResourceRef,
    event: &str,
    db_ref: Option<&str>,
) -> AppResult<()> {
    let now = now_unix_seconds()?;
    let updated = operations::update_item(
        conn,
        operation_ref.as_str(),
        step_ref.as_str(),
        item_ref.as_str(),
        state,
        effect_state,
        error_code,
        now,
    )?;
    operations::insert_receipt(
        conn,
        &NewOperationReceipt {
            receipt_id: receipt_id.as_str().to_string(),
            operation_id: operation_ref.as_str().to_string(),
            step_id: step_ref.as_str().to_string(),
            item_id: Some(item_ref.as_str().to_string()),
            revision: updated.revision,
            event: event.to_string(),
            input_fingerprint: updated.input_fingerprint,
            target_ref: updated.target_ref,
            child_task_kind: updated.child_task_kind,
            child_task_id: updated.child_task_id,
            db_ref: db_ref.map(str::to_string),
            effect_state: effect_state.to_string(),
            error_code: error_code.map(str::to_string),
            recorded_at: now,
        },
    )?;
    operations::refresh_progress(conn, operation_ref.as_str(), now)?;
    Ok(())
}

pub fn bind_child_task(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    step_ref: &ResourceRef,
    task_kind: &str,
    task_ref: &ResourceRef,
) -> AppResult<ResourceRef> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    require_owner(&conn, caller, operation_ref.as_str())?;
    let binding = operations::bind_child_task(
        &conn,
        operation_ref.as_str(),
        step_ref.as_str(),
        task_kind,
        task_ref.as_str(),
        now_unix_seconds()?,
    )?;
    ResourceRef::from_trusted(binding.task_id).map_err(AppError::from)
}

pub fn finish_operation(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    state: OperationState,
) -> AppResult<OperationSnapshot> {
    if !matches!(
        state,
        OperationState::Succeeded
            | OperationState::PartiallySucceeded
            | OperationState::Failed
            | OperationState::OutcomeUnknown
    ) {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    require_owner(&conn, caller, operation_ref.as_str())?;
    let state = operation_state_to_string(state);
    let now = now_unix_seconds()?;
    operations::refresh_progress(&conn, operation_ref.as_str(), now)?;
    operations::complete_operation(&conn, operation_ref.as_str(), state, now)?;
    let record = plans::operation(&conn, operation_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    plan_service::operation_snapshot(&conn, caller, &record)
}

pub fn finish_cancelled(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
) -> AppResult<OperationSnapshot> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    require_owner(&conn, caller, operation_ref.as_str())?;
    let now = now_unix_seconds()?;
    operations::finish_cancel(&conn, operation_ref.as_str(), now)?;
    operations::refresh_progress(&conn, operation_ref.as_str(), now)?;
    let record = plans::operation(&conn, operation_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    plan_service::operation_snapshot(&conn, caller, &record)
}

pub fn child_task_binding(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    step_ref: &ResourceRef,
) -> AppResult<Option<ResourceRef>> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    require_owner(&conn, caller, operation_ref.as_str())?;
    operations::child_task(&conn, operation_ref.as_str(), step_ref.as_str())?
        .map(|binding| ResourceRef::from_trusted(binding.task_id).map_err(AppError::from))
        .transpose()
}

pub fn receipts(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
    step_ref: &ResourceRef,
) -> AppResult<usize> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    require_owner(&conn, caller, operation_ref.as_str())?;
    Ok(operations::receipts(&conn, operation_ref.as_str(), step_ref.as_str())?.len())
}

fn require_owner(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
    operation_id: &str,
) -> AppResult<operations::OperationOwner> {
    let owner = require_library_owner(conn, caller, operation_id)?;
    if owner.principal_id != caller.principal_id.as_str()
        || owner.session_id.as_deref() != caller.session_id.as_ref().map(ResourceRef::as_str)
        || owner.instance_id != caller.instance_id.as_str()
    {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    Ok(owner)
}

fn require_library_owner(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
    operation_id: &str,
) -> AppResult<operations::OperationOwner> {
    let owner = operations::operation_owner(conn, operation_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if owner.library_id != caller.library.library_id.as_str()
        || owner.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(owner)
}

fn validate_caller(conn: &rusqlite::Connection, caller: &CallerContext) -> AppResult<()> {
    let principal = authorization::principal(conn, caller.principal_id.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::GrantRevoked))?;
    if principal.grant_epoch != caller.grant_epoch.as_str() {
        return Err(AppError::from(AgentErrorCode::GrantRevoked));
    }
    let library =
        libraries::current(conn)?.ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
    if library.health != libraries::LibraryHealth::Ready {
        return Err(AppError::from(AgentErrorCode::LibraryOffline));
    }
    if library.library_id != caller.library.library_id.as_str()
        || library.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(())
}

fn u32_of(value: i64) -> AppResult<u32> {
    u32::try_from(value).map_err(|_| AppError::internal("操作计数值越界"))
}

fn operation_state_to_string(state: OperationState) -> &'static str {
    match state {
        OperationState::Queued => "queued",
        OperationState::Running => "running",
        OperationState::Succeeded => "succeeded",
        OperationState::PartiallySucceeded => "partially_succeeded",
        OperationState::Failed => "failed",
        OperationState::Cancelled => "cancelled",
        OperationState::Interrupted => "interrupted",
        OperationState::OutcomeUnknown => "outcome_unknown",
    }
}

fn now_unix_seconds() -> AppResult<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(|_| AppError::internal("系统时间早于 Unix 纪元"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::agent::{BudgetSummary, CapabilityId, PlanScopeSummary};
    use crate::db::{init_memory, operations as db_operations};
    use crate::services::authorization::{self, TrustedAdapterIdentity};
    use crate::services::managed_library;
    use crate::services::plans::{self as plans_service, PlanDraft};

    fn ready_db(root: &std::path::Path) -> Database {
        let conn = init_memory().unwrap();
        managed_library::adopt_initial_root(&conn, root).unwrap();
        Database::new(conn)
    }

    fn caller(db: &Database, session: &str, instance: &str) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Gui {
                session_id: session.to_string(),
                instance_id: instance.to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn draft() -> PlanDraft {
        PlanDraft {
            capability: CapabilityId::ImportsSubmit,
            scope: PlanScopeSummary {
                scope_ref: ResourceRef::generated("scope"),
                source_ref: Some(ResourceRef::generated("directory")),
                selection_ref: None,
                target_ref: None,
                service_ref: None,
                service_revision: None,
                model: None,
            },
            item_count: 2,
            summary: serde_json::from_value(serde_json::json!("恢复操作测试")).unwrap(),
            details_json: None,
            budget: BudgetSummary {
                max_requests: 0,
                max_tokens: 0,
                max_seconds: 300,
                max_items: 2,
                monetary_limit: None,
            },
            ttl_seconds: 0,
        }
    }

    fn create_operation(db: &Database, caller: &CallerContext) -> (ResourceRef, ResourceRef) {
        let current = draft();
        let preview = plans_service::prepare_plan(db, caller, current.clone()).unwrap();
        plans_service::approve_plan(db, caller, &preview.plan_id, &preview.review_ref).unwrap();
        let submitted = plans_service::submit_plan(
            db,
            caller,
            &preview.plan_id,
            &ResourceRef::generated("idempotency"),
            &current,
        )
        .unwrap();
        let step = {
            let conn = db.lock().unwrap();
            db_operations::steps(&conn, submitted.operation.operation_id.as_str())
                .unwrap()
                .into_iter()
                .next()
                .unwrap()
        };
        (
            submitted.operation.operation_id,
            ResourceRef::from_trusted(step.step_id).unwrap(),
        )
    }

    fn error_code(error: AppError) -> String {
        error.code().to_string()
    }

    #[test]
    fn claim_cancel_and_receipts_preserve_successful_items() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = caller(&db, "session_ops_alpha", "instance_ops_alpha");
        let (operation_ref, step_ref) = create_operation(&db, &caller);
        let queue = OperationQueue::default();

        assert_eq!(
            queue.claim_next(&db).unwrap().as_deref(),
            Some(operation_ref.as_str())
        );
        assert!(queue.claim_next(&db).unwrap().is_none());
        let first = register_item(
            &db,
            &caller,
            &operation_ref,
            &step_ref,
            "operation_item_first",
            0,
            Some("fingerprint-a"),
            Some("target-a"),
        )
        .unwrap();
        let second = register_item(
            &db,
            &caller,
            &operation_ref,
            &step_ref,
            "operation_item_second",
            1,
            Some("fingerprint-b"),
            Some("target-b"),
        )
        .unwrap();
        record_item_state(
            &db,
            &caller,
            &operation_ref,
            &step_ref,
            &first,
            "succeeded",
            "registered",
            None,
            &ResourceRef::generated("receipt"),
            "item_registered",
            Some("asset:41"),
        )
        .unwrap();

        let cancelling = cancel_operation(&db, &caller, &operation_ref).unwrap();
        assert_eq!(
            cancelling.phase,
            crate::contracts::agent::OperationPhase::Cancelling
        );
        assert_eq!(cancelling.completed, 1);
        queue.release(operation_ref.as_str()).unwrap();
        assert!(queue.claim_next(&db).unwrap().is_none());

        let cancelled = finish_cancelled(&db, &caller, &operation_ref).unwrap();
        assert_eq!(cancelled.state, OperationState::Cancelled);
        assert_eq!(cancelled.completed, 1);
        assert_eq!(cancelled.total, Some(2));
        assert_eq!(
            receipts(&db, &caller, &operation_ref, &step_ref).unwrap(),
            2,
            "cancellation must add a receipt for every item revision"
        );
        let conn = db.lock().unwrap();
        let second_state: String = conn
            .query_row(
                "SELECT state FROM operation_items
                  WHERE operation_id=?1 AND step_id=?2 AND item_id=?3",
                rusqlite::params![operation_ref.as_str(), step_ref.as_str(), second.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(second_state, "cancelled");
        let receipts =
            db_operations::receipts(&conn, operation_ref.as_str(), step_ref.as_str()).unwrap();
        assert_eq!(receipts.last().unwrap().event, "item_cancelled");
        assert_eq!(
            receipts.last().unwrap().item_id.as_deref(),
            Some(second.as_str())
        );
    }

    #[test]
    fn explicit_worker_reservation_shares_the_import_single_slot() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = caller(&db, "session_queue_slot", "instance_queue_slot");
        let (operation_ref, _) = create_operation(&db, &caller);
        assert!(is_import_operation(&db, &caller, &operation_ref).unwrap());
        let queue = Arc::new(OperationQueue::default());

        let lease = queue
            .try_reserve_operation(&db, operation_ref.as_str(), "imports.submit")
            .unwrap()
            .expect("the queued operation should reserve the idle shared slot");
        assert!(queue.claim_next(&db).unwrap().is_none());
        drop(lease);
        assert_eq!(
            queue.claim_next(&db).unwrap().as_deref(),
            Some(operation_ref.as_str()),
            "releasing the explicit worker lease must make the queued work dispatchable"
        );
    }

    #[test]
    fn explicit_worker_reservation_cannot_skip_an_older_queued_operation() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = caller(&db, "session_queue_order", "instance_queue_order");
        let (first, _) = create_operation(&db, &caller);
        let (second, _) = create_operation(&db, &caller);
        let queue = Arc::new(OperationQueue::default());
        let oldest = {
            let conn = db.lock().unwrap();
            conn.query_row(
                "SELECT operation_id FROM operations
                  WHERE state='queued' AND phase='ready'
                  ORDER BY created_at,operation_id LIMIT 1",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap()
        };
        let later = if oldest == first.as_str() {
            second.as_str()
        } else {
            first.as_str()
        };

        assert!(queue
            .try_reserve_operation(&db, later, "imports.submit")
            .unwrap()
            .is_none());
        let lease = queue
            .try_reserve_operation(&db, &oldest, "imports.submit")
            .unwrap()
            .expect("the oldest queued operation may reserve the shared slot");
        drop(lease);
    }

    #[test]
    fn awaiting_explicit_tagging_start_does_not_block_import_dispatch() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = caller(&db, "session_queue_waiting", "instance_queue_waiting");
        let (waiting_tagging, _) = create_operation(&db, &caller);
        let (import, _) = create_operation(&db, &caller);
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE operations SET capability='tagging.submit',created_at=10
                  WHERE operation_id=?1",
                [waiting_tagging.as_str()],
            )
            .unwrap();
            conn.execute(
                "UPDATE operations SET created_at=20 WHERE operation_id=?1",
                [import.as_str()],
            )
            .unwrap();
        }

        let queue = Arc::new(OperationQueue::default());
        let import_lease = queue
            .try_reserve_operation(&db, import.as_str(), "imports.submit")
            .unwrap()
            .expect("an AI plan awaiting the user's explicit start holds no queue slot");
        drop(import_lease);
        assert_eq!(
            queue
                .claim_next_for_capability(&db, Some("imports.submit"))
                .unwrap()
                .as_deref(),
            Some(import.as_str()),
            "automatic import dispatch must skip an AI plan awaiting GUI start"
        );
        queue.release(import.as_str()).unwrap();

        let tagging_lease = queue
            .try_reserve_operation(&db, waiting_tagging.as_str(), "tagging.submit")
            .unwrap()
            .expect("the GUI may explicitly start the waiting tagging operation");
        drop(tagging_lease);
    }

    #[test]
    fn child_task_binding_is_idempotent_and_blocks_cross_step_reuse() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = caller(&db, "session_ops_bravo", "instance_ops_bravo");
        let (operation_ref, step_ref) = create_operation(&db, &caller);

        let task_ref = ResourceRef::generated("batch");
        let bound = bind_child_task(
            &db,
            &caller,
            &operation_ref,
            &step_ref,
            "ai_batch",
            &task_ref,
        )
        .unwrap();
        let replayed = bind_child_task(
            &db,
            &caller,
            &operation_ref,
            &step_ref,
            "ai_batch",
            &task_ref,
        )
        .unwrap();
        assert_eq!(bound, replayed);
        assert_eq!(
            child_task_binding(&db, &caller, &operation_ref, &step_ref)
                .unwrap()
                .unwrap(),
            bound
        );
        assert_eq!(
            error_code(
                bind_child_task(
                    &db,
                    &caller,
                    &operation_ref,
                    &step_ref,
                    "ai_batch",
                    &ResourceRef::generated("batch")
                )
                .unwrap_err()
            ),
            "RESOURCE_CONFLICT"
        );
    }

    #[test]
    fn startup_recovery_requires_resume_and_keeps_unknown_effect_visible() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = caller(&db, "session_ops_charlie", "instance_ops_charlie");
        let (operation_ref, step_ref) = create_operation(&db, &caller);
        register_item(
            &db,
            &caller,
            &operation_ref,
            &step_ref,
            "operation_item_unknown",
            0,
            Some("fingerprint-unknown"),
            Some("target-unknown"),
        )
        .unwrap();
        {
            let conn = db.lock().unwrap();
            db_operations::update_item(
                &conn,
                operation_ref.as_str(),
                step_ref.as_str(),
                "operation_item_unknown",
                "outcome_unknown",
                "unknown",
                Some("OUTCOME_UNKNOWN"),
                now_unix_seconds().unwrap(),
            )
            .unwrap();
        }

        let summaries = reconcile_startup(&db).unwrap();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].unknown_items, 1);
        assert!(summaries[0].requires_resume);
        let snapshot = plans_service::get_operation(&db, &caller, &operation_ref).unwrap();
        assert_eq!(snapshot.state, OperationState::Interrupted);
        assert_eq!(
            snapshot.phase,
            crate::contracts::agent::OperationPhase::Reconciling
        );

        let resumed = resume_operation(&db, &caller, &operation_ref).unwrap();
        assert_eq!(resumed.state, OperationState::Queued);
        assert_eq!(
            resumed.phase,
            crate::contracts::agent::OperationPhase::Ready
        );
        assert!(recovery_summaries(&db).unwrap().is_empty());
    }

    #[test]
    fn another_session_cannot_cancel_the_operation() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let owner = caller(&db, "session_ops_owner", "instance_ops_owner");
        let (operation_ref, _) = create_operation(&db, &owner);
        let other = caller(&db, "session_ops_other", "instance_ops_other");

        assert_eq!(
            error_code(cancel_operation(&db, &other, &operation_ref).unwrap_err()),
            "UNAUTHORIZED"
        );
    }
    #[test]
    fn resumed_gui_can_continue_after_restart_but_other_principal_cannot() {
        let root = tempfile::tempdir().unwrap();
        let db = ready_db(root.path());
        let old = caller(&db, "session_before_restart", "instance_before_restart");
        let (operation, step) = create_operation(&db, &old);
        reconcile_startup(&db).unwrap();
        let mut foreign = {
            let conn = db.lock().unwrap();
            authorization::build_caller_context(
                &conn,
                TrustedAdapterIdentity::Pi {
                    principal_id: "principal_foreign_restart".into(),
                    session_id: "session_foreign_restart".into(),
                    instance_id: "instance_foreign_restart".into(),
                },
                ResourceRef::generated("request"),
                Vec::new(),
            )
            .unwrap()
        };
        // Even a trusted GUI adapter cannot adopt another principal's operation.
        foreign.kind = crate::contracts::agent::CallerKind::Gui;
        assert_eq!(
            error_code(resume_operation(&db, &foreign, &operation).unwrap_err()),
            "UNAUTHORIZED"
        );
        let current = caller(&db, "session_after_restart", "instance_after_restart");
        resume_operation(&db, &current, &operation).unwrap();
        register_item(
            &db,
            &current,
            &operation,
            &step,
            "item_resumed_after_restart",
            0,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            error_code(cancel_operation(&db, &old, &operation).unwrap_err()),
            "UNAUTHORIZED"
        );
        cancel_operation(&db, &current, &operation).unwrap();
    }

    #[test]
    fn failed_receipt_insert_rolls_back_item_and_progress() {
        let root = tempfile::tempdir().unwrap();
        let db = ready_db(root.path());
        let owner = caller(&db, "session_receipt_atomic", "instance_receipt_atomic");
        let (operation, step) = create_operation(&db, &owner);
        let item = register_item(
            &db,
            &owner,
            &operation,
            &step,
            "item_receipt_atomic",
            0,
            None,
            None,
        )
        .unwrap();
        {
            let conn = db.lock().unwrap();
            conn.execute_batch("CREATE TRIGGER fail_receipt BEFORE INSERT ON operation_receipts BEGIN SELECT RAISE(ABORT, 'injected receipt failure'); END;").unwrap();
        }
        assert!(record_item_state(
            &db,
            &owner,
            &operation,
            &step,
            &item,
            "succeeded",
            "registered",
            None,
            &ResourceRef::generated("receipt"),
            "item_registered",
            Some("asset:42")
        )
        .is_err());
        let conn = db.lock().unwrap();
        let actual =
            db_operations::operation_item(&conn, operation.as_str(), step.as_str(), item.as_str())
                .unwrap()
                .unwrap();
        assert_eq!(actual.state, "pending");
        assert_eq!(actual.revision, 1);
        assert!(
            db_operations::receipts(&conn, operation.as_str(), step.as_str())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            crate::db::plans::operation(&conn, operation.as_str())
                .unwrap()
                .unwrap()
                .completed,
            0
        );
    }
}
