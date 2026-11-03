//! Operation plans, trusted GUI approvals, and atomic submission.
//!
//! `prepare_plan` records the exact scope a domain capability wants to execute;
//! it never creates an operation. Only the trusted GUI can approve a specific
//! plan. `submit_plan` validates the caller, library and generation, then
//! consumes the single matching approval and creates the operation in one short
//! transaction. A repeated idempotency key returns the original operation.

use std::time::{SystemTime, UNIX_EPOCH};

use sha2::{Digest, Sha256};

use crate::contracts::agent::{
    AgentErrorCode, AgentOutput, ApprovalResult, BudgetSummary, CallerKind, CapabilityId,
    Instructions, LibraryScope, MoneyLimit, OperationPhase, OperationSnapshot, OperationState,
    PlanPreview, PlanReviewStatus, PlanScopeSummary, ProtocolVersion, ResourceRef, ReviewKind,
    ShortText, SubmissionResult, CAPABILITY_DESCRIPTORS,
};
use crate::contracts::caller::{check_capability_entry, CallerContext};
use crate::db::authorization;
use crate::db::plans::{self, NewOperation, NewPlan, PlanRecord};
use crate::db::{libraries, plans as plans_db};
use crate::error::{AppError, AppResult};
use crate::state::Database;

/// A plan is only valid this long unless the caller asks for less.
const PLAN_TTL_SECONDS: u64 = 30 * 60;
/// Once a user approves, the approval is usable for this long.
const APPROVAL_TTL_SECONDS: u64 = 10 * 60;

#[derive(Debug, Clone)]
pub struct PlanDraft {
    pub capability: CapabilityId,
    pub scope: PlanScopeSummary,
    pub item_count: u32,
    pub summary: Instructions,
    /// Opaque domain-owned snapshot. It participates in planHash and is never
    /// exposed to an untrusted capability output.
    pub details_json: Option<String>,
    pub budget: BudgetSummary,
    pub ttl_seconds: u32,
}

pub fn prepare_plan(
    db: &Database,
    caller: &CallerContext,
    draft: PlanDraft,
) -> AppResult<PlanPreview> {
    let review_kind = review_kind_for_capability(draft.capability)?;
    if draft.item_count == 0 || draft.item_count > draft.budget.max_items {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let ttl = if draft.ttl_seconds == 0 {
        PLAN_TTL_SECONDS
    } else {
        draft.ttl_seconds as u64
    };
    let now = now_unix_seconds()?;
    let expires_at = now
        .checked_add(ttl as i64)
        .filter(|value| *value <= u32::MAX as i64)
        .ok_or_else(|| AppError::from(AgentErrorCode::InvalidInput))?;
    let plan_id = ResourceRef::generated("plan");
    let review_ref = ResourceRef::generated("review");
    let plan_hash = plan_hash(
        &draft.capability,
        review_kind,
        &draft.scope,
        &draft.budget,
        &draft,
    )?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    // Resolve the caller's own display name right away so the adapter that prepared
    // the plan sees the same provenance the reviewer will see. The persisted row only
    // stores the kind; the label is always resolved from the live pairing record, so a
    // renamed pairing shows its current name on every surface.
    let origin_label = origin_label_for(&conn, caller.kind, caller.principal_id.as_str())?;

    let preview = PlanPreview {
        schema_version: ProtocolVersion::V1,
        plan_id: plan_id.clone(),
        capability: draft.capability,
        library: caller.library.clone(),
        review_status: PlanReviewStatus::Pending,
        review_kind,
        origin_kind: caller.kind,
        // Only the kind is persisted; the label is always resolved from the live pairing
        // record so a renamed pairing shows its current name on every surface.
        origin_label,
        item_count: draft.item_count,
        summary: draft.summary.clone(),
        review_ref: review_ref.clone(),
        expires_at_unix_seconds: expires_at as u32,
        scope: draft.scope.clone(),
        budget: draft.budget.clone(),
    };
    preview.validate_output().map_err(AppError::from)?;

    let new_plan = NewPlan {
        plan_id: plan_id.as_str().to_string(),
        principal_id: caller.principal_id.as_str().to_string(),
        session_id: caller
            .session_id
            .as_ref()
            .map(|value| value.as_str().to_string()),
        instance_id: caller.instance_id.as_str().to_string(),
        capability: capability_to_string(draft.capability)?,
        review_kind: review_kind_to_string(review_kind).to_string(),
        review_ref: review_ref.as_str().to_string(),
        plan_hash,
        scope_ref: draft.scope.scope_ref.as_str().to_string(),
        source_ref: draft
            .scope
            .source_ref
            .as_ref()
            .map(|value| value.as_str().to_string()),
        selection_ref: draft
            .scope
            .selection_ref
            .as_ref()
            .map(|value| value.as_str().to_string()),
        target_ref: draft
            .scope
            .target_ref
            .as_ref()
            .map(|value| value.as_str().to_string()),
        service_ref: draft
            .scope
            .service_ref
            .as_ref()
            .map(|value| value.as_str().to_string()),
        service_revision: draft
            .scope
            .service_revision
            .as_ref()
            .map(|value| value.as_str().to_string()),
        model: draft
            .scope
            .model
            .as_ref()
            .map(|value| value.as_str().to_string()),
        item_count: draft.item_count as i64,
        summary: draft.summary.as_str().to_string(),
        details_json: draft.details_json.clone(),
        max_requests: draft.budget.max_requests as i64,
        max_tokens: draft.budget.max_tokens as i64,
        max_seconds: draft.budget.max_seconds as i64,
        max_items: draft.budget.max_items as i64,
        currency: draft
            .budget
            .monetary_limit
            .as_ref()
            .map(|limit| limit.currency.as_str().to_string()),
        amount_microunits: draft
            .budget
            .monetary_limit
            .as_ref()
            .map(|limit| limit.amount_microunits as i64),
        library_id: caller.library.library_id.as_str().to_string(),
        library_epoch: caller.library.library_epoch.as_str().to_string(),
        grant_epoch: caller.grant_epoch.as_str().to_string(),
        created_at: now,
        expires_at,
        origin_kind: Some(caller_kind_to_string(caller.kind).to_string()),
    };
    plans::insert_plan(&conn, &new_plan)?;
    Ok(preview)
}

pub fn pending_plans(db: &Database, caller: &CallerContext) -> AppResult<Vec<PlanPreview>> {
    check_capability_entry(CapabilityId::PlansApprove, caller).map_err(AppError::from)?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let records = plans::pending_plans_for_library(
        &conn,
        caller.library.library_id.as_str(),
        caller.library.library_epoch.as_str(),
        now_unix_seconds()?,
    )?;
    records
        .iter()
        .map(|record| plan_preview(&conn, record))
        .collect()
}

/// Trusted GUI confirmation for one specific plan. The `review_ref` must match
/// the value handed out with that plan; it is not a capability the model holds.
pub fn approve_plan(
    db: &Database,
    caller: &CallerContext,
    plan_ref: &ResourceRef,
    review_ref: &ResourceRef,
) -> AppResult<ApprovalResult> {
    check_capability_entry(CapabilityId::PlansApprove, caller).map_err(AppError::from)?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let record = load_plan_for_review(&conn, caller, plan_ref)?;
    if record.review_ref != review_ref.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let now = now_unix_seconds()?;
    if record.expires_at <= now {
        plans_db::invalidate_plan(&conn, &record.plan_id, "expired")?;
        return Err(AppError::from(AgentErrorCode::PlanExpired));
    }
    if record.review_status == "approved"
        && record
            .approval_expires_at
            .is_some_and(|expires| expires <= now)
    {
        plans_db::invalidate_plan(&conn, &record.plan_id, "expired")?;
        return Err(AppError::from(AgentErrorCode::PlanExpired));
    }
    let approval_expires_at = now
        .checked_add(APPROVAL_TTL_SECONDS as i64)
        .map(|value| value.min(record.expires_at))
        .ok_or_else(|| AppError::internal("审批期限计算失败"))?;
    match record.review_status.as_str() {
        "pending" => {
            let approval_id = ResourceRef::generated("approval").as_str().to_string();
            let approved = plans_db::approve_plan(
                &conn,
                &record.plan_id,
                &approval_id,
                &record.plan_hash,
                now,
                approval_expires_at,
            )?;
            if !approved {
                return Err(AppError::from(AgentErrorCode::PlanChanged));
            }
        }
        "approved" => {}
        "expired" => return Err(AppError::from(AgentErrorCode::PlanExpired)),
        "consumed" | "invalidated" => return Err(AppError::from(AgentErrorCode::PlanChanged)),
        _ => return Err(AppError::from(AgentErrorCode::Internal)),
    }
    Ok(ApprovalResult {
        schema_version: ProtocolVersion::V1,
        plan_id: plan_ref.clone(),
        review_status: PlanReviewStatus::Approved,
    })
}

/// Trusted-GUI rejection. Rejecting only marks the plan unusable; it never
/// touches operations, so a rejected request cannot leave a partial write behind.
/// A plan that was already approved is refused rather than silently un-approved:
/// revoking a live approval is a different decision (the user cancels the
/// operation), and doing it here would race with a submit already in flight.
pub fn reject_plan(
    db: &Database,
    caller: &CallerContext,
    plan_ref: &ResourceRef,
    review_ref: &ResourceRef,
) -> AppResult<ApprovalResult> {
    check_capability_entry(CapabilityId::PlansApprove, caller).map_err(AppError::from)?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let record = load_plan_for_review(&conn, caller, plan_ref)?;
    if record.review_ref != review_ref.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    let now = now_unix_seconds()?;
    if record.expires_at <= now {
        plans_db::invalidate_plan(&conn, &record.plan_id, "expired")?;
        return Err(AppError::from(AgentErrorCode::PlanExpired));
    }
    match record.review_status.as_str() {
        "pending" => {
            if !plans_db::reject_plan(&conn, &record.plan_id)? {
                // Lost the race to a concurrent approval or rejection.
                return Err(AppError::from(AgentErrorCode::PlanChanged));
            }
        }
        // Rejecting an already rejected plan is a no-op so a retry after a lost
        // response does not report a spurious failure.
        "invalidated" => {}
        "expired" => return Err(AppError::from(AgentErrorCode::PlanExpired)),
        "approved" | "consumed" => return Err(AppError::from(AgentErrorCode::PlanChanged)),
        _ => return Err(AppError::from(AgentErrorCode::Internal)),
    }
    Ok(ApprovalResult {
        schema_version: ProtocolVersion::V1,
        plan_id: plan_ref.clone(),
        review_status: PlanReviewStatus::Invalidated,
    })
}

/// Domain submit boundary: verify and consume the approval, then register the
/// operation and idempotency key in the same short transaction.
pub fn submit_plan(
    db: &Database,
    caller: &CallerContext,
    plan_ref: &ResourceRef,
    idempotency_key: &ResourceRef,
    current: &PlanDraft,
) -> AppResult<SubmissionResult> {
    submit_plan_with_effect(
        db,
        caller,
        plan_ref,
        idempotency_key,
        current,
        |_, _| Ok(()),
    )
}

/// Domain adapter hook for child-task creation. The effect runs inside the
/// same transaction that consumes approval and inserts the operation/step.
pub fn submit_plan_with_effect(
    db: &Database,
    caller: &CallerContext,
    plan_ref: &ResourceRef,
    idempotency_key: &ResourceRef,
    current: &PlanDraft,
    effect: impl FnOnce(&rusqlite::Transaction<'_>, &NewOperation) -> AppResult<()>,
) -> AppResult<SubmissionResult> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let record = plans::plan(&conn, plan_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    require_plan_owner(caller, &record)?;
    let capability = parse_capability(&record.capability)?;
    check_capability_entry(capability, caller).map_err(AppError::from)?;

    // Same key + same plan returns the original operation; same key + different
    // plan is a conflict and must not consume the second approval.
    if let Some(existing) = plans::operation_by_idempotency(
        &conn,
        caller.principal_id.as_str(),
        caller.library.library_epoch.as_str(),
        &record.capability,
        idempotency_key.as_str(),
    )? {
        if existing.plan_id != record.plan_id {
            return Err(AppError::from(AgentErrorCode::IdempotencyConflict));
        }
        return Ok(SubmissionResult {
            schema_version: ProtocolVersion::V1,
            operation: operation_snapshot(&conn, caller, &existing)?,
            replayed: true,
        });
    }

    let current_review_kind = review_kind_for_capability(current.capability)?;
    let current_hash = plan_hash(
        &current.capability,
        current_review_kind,
        &current.scope,
        &current.budget,
        current,
    )?;
    if current_hash != record.plan_hash {
        plans_db::invalidate_plan(&conn, &record.plan_id, "invalidated")?;
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }

    let now = now_unix_seconds()?;
    if record.expires_at <= now {
        plans_db::invalidate_plan(&conn, &record.plan_id, "expired")?;
        return Err(AppError::from(AgentErrorCode::PlanExpired));
    }
    if record.grant_epoch != caller.grant_epoch.as_str() {
        return Err(AppError::from(AgentErrorCode::GrantRevoked));
    }
    match record.review_status.as_str() {
        "approved" => {}
        "pending" => return Err(AppError::from(AgentErrorCode::ApprovalRequired)),
        "expired" => return Err(AppError::from(AgentErrorCode::PlanExpired)),
        "invalidated" => return Err(AppError::from(AgentErrorCode::PlanChanged)),
        // A consumed plan under a fresh idempotency key needs a new approval.
        "consumed" => return Err(AppError::from(AgentErrorCode::ApprovalRequired)),
        _ => return Err(AppError::from(AgentErrorCode::Internal)),
    }
    if record
        .approval_expires_at
        .is_none_or(|expires| expires <= now)
    {
        plans_db::invalidate_plan(&conn, &record.plan_id, "expired")?;
        return Err(AppError::from(AgentErrorCode::PlanExpired));
    }

    let operation_id = ResourceRef::generated("operation");
    let step_id = ResourceRef::generated("step").as_str().to_string();
    let new_operation = NewOperation {
        operation_id: operation_id.as_str().to_string(),
        step_id,
        plan_id: record.plan_id.clone(),
        principal_id: caller.principal_id.as_str().to_string(),
        capability: record.capability.clone(),
        idempotency_key: idempotency_key.as_str().to_string(),
        library_id: caller.library.library_id.as_str().to_string(),
        library_epoch: caller.library.library_epoch.as_str().to_string(),
        state: operation_state_to_string(OperationState::Queued).to_string(),
        phase: operation_phase_to_string(OperationPhase::Ready).to_string(),
        revision: 1,
        total: Some(record.item_count),
        completed: 0,
        failed: 0,
        created_at: now,
    };
    let created = plans_db::consume_and_create_operation_with(
        &conn,
        &record.plan_id,
        &record.plan_hash,
        &new_operation,
        now,
        effect,
    )?;
    if !created {
        // Another submit consumed the approval between our checks.
        if let Some(existing) = plans::operation_by_idempotency(
            &conn,
            caller.principal_id.as_str(),
            caller.library.library_epoch.as_str(),
            &record.capability,
            idempotency_key.as_str(),
        )? {
            if existing.plan_id == record.plan_id {
                return Ok(SubmissionResult {
                    schema_version: ProtocolVersion::V1,
                    operation: operation_snapshot(&conn, caller, &existing)?,
                    replayed: true,
                });
            }
        }
        return Err(AppError::from(AgentErrorCode::ApprovalRequired));
    }
    let created = plans::operation(&conn, operation_id.as_str())?
        .ok_or_else(|| AppError::internal("操作创建后无法读取"))?;
    Ok(SubmissionResult {
        schema_version: ProtocolVersion::V1,
        operation: operation_snapshot(&conn, caller, &created)?,
        replayed: false,
    })
}

pub fn get_operation(
    db: &Database,
    caller: &CallerContext,
    operation_ref: &ResourceRef,
) -> AppResult<OperationSnapshot> {
    check_capability_entry(CapabilityId::OperationsGet, caller).map_err(AppError::from)?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let record = plans::operation(&conn, operation_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if record.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    // GUI history survives WebView restarts; external callers remain isolated
    // to their authenticated conversation/connection.
    if caller.kind != crate::contracts::agent::CallerKind::Gui {
        let owner = plans::plan(&conn, &record.plan_id)?
            .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
        require_plan_owner(caller, &owner)?;
    }
    operation_snapshot(&conn, caller, &record)
}

fn load_plan_for_review(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
    plan_ref: &ResourceRef,
) -> AppResult<PlanRecord> {
    let record = plans::plan(conn, plan_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if record.library_id != caller.library.library_id.as_str()
        || record.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(record)
}

fn require_plan_owner(caller: &CallerContext, record: &PlanRecord) -> AppResult<()> {
    if record.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    if record.session_id.as_deref() != caller.session_id.as_ref().map(ResourceRef::as_str)
        || record.instance_id != caller.instance_id.as_str()
    {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    if record.library_id != caller.library.library_id.as_str()
        || record.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(())
}

pub(crate) fn validate_caller(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
) -> AppResult<()> {
    validate_caller_subject(conn, caller)?;
    let library =
        libraries::current(conn)?.ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
    if library.health != libraries::LibraryHealth::Ready {
        return Err(AppError::from(AgentErrorCode::LibraryOffline));
    }
    Ok(())
}

fn validate_caller_subject(conn: &rusqlite::Connection, caller: &CallerContext) -> AppResult<()> {
    let principal = authorization::principal(conn, caller.principal_id.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::GrantRevoked))?;
    if principal.grant_epoch != caller.grant_epoch.as_str() {
        return Err(AppError::from(AgentErrorCode::GrantRevoked));
    }
    let library =
        libraries::current(conn)?.ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
    if library.library_id != caller.library.library_id.as_str()
        || library.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(())
}

pub(crate) fn review_kind_for_capability(capability: CapabilityId) -> AppResult<ReviewKind> {
    let descriptor = CAPABILITY_DESCRIPTORS
        .iter()
        .find(|descriptor| descriptor.capability == capability)
        .ok_or_else(|| AppError::from(AgentErrorCode::UnsupportedCapability))?;
    match descriptor.approval {
        crate::contracts::agent::ApprovalRequirement::Plan => Ok(ReviewKind::Ordinary),
        crate::contracts::agent::ApprovalRequirement::PermanentTrash => {
            Ok(ReviewKind::PermanentTrash)
        }
        _ => Err(AppError::from(AgentErrorCode::InvalidInput)),
    }
}

/// Resolve the human name shown next to a plan's origin.
///
/// Only the adapter kind is persisted on the plan row; the name always comes from the
/// live pairing record, so a renamed pairing shows its current name and a plan prepared
/// by a client that was later removed simply has no label (never a stale one).
fn origin_label_for(
    conn: &rusqlite::Connection,
    kind: CallerKind,
    principal_id: &str,
) -> AppResult<Option<ShortText>> {
    match kind {
        CallerKind::Mcp => crate::db::mcp_pairings::find_by_principal(conn, principal_id)?
            .map(|pairing| to_bounded(pairing.label))
            .transpose(),
        _ => Ok(None),
    }
}

pub(crate) fn plan_preview(
    conn: &rusqlite::Connection,
    record: &PlanRecord,
) -> AppResult<PlanPreview> {
    let origin_kind = match record.origin_kind.as_deref() {
        Some(value) => parse_caller_kind(value)?,
        // Rows written before A5-02 recorded the adapter identity. Guessing here
        // would present a fabricated provenance to the reviewer, so an old plan is
        // reported as the trusted local interface and the label stays absent.
        None => CallerKind::Gui,
    };
    let origin_label = origin_label_for(conn, origin_kind, &record.principal_id)?;
    let budget = BudgetSummary {
        max_requests: u32_of(record.max_requests)?,
        max_tokens: u32_of(record.max_tokens)?,
        max_seconds: u32_of(record.max_seconds)?,
        max_items: u32_of(record.max_items)?,
        monetary_limit: match (&record.currency, record.amount_microunits) {
            (Some(currency), Some(amount)) => Some(MoneyLimit {
                currency: to_bounded(currency.clone())?,
                amount_microunits: u32_of(amount)?,
            }),
            _ => None,
        },
    };
    Ok(PlanPreview {
        schema_version: ProtocolVersion::V1,
        plan_id: to_ref(record.plan_id.clone())?,
        capability: parse_capability(&record.capability)?,
        library: LibraryScope {
            library_id: to_ref(record.library_id.clone())?,
            library_epoch: to_ref(record.library_epoch.clone())?,
        },
        review_status: parse_review_status(&record.review_status)?,
        review_kind: parse_review_kind(&record.review_kind)?,
        origin_kind,
        origin_label,
        item_count: u32_of(record.item_count)?,
        summary: to_bounded(record.summary.clone())?,
        review_ref: to_ref(record.review_ref.clone())?,
        expires_at_unix_seconds: u32_of(record.expires_at)?,
        scope: PlanScopeSummary {
            scope_ref: to_ref(record.scope_ref.clone())?,
            source_ref: opt_ref(&record.source_ref)?,
            selection_ref: opt_ref(&record.selection_ref)?,
            target_ref: opt_ref(&record.target_ref)?,
            service_ref: opt_ref(&record.service_ref)?,
            service_revision: opt_ref(&record.service_revision)?,
            model: record.model.clone().map(to_bounded).transpose()?,
        },
        budget,
    })
}

pub(crate) fn operation_snapshot(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
    record: &plans::OperationRecord,
) -> AppResult<OperationSnapshot> {
    let _ = conn;
    if record.library_id != caller.library.library_id.as_str()
        || record.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    let state = parse_operation_state(&record.state)?;
    let phase = parse_operation_phase(&record.phase)?;
    Ok(OperationSnapshot {
        schema_version: ProtocolVersion::V1,
        operation_id: to_ref(record.operation_id.clone())?,
        library: caller.library.clone(),
        revision: u32_of(record.revision)?,
        state,
        phase,
        completed: u32_of(record.completed)?,
        failed: u32_of(record.failed)?,
        total: record.total.map(u32_of).transpose()?,
        result_selection_ref: None,
        error_code: None,
        can_cancel: matches!(state, OperationState::Queued | OperationState::Running),
        can_resume: caller.kind == crate::contracts::agent::CallerKind::Gui
            && record.principal_id == caller.principal_id.as_str()
            && state == OperationState::Interrupted
            && phase == OperationPhase::Reconciling,
    })
}

fn plan_hash(
    capability: &CapabilityId,
    review_kind: ReviewKind,
    scope: &PlanScopeSummary,
    budget: &BudgetSummary,
    draft: &PlanDraft,
) -> AppResult<String> {
    let canonical = serde_json::json!({
        "capability": capability_to_string(*capability)?,
        "reviewKind": review_kind_to_string(review_kind),
        "scopeRef": scope.scope_ref.as_str(),
        "sourceRef": scope.source_ref.as_ref().map(ResourceRef::as_str),
        "selectionRef": scope.selection_ref.as_ref().map(ResourceRef::as_str),
        "targetRef": scope.target_ref.as_ref().map(ResourceRef::as_str),
        "serviceRef": scope.service_ref.as_ref().map(ResourceRef::as_str),
        "serviceRevision": scope.service_revision.as_ref().map(ResourceRef::as_str),
        "model": scope.model.as_ref().map(ShortText::as_str),
        "itemCount": draft.item_count,
        "summary": draft.summary.as_str(),
        "detailsJson": draft.details_json,
        "budget": {
            "maxRequests": budget.max_requests,
            "maxTokens": budget.max_tokens,
            "maxSeconds": budget.max_seconds,
            "maxItems": budget.max_items,
            "monetary": budget.monetary_limit.as_ref().map(|limit| serde_json::json!({
                "currency": limit.currency.as_str(),
                "amountMicrounits": limit.amount_microunits,
            })),
        },
    });
    let bytes =
        serde_json::to_vec(&canonical).map_err(|_| AppError::internal("计划摘要序列化失败"))?;
    let mut hasher = Sha256::new();
    hasher.update(&bytes);
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn to_bounded<T: serde::de::DeserializeOwned>(value: String) -> AppResult<T> {
    serde_json::from_value(serde_json::Value::String(value))
        .map_err(|_| AppError::internal("持久计划字段损坏"))
}

fn to_ref(value: String) -> AppResult<ResourceRef> {
    to_bounded(value)
}

fn opt_ref(value: &Option<String>) -> AppResult<Option<ResourceRef>> {
    value.clone().map(to_ref).transpose()
}

fn u32_of(value: i64) -> AppResult<u32> {
    u32::try_from(value).map_err(|_| AppError::internal("持久计划数值越界"))
}

fn capability_to_string(capability: CapabilityId) -> AppResult<String> {
    serde_json::to_value(capability)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .ok_or_else(|| AppError::internal("能力序列化失败"))
}

fn caller_kind_to_string(kind: CallerKind) -> &'static str {
    match kind {
        CallerKind::Gui => "gui",
        CallerKind::Pi => "pi",
        CallerKind::Mcp => "mcp",
    }
}

fn parse_caller_kind(value: &str) -> AppResult<CallerKind> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .map_err(|_| AppError::internal("持久计划来源损坏"))
}

fn parse_capability(value: &str) -> AppResult<CapabilityId> {
    serde_json::from_value(serde_json::Value::String(value.to_string()))
        .map_err(|_| AppError::from(AgentErrorCode::Internal))
}

fn review_kind_to_string(kind: ReviewKind) -> &'static str {
    match kind {
        ReviewKind::Ordinary => "ordinary",
        ReviewKind::PermanentTrash => "permanent_trash",
    }
}

fn parse_review_kind(value: &str) -> AppResult<ReviewKind> {
    match value {
        "ordinary" => Ok(ReviewKind::Ordinary),
        "permanent_trash" => Ok(ReviewKind::PermanentTrash),
        _ => Err(AppError::internal("未知审核类别")),
    }
}

fn parse_review_status(value: &str) -> AppResult<PlanReviewStatus> {
    match value {
        "pending" => Ok(PlanReviewStatus::Pending),
        "approved" => Ok(PlanReviewStatus::Approved),
        "expired" => Ok(PlanReviewStatus::Expired),
        "invalidated" => Ok(PlanReviewStatus::Invalidated),
        "consumed" => Ok(PlanReviewStatus::Consumed),
        _ => Err(AppError::internal("未知计划状态")),
    }
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

fn parse_operation_state(value: &str) -> AppResult<OperationState> {
    match value {
        "queued" => Ok(OperationState::Queued),
        "running" => Ok(OperationState::Running),
        "succeeded" => Ok(OperationState::Succeeded),
        "partially_succeeded" => Ok(OperationState::PartiallySucceeded),
        "failed" => Ok(OperationState::Failed),
        "cancelled" => Ok(OperationState::Cancelled),
        "interrupted" => Ok(OperationState::Interrupted),
        "outcome_unknown" => Ok(OperationState::OutcomeUnknown),
        _ => Err(AppError::internal("未知操作状态")),
    }
}

fn operation_phase_to_string(phase: OperationPhase) -> &'static str {
    match phase {
        OperationPhase::Ready => "ready",
        OperationPhase::Executing => "executing",
        OperationPhase::WaitingReview => "waiting_review",
        OperationPhase::WaitingTags => "waiting_tags",
        OperationPhase::Cancelling => "cancelling",
        OperationPhase::Reconciling => "reconciling",
        OperationPhase::Complete => "complete",
    }
}

fn parse_operation_phase(value: &str) -> AppResult<OperationPhase> {
    match value {
        "ready" => Ok(OperationPhase::Ready),
        "executing" => Ok(OperationPhase::Executing),
        "waiting_review" => Ok(OperationPhase::WaitingReview),
        "waiting_tags" => Ok(OperationPhase::WaitingTags),
        "cancelling" => Ok(OperationPhase::Cancelling),
        "reconciling" => Ok(OperationPhase::Reconciling),
        "complete" => Ok(OperationPhase::Complete),
        _ => Err(AppError::internal("未知操作阶段")),
    }
}

fn now_unix_seconds() -> AppResult<i64> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| AppError::msg("系统时间异常"))?;
    Ok(now.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;
    use crate::db::libraries::{self, LibraryHealth};
    use crate::services::authorization::{self, TrustedAdapterIdentity};

    fn ready_db(root: &std::path::Path) -> Database {
        let conn = init_memory().unwrap();
        crate::services::managed_library::adopt_initial_root(&conn, root).unwrap();
        Database::new(conn)
    }

    fn gui_caller(db: &Database) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Gui {
                session_id: "session_plan_fixture".to_string(),
                instance_id: "instance_plan_fixture".to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn pi_caller(db: &Database, principal: &str, session: &str, instance: &str) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Pi {
                principal_id: principal.to_string(),
                session_id: session.to_string(),
                instance_id: instance.to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    /// The MCP adapter has no session, which is exactly why an external plan can
    /// never resolve a GUI-owned directory handle.
    fn mcp_caller(db: &Database, principal: &str, instance: &str) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Mcp {
                principal_id: principal.to_string(),
                instance_id: instance.to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn scope() -> PlanScopeSummary {
        PlanScopeSummary {
            scope_ref: ResourceRef::generated("scope"),
            source_ref: Some(ResourceRef::generated("directory")),
            selection_ref: None,
            target_ref: None,
            service_ref: None,
            service_revision: None,
            model: None,
        }
    }

    fn budget(max_items: u32) -> BudgetSummary {
        BudgetSummary {
            max_requests: 0,
            max_tokens: 0,
            max_seconds: 300,
            max_items,
            monetary_limit: None,
        }
    }

    fn draft(capability: CapabilityId, item_count: u32, ttl: u32) -> PlanDraft {
        PlanDraft {
            capability,
            scope: scope(),
            item_count,
            summary: serde_json::from_value(serde_json::json!("复制素材入库")).unwrap(),
            details_json: None,
            budget: budget(item_count.max(1)),
            ttl_seconds: ttl,
        }
    }

    fn token() -> ResourceRef {
        ResourceRef::generated("idempotency")
    }

    fn error_code(error: AppError) -> String {
        error.code().to_string()
    }

    #[test]
    fn prepare_records_scope_without_creating_an_operation() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let preview = prepare_plan(&db, &caller, draft(CapabilityId::ImportsSubmit, 2, 0)).unwrap();
        assert_eq!(preview.review_status, PlanReviewStatus::Pending);
        assert_eq!(preview.review_kind, ReviewKind::Ordinary);
        assert_eq!(preview.item_count, 2);
        let conn = db.lock().unwrap();
        let operations: i64 = conn
            .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(operations, 0, "等待审核时不得创建执行 operation");
        assert!(plans::plan(&conn, preview.plan_id.as_str())
            .unwrap()
            .is_some());
    }

    #[test]
    fn submit_requires_a_trusted_gui_approval() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let current = draft(CapabilityId::ImportsSubmit, 1, 0);
        let preview = prepare_plan(&db, &caller, current.clone()).unwrap();
        let key = token();

        let error = submit_plan(&db, &caller, &preview.plan_id, &key, &current).unwrap_err();
        assert_eq!(error_code(error), "APPROVAL_REQUIRED");
        {
            let conn = db.lock().unwrap();
            let operations: i64 = conn
                .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
                .unwrap();
            assert_eq!(operations, 0);
        }

        let approval = approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        assert_eq!(approval.review_status, PlanReviewStatus::Approved);
        let result = submit_plan(&db, &caller, &preview.plan_id, &key, &current).unwrap();
        assert!(!result.replayed);
        assert_eq!(result.operation.state, OperationState::Queued);
        assert_eq!(result.operation.phase, OperationPhase::Ready);
    }

    #[test]
    fn external_plan_shows_its_paired_client_name_and_rejecting_writes_nothing() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let mcp = mcp_caller(&db, "principal_mcp_a502", "instance_mcp_a502");
        {
            let conn = db.lock().unwrap();
            crate::db::mcp_pairings::insert(
                &conn,
                "mcpconn_00112233aabbccdd",
                "本地编辑器",
                "principal_mcp_a502",
                &[
                    "assets.search".to_string(),
                    "assets.trash.prepare".to_string(),
                ],
                1_700_000_000,
            )
            .unwrap();
        }
        let current = draft(CapabilityId::AssetsTrashSubmit, 3, 0);
        let preview = prepare_plan(&db, &mcp, current.clone()).unwrap();
        assert_eq!(preview.origin_kind, CallerKind::Mcp);
        {
            let conn = db.lock().unwrap();
            let stored: Option<String> = conn
                .query_row(
                    "SELECT origin_kind FROM plans WHERE plan_id=?1",
                    [preview.plan_id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(
                stored.as_deref(),
                Some("mcp"),
                "计划必须记住是哪个适配器发起的"
            );
        }

        let gui = gui_caller(&db);
        let pending = pending_plans(&db, &gui).unwrap();
        let listed = pending
            .iter()
            .find(|candidate| candidate.plan_id.as_str() == preview.plan_id.as_str())
            .expect("外部计划必须出现在可信界面的待审列表里");
        assert_eq!(
            listed.origin_kind,
            CallerKind::Mcp,
            "审核界面必须能区分外部请求和本机操作"
        );
        assert_eq!(
            listed.origin_label.as_ref().map(|label| label.as_str()),
            Some("本地编辑器"),
            "审核界面必须能指名是哪个外部客户端发起的，而不是让用户自己猜"
        );

        let rejected = reject_plan(&db, &gui, &preview.plan_id, &preview.review_ref).unwrap();
        assert_eq!(rejected.review_status, PlanReviewStatus::Invalidated);
        {
            let conn = db.lock().unwrap();
            let operations: i64 = conn
                .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
                .unwrap();
            assert_eq!(operations, 0, "拒绝不得产生任何执行记录");
            let status: String = conn
                .query_row(
                    "SELECT review_status FROM plans WHERE plan_id=?1",
                    [preview.plan_id.as_str()],
                    |row| row.get(0),
                )
                .unwrap();
            assert_eq!(status, "invalidated");
        }
        // 拒绝之后外部再提交必须失败，而且不能变成可执行任务。
        assert_eq!(
            error_code(submit_plan(&db, &mcp, &preview.plan_id, &token(), &current).unwrap_err()),
            "PLAN_CHANGED"
        );
        // 重复拒绝幂等，方便界面重试。
        reject_plan(&db, &gui, &preview.plan_id, &preview.review_ref).unwrap();
        // 拒绝权限只在可信界面：外部客户端自己不能拒绝。
        let other = mcp_caller(&db, "principal_mcp_a502b", "instance_mcp_a502b");
        assert_eq!(
            error_code(
                reject_plan(&db, &other, &preview.plan_id, &preview.review_ref).unwrap_err()
            ),
            "UNAUTHORIZED"
        );
    }

    #[test]
    fn an_approved_plan_cannot_be_rejected_and_a_wrong_review_reference_is_refused() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let mcp = mcp_caller(&db, "principal_mcp_a502c", "instance_mcp_a502c");
        let preview =
            prepare_plan(&db, &mcp, draft(CapabilityId::AssetsTrashSubmit, 1, 0)).unwrap();
        let gui = gui_caller(&db);

        assert_eq!(
            error_code(
                reject_plan(
                    &db,
                    &gui,
                    &preview.plan_id,
                    &ResourceRef::generated("review")
                )
                .unwrap_err()
            ),
            "UNAUTHORIZED"
        );

        approve_plan(&db, &gui, &preview.plan_id, &preview.review_ref).unwrap();
        assert_eq!(
            error_code(reject_plan(&db, &gui, &preview.plan_id, &preview.review_ref).unwrap_err()),
            "PLAN_CHANGED",
            "已批准的计划不能被静默撤销批准，撤销执行要走取消任务"
        );
    }

    #[test]
    fn gui_reviews_external_plan_while_original_session_submits() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let pi = pi_caller(
            &db,
            "principal_pi_review",
            "session_pi_review",
            "instance_pi_review",
        );
        let current = draft(CapabilityId::ImportsSubmit, 1, 0);
        let preview = prepare_plan(&db, &pi, current.clone()).unwrap();
        let gui = gui_caller(&db);

        assert_eq!(
            error_code(pending_plans(&db, &pi).unwrap_err()),
            "UNAUTHORIZED"
        );
        let pending = pending_plans(&db, &gui).unwrap();
        assert!(pending
            .iter()
            .any(|candidate| candidate.plan_id.as_str() == preview.plan_id.as_str()));

        let other_session = pi_caller(
            &db,
            "principal_pi_review",
            "session_pi_other",
            "instance_pi_other",
        );
        assert_eq!(
            error_code(
                approve_plan(&db, &other_session, &preview.plan_id, &preview.review_ref)
                    .unwrap_err()
            ),
            "UNAUTHORIZED"
        );
        approve_plan(&db, &gui, &preview.plan_id, &preview.review_ref).unwrap();
        assert_eq!(
            error_code(
                submit_plan(&db, &other_session, &preview.plan_id, &token(), &current).unwrap_err()
            ),
            "UNAUTHORIZED"
        );
        let result = submit_plan(&db, &pi, &preview.plan_id, &token(), &current).unwrap();
        assert_eq!(result.operation.state, OperationState::Queued);
    }

    #[test]
    fn mismatched_review_reference_cannot_approve_a_plan() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let preview = prepare_plan(&db, &caller, draft(CapabilityId::ImportsSubmit, 1, 0)).unwrap();
        let forged = ResourceRef::generated("review");
        let error = approve_plan(&db, &caller, &preview.plan_id, &forged).unwrap_err();
        assert_eq!(error_code(error), "UNAUTHORIZED");
    }

    #[test]
    fn repeated_key_returns_original_operation_and_conflicts_on_other_plan() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let first_current = draft(CapabilityId::ImportsSubmit, 1, 0);
        let second_current = draft(CapabilityId::ImportsSubmit, 1, 0);
        let first = prepare_plan(&db, &caller, first_current.clone()).unwrap();
        let second = prepare_plan(&db, &caller, second_current.clone()).unwrap();
        approve_plan(&db, &caller, &first.plan_id, &first.review_ref).unwrap();
        approve_plan(&db, &caller, &second.plan_id, &second.review_ref).unwrap();
        let key = token();

        let original = submit_plan(&db, &caller, &first.plan_id, &key, &first_current).unwrap();
        assert!(!original.replayed);
        let replay = submit_plan(&db, &caller, &first.plan_id, &key, &first_current).unwrap();
        assert!(replay.replayed);
        assert_eq!(
            replay.operation.operation_id,
            original.operation.operation_id
        );

        let error = submit_plan(&db, &caller, &second.plan_id, &key, &second_current).unwrap_err();
        assert_eq!(error_code(error), "IDEMPOTENCY_CONFLICT");
        // The conflicting submit must not consume the second plan's approval.
        let conn = db.lock().unwrap();
        let unconsumed: i64 = conn
            .query_row(
                "SELECT count(*) FROM plan_approvals WHERE plan_id=?1 AND consumed_at IS NULL",
                [second.plan_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unconsumed, 1);
        let operations: i64 = conn
            .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(operations, 1);
    }

    #[test]
    fn expired_plan_and_generation_change_cannot_consume_an_old_approval() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let expired_current = draft(CapabilityId::ImportsSubmit, 1, 0);
        let expired = prepare_plan(&db, &caller, expired_current.clone()).unwrap();
        approve_plan(&db, &caller, &expired.plan_id, &expired.review_ref).unwrap();
        // Force the plan to read as expired.
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE plans SET expires_at=?1 WHERE plan_id=?2",
                rusqlite::params![now_unix_seconds().unwrap() - 1, expired.plan_id.as_str()],
            )
            .unwrap();
        }
        let error =
            submit_plan(&db, &caller, &expired.plan_id, &token(), &expired_current).unwrap_err();
        assert_eq!(error_code(error), "PLAN_EXPIRED");

        let live_current = draft(CapabilityId::ImportsSubmit, 1, 0);
        let live = prepare_plan(&db, &caller, live_current.clone()).unwrap();
        approve_plan(&db, &caller, &live.plan_id, &live.review_ref).unwrap();
        // Revoking every grant advances the principal generation.
        authorization::revoke_all_directory_grants(&db, &caller).unwrap();
        let error = submit_plan(&db, &caller, &live.plan_id, &token(), &live_current).unwrap_err();
        assert_eq!(error_code(error), "GRANT_REVOKED");
    }

    #[test]
    fn changed_scope_or_service_invalidates_the_plan_without_consuming_approval() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let current = draft(CapabilityId::ImportsSubmit, 1, 0);
        let preview = prepare_plan(&db, &caller, current.clone()).unwrap();
        approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();

        let mut changed = current.clone();
        changed.scope.scope_ref = ResourceRef::generated("scope");
        changed.scope.service_ref = Some(ResourceRef::generated("service"));
        changed.scope.service_revision = Some(ResourceRef::generated("revision"));
        changed.scope.model = Some(serde_json::from_value(serde_json::json!("new-model")).unwrap());

        let error = submit_plan(&db, &caller, &preview.plan_id, &token(), &changed).unwrap_err();
        assert_eq!(error_code(error), "PLAN_CHANGED");

        let conn = db.lock().unwrap();
        let status: String = conn
            .query_row(
                "SELECT review_status FROM plans WHERE plan_id=?1",
                [preview.plan_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "invalidated");
        let unconsumed: i64 = conn
            .query_row(
                "SELECT count(*) FROM plan_approvals WHERE plan_id=?1 AND consumed_at IS NULL",
                [preview.plan_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(unconsumed, 1);
        let operations: i64 = conn
            .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(operations, 0);
    }

    #[test]
    fn expired_approval_cannot_be_reused_or_refreshed() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let current = draft(CapabilityId::ImportsSubmit, 1, 0);
        let preview = prepare_plan(&db, &caller, current.clone()).unwrap();
        approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE plans SET approval_expires_at=?1 WHERE plan_id=?2",
                rusqlite::params![now_unix_seconds().unwrap() - 1, preview.plan_id.as_str()],
            )
            .unwrap();
        }

        assert_eq!(
            error_code(
                approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap_err()
            ),
            "PLAN_EXPIRED"
        );
        assert_eq!(
            error_code(
                submit_plan(&db, &caller, &preview.plan_id, &token(), &current).unwrap_err()
            ),
            "PLAN_EXPIRED"
        );
        let conn = db.lock().unwrap();
        let status: String = conn
            .query_row(
                "SELECT review_status FROM plans WHERE plan_id=?1",
                [preview.plan_id.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(status, "expired");
    }

    #[test]
    fn offline_library_cannot_consume_an_approval() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let current = draft(CapabilityId::ImportsSubmit, 1, 0);
        let preview = prepare_plan(&db, &caller, current.clone()).unwrap();
        approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        {
            let conn = db.lock().unwrap();
            let library = libraries::current(&conn).unwrap().unwrap();
            libraries::set_health(&conn, &library, LibraryHealth::Offline).unwrap();
        }

        let error = submit_plan(&db, &caller, &preview.plan_id, &token(), &current).unwrap_err();
        assert_eq!(error_code(error), "LIBRARY_OFFLINE");
    }

    #[test]
    fn permanent_trash_plan_is_stored_with_its_own_review_kind() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let preview =
            prepare_plan(&db, &caller, draft(CapabilityId::TrashEmptySubmit, 1, 0)).unwrap();
        assert_eq!(preview.review_kind, ReviewKind::PermanentTrash);
        // Ordinary plan capabilities cannot masquerade as a permanent-trash plan.
        let conn = db.lock().unwrap();
        let stored = plans::plan(&conn, preview.plan_id.as_str())
            .unwrap()
            .unwrap();
        assert_eq!(stored.review_kind, "permanent_trash");
    }

    #[test]
    fn two_concurrent_submits_accept_exactly_once() {
        let managed = tempfile::tempdir().unwrap();
        let db = std::sync::Arc::new(ready_db(managed.path()));
        let caller = gui_caller(&db);
        let current = draft(CapabilityId::ImportsSubmit, 1, 0);
        let preview = prepare_plan(&db, &caller, current.clone()).unwrap();
        approve_plan(&db, &caller, &preview.plan_id, &preview.review_ref).unwrap();
        let key = token();

        let mut handles = Vec::new();
        for _ in 0..2 {
            let db = std::sync::Arc::clone(&db);
            let caller = caller.clone();
            let plan_id = preview.plan_id.clone();
            let key = key.clone();
            let current = current.clone();
            handles.push(std::thread::spawn(move || {
                submit_plan(&db, &caller, &plan_id, &key, &current)
            }));
        }
        let results: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        let ids: Vec<_> = results
            .iter()
            .map(|result| {
                result
                    .as_ref()
                    .unwrap()
                    .operation
                    .operation_id
                    .as_str()
                    .to_string()
            })
            .collect();
        assert_eq!(ids[0], ids[1], "并发提交应返回同一操作");
        assert_eq!(
            results
                .iter()
                .filter(|result| !result.as_ref().unwrap().replayed)
                .count(),
            1,
            "只应有一次真实受理"
        );
        let conn = db.lock().unwrap();
        let operations: i64 = conn
            .query_row("SELECT count(*) FROM operations", [], |row| row.get(0))
            .unwrap();
        assert_eq!(operations, 1);
    }
    #[test]
    fn operation_reads_isolate_external_sessions_but_gui_can_review_restart() {
        let root = tempfile::tempdir().unwrap();
        let db = ready_db(root.path());
        let owner = pi_caller(
            &db,
            "principal_pi_read",
            "session_pi_read_fixture",
            "instance_pi_read",
        );
        let current = draft(CapabilityId::ImportsSubmit, 1, 0);
        let prepared = prepare_plan(&db, &owner, current.clone()).unwrap();
        let gui = gui_caller(&db);
        approve_plan(&db, &gui, &prepared.plan_id, &prepared.review_ref).unwrap();
        let submitted = submit_plan(&db, &owner, &prepared.plan_id, &token(), &current).unwrap();
        let other = pi_caller(
            &db,
            "principal_pi_read",
            "session_pi_other_read",
            "instance_pi_other_read",
        );
        assert_eq!(
            error_code(get_operation(&db, &other, &submitted.operation.operation_id).unwrap_err()),
            "UNAUTHORIZED"
        );
        assert!(get_operation(&db, &owner, &submitted.operation.operation_id).is_ok());

        let gui_plan = draft(CapabilityId::ImportsSubmit, 1, 0);
        let prepared = prepare_plan(&db, &gui, gui_plan.clone()).unwrap();
        approve_plan(&db, &gui, &prepared.plan_id, &prepared.review_ref).unwrap();
        let accepted = submit_plan(&db, &gui, &prepared.plan_id, &token(), &gui_plan).unwrap();
        crate::services::operations::reconcile_startup(&db).unwrap();
        let mut restarted = gui.clone();
        restarted.session_id = Some(ResourceRef::generated("session"));
        restarted.instance_id = ResourceRef::generated("instance");
        let snapshot = get_operation(&db, &restarted, &accepted.operation.operation_id).unwrap();
        assert!(snapshot.can_resume);
        assert_eq!(snapshot.state, OperationState::Interrupted);
    }
}
