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
    let draft = submit_draft(&view)?;
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

/// Build the `imports.submit` plan draft for a prepared import plan.
///
/// This is the single place that decides what an import submission plan looks
/// like. `plans::submit_plan_with_effect` recomputes `plan_hash` from the draft
/// passed by the caller and invalidates the stored plan when the two differ, so
/// any second construction site would drift into `PlanChanged` as soon as one of
/// them changes. `confirm` builds it here today; the protocol-level
/// `imports.prepare` adapter (see `docs/contracts/agent-v1.md` §3.4) must reuse
/// this function instead of assembling its own.
///
/// `ImportPlanView` itself is the GUI workbench checklist and is deliberately
/// not the `PlanPreview` that `imports.prepare` declares: it carries no
/// `reviewRef`, because a checklist nobody is asked to approve has nothing to
/// review. The reviewable preview is this draft handed to `plans::prepare_plan`.
pub(crate) fn submit_draft(
    view: &import_prepare::ImportPlanView,
) -> AppResult<plan_service::PlanDraft> {
    Ok(plan_service::PlanDraft {
        capability: CapabilityId::ImportsSubmit,
        scope: PlanScopeSummary {
            scope_ref: view.plan_ref.clone(),
            source_ref: view.source_ref.clone(),
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
    })
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

// Execution context reconstruction used to live here as `execution_context`,
// hard-coded to `principal_gui_host`. It is now `operation_dispatch::execution_caller`,
// which rebuilds the same principal/session/instance/grantEpoch fields from the
// persisted plan and operation rows but derives the caller kind from
// `plan.origin_kind` instead of asserting the GUI principal. Keeping a second,
// narrower copy here is what let a Pi-created import operation be accepted and
// then never drained.

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
    /// The submitted plan row must be exactly what `submit_draft` produces.
    ///
    /// `plans::submit_plan_with_effect` recomputes `plan_hash` from the caller's
    /// draft and invalidates the stored plan on any difference, so this asserts
    /// the invariant that keeps `confirm` and the future protocol-level
    /// `imports.prepare` adapter from drifting apart: both must build their
    /// draft through `submit_draft`, and what ends up persisted must match it.
    #[test]
    fn the_submitted_plan_is_exactly_the_draft_submit_draft_builds() {
        let root = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let file = source.path().join("ok.jpg");
        image::RgbImage::new(8, 8).save(&file).unwrap();
        let (db, caller) = fixture(root.path());
        let plan = prepare(&db, &caller, &file);
        confirm(&db, &caller, &plan, &ResourceRef::generated("idempotency")).unwrap();

        let view = import_prepare::get_import_plan(&db, &caller, &plan).unwrap();
        let draft = submit_draft(&view).unwrap();
        let conn = db.lock().unwrap();
        // Exactly one reviewed plan exists; its scope points back at the
        // prepared import plan.
        let plan_id: String = conn
            .query_row(
                "SELECT plan_id FROM plans WHERE scope_ref = ?1",
                [plan.as_str()],
                |r| r.get(0),
            )
            .unwrap();
        let row = plans::plan(&conn, &plan_id).unwrap().unwrap();
        assert_eq!(row.scope_ref, draft.scope.scope_ref.as_str());
        assert_eq!(
            row.source_ref.as_deref(),
            draft.scope.source_ref.as_ref().map(ResourceRef::as_str)
        );
        assert_eq!(row.item_count, i64::from(draft.item_count));
        assert_eq!(row.summary, draft.summary.as_str());
        assert_eq!(row.max_seconds, draft.budget.max_seconds as i64);
        assert_eq!(row.max_items, i64::from(draft.budget.max_items));
        assert_eq!(row.capability, "imports.submit");
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
