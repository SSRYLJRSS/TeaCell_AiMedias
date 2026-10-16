//! Trusted import acceptance and persistent dispatch bindings.
use std::sync::Mutex;

use crate::contracts::agent::{
    AgentErrorCode, BudgetSummary, CapabilityId, PlanScopeSummary, ResourceRef, SubmissionResult,
};
use crate::contracts::caller::CallerContext;
use crate::db::plans;
use crate::error::{AppError, AppResult};
use crate::services::{import_prepare, plans as plan_service};
use crate::state::Database;

// Serializes acceptance and source verification, never holds the database lock
// across file IO, and never covers execution.
static ACCEPTANCE: Mutex<()> = Mutex::new(());

pub fn confirm(
    db: &Database,
    caller: &CallerContext,
    import_ref: &ResourceRef,
    key: &ResourceRef,
) -> AppResult<SubmissionResult> {
    if caller.kind != crate::contracts::agent::CallerKind::Gui {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let _guard = ACCEPTANCE
        .lock()
        .map_err(|_| AppError::internal("导入受理锁中毒"))?;
    // Replay before loading the expiring preparation. A lost response must not
    // create another plan or require the original source files to still exist.
    {
        let conn = db.lock()?;
        plan_service::validate_caller(&conn, caller)?;
        if let Some(existing) = plans::operation_by_idempotency(
            &conn,
            caller.principal_id.as_str(),
            caller.library.library_epoch.as_str(),
            "imports.submit",
            key.as_str(),
        )? {
            let plan = plans::plan(&conn, &existing.plan_id)?
                .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
            if plan.scope_ref != import_ref.as_str() {
                return Err(AppError::from(AgentErrorCode::IdempotencyConflict));
            }
            return Ok(SubmissionResult {
                schema_version: crate::contracts::agent::ProtocolVersion::V1,
                operation: plan_service::operation_snapshot(&conn, caller, &existing)?,
                replayed: true,
            });
        }
    }
    let view = import_prepare::get_import_plan(db, caller, import_ref)?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| AppError::internal("系统时间无效"))?
        .as_secs() as i64;
    if view.expires_at_unix_seconds <= now {
        return Err(AppError::from(AgentErrorCode::PlanExpired));
    }
    let verified = import_prepare::verify_import_plan(db, caller, import_ref)?;
    if verified.library_changed || !verified.stale_item_refs.is_empty() {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    if view.unsupported_count > 0 {
        return Err(AppError::invalid_arg("请先移除不可处理文件并重新检查"));
    }
    let draft = plan_service::PlanDraft {
        capability: CapabilityId::ImportsSubmit,
        scope: PlanScopeSummary {
            scope_ref: import_ref.clone(),
            source_ref: view.source_ref,
            selection_ref: None,
            target_ref: None,
            service_ref: None,
            service_revision: None,
            model: None,
        },
        item_count: view.item_count,
        summary: serde_json::from_value(serde_json::json!("执行已确认的导入计划"))
            .map_err(|_| AppError::internal("导入摘要构造失败"))?,
        details_json: None,
        budget: BudgetSummary {
            max_requests: 0,
            max_tokens: 0,
            max_seconds: 3600,
            max_items: view.item_count,
            monetary_limit: None,
        },
        ttl_seconds: 0,
    };
    let preview = plan_service::prepare_plan(db, caller, draft.clone())?;
    plan_service::approve_plan(db, caller, &preview.plan_id, &preview.review_ref)?;
    plan_service::submit_plan_with_effect(
        db,
        caller,
        &preview.plan_id,
        key,
        &draft,
        |transaction, operation| {
            let items = crate::db::import_plans::items(transaction, import_ref.as_str())?;
            crate::services::import_execute::seed_import_operation_items(
                transaction,
                &operation.operation_id,
                &operation.step_id,
                &items,
                operation.created_at,
            )
        },
    )
}

pub fn import_ref(db: &Database, operation: &ResourceRef) -> AppResult<ResourceRef> {
    let conn = db.lock()?;
    let op = plans::operation(&conn, operation.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if op.capability != "imports.submit" {
        return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
    }
    let plan = plans::plan(&conn, &op.plan_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let prepared = crate::db::import_plans::plan(&conn, &plan.scope_ref)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if prepared.principal_id != op.principal_id
        || prepared.library_id != op.library_id
        || prepared.library_epoch != op.library_epoch
    {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    ResourceRef::from_trusted(plan.scope_ref).map_err(AppError::from)
}

/// Recover the trusted GUI execution context bound at acceptance (or explicit
/// resume), never from IPC arguments. This does not renew grants or approvals.
pub fn execution_context(db: &Database, operation: &ResourceRef) -> AppResult<CallerContext> {
    let conn = db.lock()?;
    let op = plans::operation(&conn, operation.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    let plan = plans::plan(&conn, &op.plan_id)?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if op.capability != "imports.submit" || op.principal_id != "principal_gui_host" {
        return Err(AppError::from(AgentErrorCode::UnsupportedCapability));
    }
    Ok(CallerContext {
        kind: crate::contracts::agent::CallerKind::Gui,
        principal_id: ResourceRef::from_trusted(op.principal_id)?,
        session_id: plan.session_id.map(ResourceRef::from_trusted).transpose()?,
        instance_id: ResourceRef::from_trusted(plan.instance_id)?,
        request_id: ResourceRef::generated("request"),
        grant_epoch: ResourceRef::from_trusted(plan.grant_epoch)?,
        library: crate::contracts::agent::LibraryScope {
            library_id: ResourceRef::from_trusted(op.library_id)?,
            library_epoch: ResourceRef::from_trusted(op.library_epoch)?,
        },
        origin_chain: Vec::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{authorization, managed_library};
    fn fixture(root: &std::path::Path) -> (Database, CallerContext) {
        let conn = crate::db::init_memory().unwrap();
        managed_library::adopt_initial_root(&conn, root).unwrap();
        let caller = authorization::build_caller_context(
            &conn,
            authorization::TrustedAdapterIdentity::Gui {
                session_id: "session_submission".into(),
                instance_id: "instance_submission".into(),
            },
            ResourceRef::generated("request"),
            vec![],
        )
        .unwrap();
        (Database::new(conn), caller)
    }
    fn prepare(db: &Database, caller: &CallerContext, file: &std::path::Path) -> ResourceRef {
        import_prepare::prepare_import(
            db,
            caller,
            import_prepare::PrepareImportRequest {
                source_ref: None,
                paths: vec![file.to_str().unwrap().into()],
                collection: None,
                rename_pattern: None,
                base_plan_ref: None,
                excluded_item_refs: vec![],
            },
        )
        .unwrap()
        .plan_ref
    }
    #[test]
    fn lost_response_replays_same_operation_even_after_source_disappears() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("测试.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = fixture(root.path());
        let plan = prepare(&db, &caller, &file);
        let key = ResourceRef::generated("idempotency");
        let first = confirm(&db, &caller, &plan, &key).unwrap();
        std::fs::remove_file(file).unwrap();
        let replay = confirm(&db, &caller, &plan, &key).unwrap();
        assert!(replay.replayed);
        assert_eq!(first.operation.operation_id, replay.operation.operation_id);
        let conn = db.lock().unwrap();
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM operations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        assert_eq!(
            conn.query_row("SELECT COUNT(*) FROM plans", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn key_conflict_does_not_create_or_approve_another_plan() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("ok.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = fixture(root.path());
        let a = prepare(&db, &caller, &file);
        let b = prepare(&db, &caller, &file);
        let key = ResourceRef::generated("idempotency");
        confirm(&db, &caller, &a, &key).unwrap();
        assert_eq!(
            confirm(&db, &caller, &b, &key).unwrap_err().code(),
            "IDEMPOTENCY_CONFLICT"
        );
        assert_eq!(
            db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM plans", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn replay_rejects_revoked_context_and_offline_library() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("ok.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = fixture(root.path());
        let plan = prepare(&db, &caller, &file);
        let key = ResourceRef::generated("idempotency");
        confirm(&db, &caller, &plan, &key).unwrap();
        let mut stale = caller.clone();
        stale.grant_epoch = ResourceRef::generated("epoch");
        assert_eq!(
            confirm(&db, &stale, &plan, &key).unwrap_err().code(),
            "GRANT_REVOKED"
        );
        db.lock()
            .unwrap()
            .execute("UPDATE libraries SET health='offline'", [])
            .unwrap();
        assert_eq!(
            confirm(&db, &caller, &plan, &key).unwrap_err().code(),
            "LIBRARY_OFFLINE"
        );
    }
    #[test]
    fn concurrent_confirmations_accept_once() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("ok.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = fixture(root.path());
        let plan = prepare(&db, &caller, &file);
        let key = ResourceRef::generated("idempotency");
        std::thread::scope(|scope| {
            let a = scope.spawn(|| confirm(&db, &caller, &plan, &key).unwrap());
            let b = scope.spawn(|| confirm(&db, &caller, &plan, &key).unwrap());
            assert_eq!(
                a.join().unwrap().operation.operation_id,
                b.join().unwrap().operation.operation_id
            );
        });
        assert_eq!(
            db.lock()
                .unwrap()
                .query_row("SELECT COUNT(*) FROM operations", [], |r| r
                    .get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
}
