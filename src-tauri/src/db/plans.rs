//! Persisted operation plans, approvals, and the operations they authorize.
//!
//! This module owns SQL only. It stores the exact prepared scope and the
//! approval consumption state; it never decides whether a plan may run. The
//! subject, library, generation, and expiry checks live in `services::plans`.

use rusqlite::{Connection, OptionalExtension};

use crate::error::AppResult;

use super::operations::{self, NewOperationStep};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanRecord {
    pub plan_id: String,
    pub principal_id: String,
    pub session_id: Option<String>,
    pub instance_id: String,
    pub capability: String,
    pub review_kind: String,
    pub review_ref: String,
    pub plan_hash: String,
    pub scope_ref: String,
    pub source_ref: Option<String>,
    pub selection_ref: Option<String>,
    pub target_ref: Option<String>,
    pub service_ref: Option<String>,
    pub service_revision: Option<String>,
    pub model: Option<String>,
    pub item_count: i64,
    pub summary: String,
    pub details_json: Option<String>,
    pub max_requests: i64,
    pub max_tokens: i64,
    pub max_seconds: i64,
    pub max_items: i64,
    pub currency: Option<String>,
    pub amount_microunits: Option<i64>,
    pub library_id: String,
    pub library_epoch: String,
    pub grant_epoch: String,
    pub review_status: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub approved_at: Option<i64>,
    pub approval_expires_at: Option<i64>,
    pub consumed_at: Option<i64>,
    pub consumed_operation_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct NewPlan {
    pub plan_id: String,
    pub principal_id: String,
    pub session_id: Option<String>,
    pub instance_id: String,
    pub capability: String,
    pub review_kind: String,
    pub review_ref: String,
    pub plan_hash: String,
    pub scope_ref: String,
    pub source_ref: Option<String>,
    pub selection_ref: Option<String>,
    pub target_ref: Option<String>,
    pub service_ref: Option<String>,
    pub service_revision: Option<String>,
    pub model: Option<String>,
    pub item_count: i64,
    pub summary: String,
    pub details_json: Option<String>,
    pub max_requests: i64,
    pub max_tokens: i64,
    pub max_seconds: i64,
    pub max_items: i64,
    pub currency: Option<String>,
    pub amount_microunits: Option<i64>,
    pub library_id: String,
    pub library_epoch: String,
    pub grant_epoch: String,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecord {
    pub operation_id: String,
    pub plan_id: String,
    pub principal_id: String,
    pub capability: String,
    pub idempotency_key: String,
    pub library_id: String,
    pub library_epoch: String,
    pub state: String,
    pub phase: String,
    pub revision: i64,
    pub total: Option<i64>,
    pub completed: i64,
    pub failed: i64,
}

#[derive(Debug, Clone)]
pub struct NewOperation {
    pub operation_id: String,
    pub step_id: String,
    pub plan_id: String,
    pub principal_id: String,
    pub capability: String,
    pub idempotency_key: String,
    pub library_id: String,
    pub library_epoch: String,
    pub state: String,
    pub phase: String,
    pub revision: i64,
    pub total: Option<i64>,
    pub completed: i64,
    pub failed: i64,
    pub created_at: i64,
}

pub(super) fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS plans (
            plan_id TEXT PRIMARY KEY,
            principal_id TEXT NOT NULL REFERENCES authorization_principals(principal_id)
                ON DELETE CASCADE,
            session_id TEXT,
            instance_id TEXT NOT NULL,
            capability TEXT NOT NULL,
            review_kind TEXT NOT NULL CHECK(review_kind IN ('ordinary','permanent_trash')),
            review_ref TEXT NOT NULL,
            plan_hash TEXT NOT NULL,
            scope_ref TEXT NOT NULL,
            source_ref TEXT,
            selection_ref TEXT,
            target_ref TEXT,
            service_ref TEXT,
            service_revision TEXT,
            model TEXT,
            item_count INTEGER NOT NULL,
            summary TEXT NOT NULL,
            details_json TEXT,
            max_requests INTEGER NOT NULL,
            max_tokens INTEGER NOT NULL,
            max_seconds INTEGER NOT NULL,
            max_items INTEGER NOT NULL,
            currency TEXT,
            amount_microunits INTEGER,
            library_id TEXT NOT NULL,
            library_epoch TEXT NOT NULL,
            grant_epoch TEXT NOT NULL,
            review_status TEXT NOT NULL
                CHECK(review_status IN ('pending','approved','expired','invalidated','consumed')),
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            approved_at INTEGER,
            approval_expires_at INTEGER,
            consumed_at INTEGER,
            consumed_operation_id TEXT,
            CHECK((currency IS NULL) = (amount_microunits IS NULL))
        );
        CREATE INDEX IF NOT EXISTS idx_plans_principal
            ON plans(principal_id, review_status, created_at DESC);
        CREATE TABLE IF NOT EXISTS plan_approvals (
            approval_id TEXT PRIMARY KEY,
            plan_id TEXT NOT NULL REFERENCES plans(plan_id) ON DELETE CASCADE,
            principal_id TEXT NOT NULL,
            plan_hash TEXT NOT NULL,
            approved_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            consumed_at INTEGER,
            consumed_operation_id TEXT,
            consumed_idempotency_key TEXT
        );
        CREATE INDEX IF NOT EXISTS idx_plan_approvals_plan
            ON plan_approvals(plan_id, consumed_at);
        CREATE TABLE IF NOT EXISTS operations (
            operation_id TEXT PRIMARY KEY,
            plan_id TEXT NOT NULL,
            principal_id TEXT NOT NULL,
            capability TEXT NOT NULL,
            idempotency_key TEXT NOT NULL,
            library_id TEXT NOT NULL,
            library_epoch TEXT NOT NULL,
            state TEXT NOT NULL,
            phase TEXT NOT NULL,
            revision INTEGER NOT NULL,
            total INTEGER,
            completed INTEGER NOT NULL,
            failed INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            UNIQUE(principal_id, library_epoch, capability, idempotency_key)
        );
        CREATE INDEX IF NOT EXISTS idx_operations_plan
            ON operations(plan_id);
        ",
    )?;
    let columns: Vec<String> = {
        let mut statement = conn.prepare("PRAGMA table_info(plans)")?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<_, _>>()?;
        columns
    };
    if !columns.iter().any(|column| column == "details_json") {
        conn.execute_batch("ALTER TABLE plans ADD COLUMN details_json TEXT")?;
    }
    Ok(())
}

pub fn insert_plan(conn: &Connection, plan: &NewPlan) -> AppResult<()> {
    conn.execute(
        "INSERT INTO plans(
            plan_id,principal_id,session_id,instance_id,capability,review_kind,review_ref,
            plan_hash,scope_ref,source_ref,selection_ref,target_ref,service_ref,
            service_revision,model,item_count,summary,details_json,max_requests,max_tokens,max_seconds,
            max_items,currency,amount_microunits,library_id,library_epoch,grant_epoch,
            review_status,created_at,expires_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,
                  ?21,?22,?23,?24,?25,?26,?27,'pending',?28,?29)",
        rusqlite::params![
            plan.plan_id,
            plan.principal_id,
            plan.session_id,
            plan.instance_id,
            plan.capability,
            plan.review_kind,
            plan.review_ref,
            plan.plan_hash,
            plan.scope_ref,
            plan.source_ref,
            plan.selection_ref,
            plan.target_ref,
            plan.service_ref,
            plan.service_revision,
            plan.model,
            plan.item_count,
            plan.summary,
            plan.details_json,
            plan.max_requests,
            plan.max_tokens,
            plan.max_seconds,
            plan.max_items,
            plan.currency,
            plan.amount_microunits,
            plan.library_id,
            plan.library_epoch,
            plan.grant_epoch,
            plan.created_at,
            plan.expires_at,
        ],
    )?;
    Ok(())
}

const PLAN_COLUMNS: &str =
    "plan_id,principal_id,session_id,instance_id,capability,review_kind,review_ref,
        plan_hash,scope_ref,source_ref,selection_ref,target_ref,service_ref,
        service_revision,model,item_count,summary,details_json,max_requests,max_tokens,max_seconds,
        max_items,currency,amount_microunits,library_id,library_epoch,grant_epoch,
        review_status,created_at,expires_at,approved_at,approval_expires_at,
        consumed_at,consumed_operation_id";

fn map_plan(row: &rusqlite::Row<'_>) -> rusqlite::Result<PlanRecord> {
    Ok(PlanRecord {
        plan_id: row.get(0)?,
        principal_id: row.get(1)?,
        session_id: row.get(2)?,
        instance_id: row.get(3)?,
        capability: row.get(4)?,
        review_kind: row.get(5)?,
        review_ref: row.get(6)?,
        plan_hash: row.get(7)?,
        scope_ref: row.get(8)?,
        source_ref: row.get(9)?,
        selection_ref: row.get(10)?,
        target_ref: row.get(11)?,
        service_ref: row.get(12)?,
        service_revision: row.get(13)?,
        model: row.get(14)?,
        item_count: row.get(15)?,
        summary: row.get(16)?,
        details_json: row.get(17)?,
        max_requests: row.get(18)?,
        max_tokens: row.get(19)?,
        max_seconds: row.get(20)?,
        max_items: row.get(21)?,
        currency: row.get(22)?,
        amount_microunits: row.get(23)?,
        library_id: row.get(24)?,
        library_epoch: row.get(25)?,
        grant_epoch: row.get(26)?,
        review_status: row.get(27)?,
        created_at: row.get(28)?,
        expires_at: row.get(29)?,
        approved_at: row.get(30)?,
        approval_expires_at: row.get(31)?,
        consumed_at: row.get(32)?,
        consumed_operation_id: row.get(33)?,
    })
}

pub fn plan(conn: &Connection, plan_id: &str) -> AppResult<Option<PlanRecord>> {
    Ok(conn
        .query_row(
            &format!("SELECT {PLAN_COLUMNS} FROM plans WHERE plan_id=?1"),
            [plan_id],
            map_plan,
        )
        .optional()?)
}

pub fn pending_plans_for_library(
    conn: &Connection,
    library_id: &str,
    library_epoch: &str,
    now: i64,
) -> AppResult<Vec<PlanRecord>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {PLAN_COLUMNS} FROM plans
          WHERE library_id=?1 AND library_epoch=?2
            AND review_status='pending' AND expires_at>?3
          ORDER BY created_at DESC"
    ))?;
    let rows = statement.query_map(rusqlite::params![library_id, library_epoch, now], map_plan)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Atomically mark a pending plan approved and record its approval in one transaction.
pub fn approve_plan(
    conn: &Connection,
    plan_id: &str,
    approval_id: &str,
    plan_hash: &str,
    now: i64,
    approval_expires_at: i64,
) -> AppResult<bool> {
    let transaction = conn.unchecked_transaction()?;
    let changed = transaction.execute(
        "UPDATE plans
            SET review_status='approved',approved_at=?1,approval_expires_at=?2
          WHERE plan_id=?3 AND plan_hash=?4 AND review_status='pending'",
        rusqlite::params![now, approval_expires_at, plan_id, plan_hash],
    )?;
    if changed != 1 {
        return Ok(false);
    }
    transaction.execute(
        "INSERT INTO plan_approvals(
            approval_id,plan_id,principal_id,plan_hash,approved_at,expires_at
         ) SELECT ?1,?2,principal_id,?3,?4,?5 FROM plans WHERE plan_id=?2",
        rusqlite::params![approval_id, plan_id, plan_hash, now, approval_expires_at],
    )?;
    transaction.commit()?;
    Ok(true)
}

/// Mark an approved (or pending) plan as no longer usable without changing its scope.
pub fn invalidate_plan(conn: &Connection, plan_id: &str, status: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE plans SET review_status=?1
          WHERE plan_id=?2 AND review_status IN ('pending','approved')",
        rusqlite::params![status, plan_id],
    )?;
    Ok(())
}

pub fn operation(conn: &Connection, operation_id: &str) -> AppResult<Option<OperationRecord>> {
    Ok(conn
        .query_row(
            "SELECT operation_id,plan_id,principal_id,capability,idempotency_key,library_id,
                    library_epoch,state,phase,revision,total,completed,failed
               FROM operations WHERE operation_id=?1",
            [operation_id],
            map_operation,
        )
        .optional()?)
}

pub fn operation_by_idempotency(
    conn: &Connection,
    principal_id: &str,
    library_epoch: &str,
    capability: &str,
    idempotency_key: &str,
) -> AppResult<Option<OperationRecord>> {
    Ok(conn
        .query_row(
            "SELECT operation_id,plan_id,principal_id,capability,idempotency_key,library_id,
                    library_epoch,state,phase,revision,total,completed,failed
               FROM operations
              WHERE principal_id=?1 AND library_epoch=?2 AND capability=?3 AND idempotency_key=?4",
            rusqlite::params![principal_id, library_epoch, capability, idempotency_key],
            map_operation,
        )
        .optional()?)
}

fn map_operation(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationRecord> {
    Ok(OperationRecord {
        operation_id: row.get(0)?,
        plan_id: row.get(1)?,
        principal_id: row.get(2)?,
        capability: row.get(3)?,
        idempotency_key: row.get(4)?,
        library_id: row.get(5)?,
        library_epoch: row.get(6)?,
        state: row.get(7)?,
        phase: row.get(8)?,
        revision: row.get(9)?,
        total: row.get(10)?,
        completed: row.get(11)?,
        failed: row.get(12)?,
    })
}

pub fn insert_operation(conn: &Connection, operation: &NewOperation) -> AppResult<()> {
    conn.execute(
        "INSERT INTO operations(
            operation_id,plan_id,principal_id,capability,idempotency_key,library_id,
            library_epoch,state,phase,revision,total,completed,failed,created_at,updated_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?14)",
        rusqlite::params![
            operation.operation_id,
            operation.plan_id,
            operation.principal_id,
            operation.capability,
            operation.idempotency_key,
            operation.library_id,
            operation.library_epoch,
            operation.state,
            operation.phase,
            operation.revision,
            operation.total,
            operation.completed,
            operation.failed,
            operation.created_at,
        ],
    )?;
    Ok(())
}

/// Consume the single unconsumed approval bound to a plan and register the
/// operation in the same transaction. Returns `false` when no unconsumed
/// approval with a matching hash is available.
pub fn consume_and_create_operation(
    conn: &Connection,
    plan_id: &str,
    plan_hash: &str,
    operation: &NewOperation,
    now: i64,
) -> AppResult<bool> {
    consume_and_create_operation_with(conn, plan_id, plan_hash, operation, now, |_, _| Ok(()))
}

pub fn consume_and_create_operation_with(
    conn: &Connection,
    plan_id: &str,
    plan_hash: &str,
    operation: &NewOperation,
    now: i64,
    effect: impl FnOnce(&rusqlite::Transaction<'_>, &NewOperation) -> AppResult<()>,
) -> AppResult<bool> {
    let transaction = conn.unchecked_transaction()?;
    let consumed = transaction.execute(
        "UPDATE plan_approvals
            SET consumed_at=?1,consumed_operation_id=?2,consumed_idempotency_key=?3
          WHERE plan_id=?4 AND plan_hash=?5 AND consumed_at IS NULL AND expires_at>?1
            AND EXISTS (
                SELECT 1
                  FROM plans
                 WHERE plans.plan_id=plan_approvals.plan_id
                   AND plans.plan_hash=?5
                   AND plans.review_status='approved'
            )",
        rusqlite::params![
            now,
            operation.operation_id,
            operation.idempotency_key,
            plan_id,
            plan_hash,
        ],
    )?;
    if consumed != 1 {
        return Ok(false);
    }
    transaction.execute(
        "UPDATE plans
            SET review_status='consumed',consumed_at=?1,consumed_operation_id=?2
          WHERE plan_id=?3",
        rusqlite::params![now, operation.operation_id, plan_id],
    )?;
    transaction.execute(
        "INSERT INTO operations(
            operation_id,plan_id,principal_id,capability,idempotency_key,library_id,
            library_epoch,state,phase,revision,total,completed,failed,created_at,updated_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?14)",
        rusqlite::params![
            operation.operation_id,
            operation.plan_id,
            operation.principal_id,
            operation.capability,
            operation.idempotency_key,
            operation.library_id,
            operation.library_epoch,
            operation.state,
            operation.phase,
            operation.revision,
            operation.total,
            operation.completed,
            operation.failed,
            operation.created_at,
        ],
    )?;
    operations::insert_step(
        &transaction,
        &NewOperationStep {
            operation_id: operation.operation_id.clone(),
            step_id: operation.step_id.clone(),
            ordinal: 0,
            capability: operation.capability.clone(),
            total: operation.total,
            created_at: operation.created_at,
        },
    )?;
    effect(&transaction, operation)?;
    transaction.commit()?;
    Ok(true)
}
