//! Persistent operation queue, step/item identity, receipts, and child bindings.
//!
//! This module owns SQL only. It does not execute domain work and never treats
//! a counter or event as a substitute for a receipt.

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::error::{AppError, AppResult};

const TERMINAL_STATES: &str = "'succeeded','partially_succeeded','failed','cancelled'";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationStepRecord {
    pub operation_id: String,
    pub step_id: String,
    pub ordinal: i64,
    pub capability: String,
    pub state: String,
    pub phase: String,
    pub revision: i64,
    pub total: Option<i64>,
    pub completed: i64,
    pub failed: i64,
    pub created_at: i64,
    pub updated_at: i64,
    pub started_at: Option<i64>,
    pub finished_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct NewOperationStep {
    pub operation_id: String,
    pub step_id: String,
    pub ordinal: i64,
    pub capability: String,
    pub total: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationItemRecord {
    pub operation_id: String,
    pub step_id: String,
    pub item_id: String,
    pub ordinal: i64,
    pub state: String,
    pub input_fingerprint: Option<String>,
    pub target_ref: Option<String>,
    pub child_task_kind: Option<String>,
    pub child_task_id: Option<String>,
    pub effect_state: String,
    pub error_code: Option<String>,
    pub revision: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewOperationItem {
    pub operation_id: String,
    pub step_id: String,
    pub item_id: String,
    pub ordinal: i64,
    pub input_fingerprint: Option<String>,
    pub target_ref: Option<String>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationReceiptRecord {
    pub receipt_id: String,
    pub operation_id: String,
    pub step_id: String,
    pub item_id: Option<String>,
    pub revision: i64,
    pub event: String,
    pub input_fingerprint: Option<String>,
    pub target_ref: Option<String>,
    pub child_task_kind: Option<String>,
    pub child_task_id: Option<String>,
    pub db_ref: Option<String>,
    pub effect_state: String,
    pub error_code: Option<String>,
    pub recorded_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewOperationReceipt {
    pub receipt_id: String,
    pub operation_id: String,
    pub step_id: String,
    pub item_id: Option<String>,
    pub revision: i64,
    pub event: String,
    pub input_fingerprint: Option<String>,
    pub target_ref: Option<String>,
    pub child_task_kind: Option<String>,
    pub child_task_id: Option<String>,
    pub db_ref: Option<String>,
    pub effect_state: String,
    pub error_code: Option<String>,
    pub recorded_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildTaskRecord {
    pub operation_id: String,
    pub step_id: String,
    pub task_kind: String,
    pub task_id: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationOwner {
    pub principal_id: String,
    pub session_id: Option<String>,
    pub instance_id: String,
    pub library_id: String,
    pub library_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoverySummary {
    pub operation_id: String,
    pub pending_steps: i64,
    pub running_steps: i64,
    pub unknown_items: i64,
    pub requires_resume: bool,
    pub created_at: i64,
}

pub(super) fn migrate_structure(conn: &Connection) -> AppResult<()> {
    ensure_operation_column(conn, "finished_at", "INTEGER")?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS operation_steps (
            operation_id TEXT NOT NULL REFERENCES operations(operation_id) ON DELETE CASCADE,
            step_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            capability TEXT NOT NULL,
            state TEXT NOT NULL CHECK(state IN (
                'queued','running','succeeded','partially_succeeded','failed',
                'cancelled','interrupted','outcome_unknown'
            )),
            phase TEXT NOT NULL CHECK(phase IN (
                'ready','executing','waiting_review','waiting_tags',
                'cancelling','reconciling','complete'
            )),
            revision INTEGER NOT NULL,
            total INTEGER,
            completed INTEGER NOT NULL DEFAULT 0,
            failed INTEGER NOT NULL DEFAULT 0,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            started_at INTEGER,
            finished_at INTEGER,
            PRIMARY KEY(operation_id, step_id),
            UNIQUE(operation_id, ordinal)
        );
        CREATE INDEX IF NOT EXISTS idx_operation_steps_state
            ON operation_steps(state, phase);

        CREATE TABLE IF NOT EXISTS operation_items (
            operation_id TEXT NOT NULL,
            step_id TEXT NOT NULL,
            item_id TEXT NOT NULL,
            ordinal INTEGER NOT NULL,
            state TEXT NOT NULL CHECK(state IN (
                'pending','running','succeeded','failed','cancelled','outcome_unknown'
            )),
            input_fingerprint TEXT,
            target_ref TEXT,
            child_task_kind TEXT,
            child_task_id TEXT,
            effect_state TEXT NOT NULL CHECK(effect_state IN (
                'none','staged','published','registered','removed','unknown'
            )),
            error_code TEXT,
            revision INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(operation_id, step_id, item_id),
            UNIQUE(operation_id, step_id, ordinal),
            CHECK((child_task_kind IS NULL) = (child_task_id IS NULL)),
            FOREIGN KEY(operation_id, step_id)
                REFERENCES operation_steps(operation_id, step_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_operation_items_state
            ON operation_items(operation_id, step_id, state, ordinal);

        CREATE TABLE IF NOT EXISTS operation_receipts (
            receipt_id TEXT PRIMARY KEY,
            operation_id TEXT NOT NULL,
            step_id TEXT NOT NULL,
            item_id TEXT,
            revision INTEGER NOT NULL,
            event TEXT NOT NULL,
            input_fingerprint TEXT,
            target_ref TEXT,
            child_task_kind TEXT,
            child_task_id TEXT,
            db_ref TEXT,
            effect_state TEXT NOT NULL CHECK(effect_state IN (
                'none','staged','published','registered','removed','unknown'
            )),
            error_code TEXT,
            recorded_at INTEGER NOT NULL,
            FOREIGN KEY(operation_id, step_id)
                REFERENCES operation_steps(operation_id, step_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_operation_receipts_stream
            ON operation_receipts(operation_id, step_id, revision, recorded_at);

        CREATE TABLE IF NOT EXISTS operation_child_tasks (
            operation_id TEXT NOT NULL,
            step_id TEXT NOT NULL,
            task_kind TEXT NOT NULL,
            task_id TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY(operation_id, step_id),
            UNIQUE(task_kind, task_id),
            FOREIGN KEY(operation_id, step_id)
                REFERENCES operation_steps(operation_id, step_id) ON DELETE CASCADE
        );

        CREATE TABLE IF NOT EXISTS operation_recovery (
            operation_id TEXT PRIMARY KEY REFERENCES operations(operation_id) ON DELETE CASCADE,
            pending_steps INTEGER NOT NULL,
            running_steps INTEGER NOT NULL,
            unknown_items INTEGER NOT NULL,
            requires_resume INTEGER NOT NULL CHECK(requires_resume IN (0,1)),
            created_at INTEGER NOT NULL,
            resolved_at INTEGER
        );
        ",
    )?;
    Ok(())
}

pub fn insert_step(conn: &Connection, step: &NewOperationStep) -> AppResult<()> {
    if let Some(existing) = operation_step(conn, &step.operation_id, &step.step_id)? {
        if existing.ordinal == step.ordinal
            && existing.capability == step.capability
            && existing.total == step.total
        {
            return Ok(());
        }
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::ResourceConflict,
        ));
    }
    conn.execute(
        "INSERT INTO operation_steps(
            operation_id,step_id,ordinal,capability,state,phase,revision,total,
            completed,failed,created_at,updated_at
         ) VALUES(?1,?2,?3,?4,'queued','ready',1,?5,0,0,?6,?6)",
        rusqlite::params![
            step.operation_id,
            step.step_id,
            step.ordinal,
            step.capability,
            step.total,
            step.created_at,
        ],
    )?;
    Ok(())
}

pub fn operation_step(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
) -> AppResult<Option<OperationStepRecord>> {
    Ok(conn
        .query_row(
            "SELECT operation_id,step_id,ordinal,capability,state,phase,revision,total,
                    completed,failed,created_at,updated_at,started_at,finished_at
               FROM operation_steps
              WHERE operation_id=?1 AND step_id=?2",
            rusqlite::params![operation_id, step_id],
            map_step,
        )
        .optional()?)
}

pub fn steps(conn: &Connection, operation_id: &str) -> AppResult<Vec<OperationStepRecord>> {
    let mut statement = conn.prepare(
        "SELECT operation_id,step_id,ordinal,capability,state,phase,revision,total,
                completed,failed,created_at,updated_at,started_at,finished_at
           FROM operation_steps
          WHERE operation_id=?1 ORDER BY ordinal",
    )?;
    let rows = statement.query_map([operation_id], map_step)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn map_step(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationStepRecord> {
    Ok(OperationStepRecord {
        operation_id: row.get(0)?,
        step_id: row.get(1)?,
        ordinal: row.get(2)?,
        capability: row.get(3)?,
        state: row.get(4)?,
        phase: row.get(5)?,
        revision: row.get(6)?,
        total: row.get(7)?,
        completed: row.get(8)?,
        failed: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
        started_at: row.get(12)?,
        finished_at: row.get(13)?,
    })
}

pub fn insert_item(conn: &Connection, item: &NewOperationItem) -> AppResult<()> {
    if let Some(existing) = operation_item(conn, &item.operation_id, &item.step_id, &item.item_id)?
    {
        if existing.ordinal == item.ordinal
            && existing.input_fingerprint == item.input_fingerprint
            && existing.target_ref == item.target_ref
        {
            return Ok(());
        }
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::ResourceConflict,
        ));
    }
    conn.execute(
        "INSERT INTO operation_items(
            operation_id,step_id,item_id,ordinal,state,input_fingerprint,target_ref,
            effect_state,revision,updated_at
         ) VALUES(?1,?2,?3,?4,'pending',?5,?6,'none',1,?7)",
        rusqlite::params![
            item.operation_id,
            item.step_id,
            item.item_id,
            item.ordinal,
            item.input_fingerprint,
            item.target_ref,
            item.created_at,
        ],
    )?;
    Ok(())
}

pub fn operation_item(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    item_id: &str,
) -> AppResult<Option<OperationItemRecord>> {
    Ok(conn
        .query_row(
            "SELECT operation_id,step_id,item_id,ordinal,state,input_fingerprint,
                    target_ref,child_task_kind,child_task_id,effect_state,error_code,
                    revision,updated_at
               FROM operation_items
              WHERE operation_id=?1 AND step_id=?2 AND item_id=?3",
            rusqlite::params![operation_id, step_id, item_id],
            map_item,
        )
        .optional()?)
}

pub fn items(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
) -> AppResult<Vec<OperationItemRecord>> {
    let mut statement = conn.prepare(
        "SELECT operation_id,step_id,item_id,ordinal,state,input_fingerprint,
                target_ref,child_task_kind,child_task_id,effect_state,error_code,
                revision,updated_at
           FROM operation_items
          WHERE operation_id=?1 AND step_id=?2 ORDER BY ordinal",
    )?;
    let rows = statement.query_map(rusqlite::params![operation_id, step_id], map_item)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn map_item(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationItemRecord> {
    Ok(OperationItemRecord {
        operation_id: row.get(0)?,
        step_id: row.get(1)?,
        item_id: row.get(2)?,
        ordinal: row.get(3)?,
        state: row.get(4)?,
        input_fingerprint: row.get(5)?,
        target_ref: row.get(6)?,
        child_task_kind: row.get(7)?,
        child_task_id: row.get(8)?,
        effect_state: row.get(9)?,
        error_code: row.get(10)?,
        revision: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

#[allow(clippy::too_many_arguments)]
pub fn update_item(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    item_id: &str,
    state: &str,
    effect_state: &str,
    error_code: Option<&str>,
    now: i64,
) -> AppResult<OperationItemRecord> {
    let changed = conn.execute(
        "UPDATE operation_items
            SET state=?1,effect_state=?2,error_code=?3,revision=revision+1,updated_at=?4
          WHERE operation_id=?5 AND step_id=?6 AND item_id=?7",
        rusqlite::params![
            state,
            effect_state,
            error_code,
            now,
            operation_id,
            step_id,
            item_id,
        ],
    )?;
    if changed != 1 {
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::ReferenceExpired,
        ));
    }
    operation_item(conn, operation_id, step_id, item_id)?
        .ok_or_else(|| AppError::internal("操作项目更新后无法读取"))
}

/// Resolve open items and append the matching receipt in the caller's
/// transaction. Cancellation cannot erase an effect that may already have
/// escaped the database; such items remain explicitly unknown.
pub fn resolve_open_items(
    conn: &Connection,
    operation_id: &str,
    step_id: Option<&str>,
    requested_state: &str,
    event: &str,
    error_code: Option<&str>,
    now: i64,
) -> AppResult<usize> {
    if !matches!(requested_state, "cancelled" | "outcome_unknown") {
        return Err(AppError::invalid_arg("非法未完成操作项目终态"));
    }
    let open_items = {
        let mut statement = conn.prepare(
            "SELECT step_id,item_id,effect_state
               FROM operation_items
              WHERE operation_id=?1
                AND (?2 IS NULL OR step_id=?2)
                AND state IN ('pending','running')
              ORDER BY step_id,ordinal",
        )?;
        let rows = statement.query_map(rusqlite::params![operation_id, step_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (item_step_id, item_id, prior_effect_state) in &open_items {
        let (state, effect_state, item_error) =
            if requested_state == "cancelled" && prior_effect_state == "none" {
                ("cancelled", "none", None)
            } else {
                (
                    "outcome_unknown",
                    "unknown",
                    error_code.or(Some("OUTCOME_UNKNOWN")),
                )
            };
        let item = update_item(
            conn,
            operation_id,
            item_step_id,
            item_id,
            state,
            effect_state,
            item_error,
            now,
        )?;
        insert_receipt(
            conn,
            &NewOperationReceipt {
                receipt_id: format!(
                    "system:item:{}:{}:{}:r{}",
                    operation_id, item_step_id, item_id, item.revision
                ),
                operation_id: operation_id.to_string(),
                step_id: item_step_id.clone(),
                item_id: Some(item_id.clone()),
                revision: item.revision,
                event: event.to_string(),
                input_fingerprint: item.input_fingerprint,
                target_ref: item.target_ref,
                child_task_kind: item.child_task_kind,
                child_task_id: item.child_task_id,
                db_ref: None,
                effect_state: effect_state.to_string(),
                error_code: item_error.map(str::to_string),
                recorded_at: now,
            },
        )?;
    }
    Ok(open_items.len())
}

pub fn resolve_running_items_unknown(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    event: &str,
    error_code: &str,
    now: i64,
) -> AppResult<usize> {
    let running_items = {
        let mut statement = conn.prepare(
            "SELECT item_id FROM operation_items
              WHERE operation_id=?1 AND step_id=?2 AND state='running'
              ORDER BY ordinal",
        )?;
        let rows = statement.query_map(rusqlite::params![operation_id, step_id], |row| {
            row.get::<_, String>(0)
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for item_id in &running_items {
        let item = update_item(
            conn,
            operation_id,
            step_id,
            item_id,
            "outcome_unknown",
            "unknown",
            Some(error_code),
            now,
        )?;
        insert_receipt(
            conn,
            &NewOperationReceipt {
                receipt_id: format!(
                    "system:item:{}:{}:{}:r{}",
                    operation_id, step_id, item_id, item.revision
                ),
                operation_id: operation_id.to_string(),
                step_id: step_id.to_string(),
                item_id: Some(item_id.clone()),
                revision: item.revision,
                event: event.to_string(),
                input_fingerprint: item.input_fingerprint,
                target_ref: item.target_ref,
                child_task_kind: item.child_task_kind,
                child_task_id: item.child_task_id,
                db_ref: None,
                effect_state: "unknown".to_string(),
                error_code: Some(error_code.to_string()),
                recorded_at: now,
            },
        )?;
    }
    Ok(running_items.len())
}

pub fn insert_receipt(conn: &Connection, receipt: &NewOperationReceipt) -> AppResult<()> {
    let inserted = conn.execute(
        "INSERT OR IGNORE INTO operation_receipts(
            receipt_id,operation_id,step_id,item_id,revision,event,input_fingerprint,
            target_ref,child_task_kind,child_task_id,db_ref,effect_state,error_code,
            recorded_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        rusqlite::params![
            receipt.receipt_id,
            receipt.operation_id,
            receipt.step_id,
            receipt.item_id,
            receipt.revision,
            receipt.event,
            receipt.input_fingerprint,
            receipt.target_ref,
            receipt.child_task_kind,
            receipt.child_task_id,
            receipt.db_ref,
            receipt.effect_state,
            receipt.error_code,
            receipt.recorded_at,
        ],
    )?;
    if inserted == 1 {
        return Ok(());
    }
    let existing = conn.query_row(
        "SELECT receipt_id,operation_id,step_id,item_id,revision,event,input_fingerprint,
                target_ref,child_task_kind,child_task_id,db_ref,effect_state,error_code,
                recorded_at
           FROM operation_receipts WHERE receipt_id=?1",
        [receipt.receipt_id.as_str()],
        map_receipt,
    )?;
    if receipt_matches(&existing, receipt) {
        Ok(())
    } else {
        Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::ResourceConflict,
        ))
    }
}

pub fn receipt_exists(conn: &Connection, receipt_id: &str) -> AppResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM operation_receipts WHERE receipt_id=?1)",
        [receipt_id],
        |row| row.get(0),
    )?)
}

pub fn receipts(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
) -> AppResult<Vec<OperationReceiptRecord>> {
    let mut statement = conn.prepare(
        "SELECT receipt_id,operation_id,step_id,item_id,revision,event,input_fingerprint,
                target_ref,child_task_kind,child_task_id,db_ref,effect_state,error_code,
                recorded_at
           FROM operation_receipts
          WHERE operation_id=?1 AND step_id=?2 ORDER BY revision,recorded_at",
    )?;
    let rows = statement.query_map(rusqlite::params![operation_id, step_id], map_receipt)?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

fn map_receipt(row: &rusqlite::Row<'_>) -> rusqlite::Result<OperationReceiptRecord> {
    Ok(OperationReceiptRecord {
        receipt_id: row.get(0)?,
        operation_id: row.get(1)?,
        step_id: row.get(2)?,
        item_id: row.get(3)?,
        revision: row.get(4)?,
        event: row.get(5)?,
        input_fingerprint: row.get(6)?,
        target_ref: row.get(7)?,
        child_task_kind: row.get(8)?,
        child_task_id: row.get(9)?,
        db_ref: row.get(10)?,
        effect_state: row.get(11)?,
        error_code: row.get(12)?,
        recorded_at: row.get(13)?,
    })
}

fn receipt_matches(existing: &OperationReceiptRecord, wanted: &NewOperationReceipt) -> bool {
    existing.operation_id == wanted.operation_id
        && existing.step_id == wanted.step_id
        && existing.item_id == wanted.item_id
        && existing.revision == wanted.revision
        && existing.event == wanted.event
        && existing.input_fingerprint == wanted.input_fingerprint
        && existing.target_ref == wanted.target_ref
        && existing.child_task_kind == wanted.child_task_kind
        && existing.child_task_id == wanted.child_task_id
        && existing.db_ref == wanted.db_ref
        && existing.effect_state == wanted.effect_state
        && existing.error_code == wanted.error_code
}

pub fn bind_child_task(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    task_kind: &str,
    task_id: &str,
    now: i64,
) -> AppResult<ChildTaskRecord> {
    let transaction = conn.unchecked_transaction()?;
    let binding =
        bind_child_task_in_tx(&transaction, operation_id, step_id, task_kind, task_id, now)?;
    transaction.commit()?;
    Ok(binding)
}

pub fn bind_child_task_in_tx(
    transaction: &Transaction<'_>,
    operation_id: &str,
    step_id: &str,
    task_kind: &str,
    task_id: &str,
    now: i64,
) -> AppResult<ChildTaskRecord> {
    if let Some(existing) = child_task_in_tx(transaction, operation_id, step_id)? {
        if existing.task_kind == task_kind && existing.task_id == task_id {
            return Ok(existing);
        }
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::ResourceConflict,
        ));
    }
    transaction.execute(
        "INSERT INTO operation_child_tasks(
            operation_id,step_id,task_kind,task_id,created_at
         ) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![operation_id, step_id, task_kind, task_id, now],
    )?;
    Ok(ChildTaskRecord {
        operation_id: operation_id.to_string(),
        step_id: step_id.to_string(),
        task_kind: task_kind.to_string(),
        task_id: task_id.to_string(),
        created_at: now,
    })
}

pub fn child_task(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
) -> AppResult<Option<ChildTaskRecord>> {
    Ok(conn
        .query_row(
            "SELECT operation_id,step_id,task_kind,task_id,created_at
               FROM operation_child_tasks
              WHERE operation_id=?1 AND step_id=?2",
            rusqlite::params![operation_id, step_id],
            |row| {
                Ok(ChildTaskRecord {
                    operation_id: row.get(0)?,
                    step_id: row.get(1)?,
                    task_kind: row.get(2)?,
                    task_id: row.get(3)?,
                    created_at: row.get(4)?,
                })
            },
        )
        .optional()?)
}

pub fn operation_owner(conn: &Connection, operation_id: &str) -> AppResult<Option<OperationOwner>> {
    Ok(conn
        .query_row(
            "SELECT o.principal_id,p.session_id,p.instance_id,o.library_id,o.library_epoch
               FROM operations o
               JOIN plans p ON p.plan_id=o.plan_id
              WHERE o.operation_id=?1",
            [operation_id],
            |row| {
                Ok(OperationOwner {
                    principal_id: row.get(0)?,
                    session_id: row.get(1)?,
                    instance_id: row.get(2)?,
                    library_id: row.get(3)?,
                    library_epoch: row.get(4)?,
                })
            },
        )
        .optional()?)
}

pub fn claim_next(conn: &Connection, now: i64) -> AppResult<Option<String>> {
    claim_next_for_capability(conn, now, None)
}

pub fn claim_next_for_capability(
    conn: &Connection,
    now: i64,
    capability: Option<&str>,
) -> AppResult<Option<String>> {
    let transaction = conn.unchecked_transaction()?;
    let operation: Option<(String, String)> = transaction
        .query_row(
            "SELECT o.operation_id,o.capability
               FROM operations o
              WHERE o.state='queued' AND o.phase='ready'
                AND o.capability != 'tagging.submit'
                AND EXISTS (
                    SELECT 1 FROM operation_steps s
                     WHERE s.operation_id=o.operation_id AND s.state='queued'
                )
              ORDER BY o.created_at,o.operation_id
              LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((operation_id, operation_capability)) = operation else {
        transaction.commit()?;
        return Ok(None);
    };
    if capability.is_some_and(|expected| operation_capability != expected) {
        transaction.commit()?;
        return Ok(None);
    }
    let changed = transaction.execute(
        "UPDATE operations
            SET state='running',phase='executing',revision=revision+1,updated_at=?1
          WHERE operation_id=?2 AND state='queued' AND phase='ready'",
        rusqlite::params![now, operation_id],
    )?;
    if changed != 1 {
        transaction.commit()?;
        return Ok(None);
    }
    transaction.execute(
        "UPDATE operation_steps
            SET state='running',phase='executing',started_at=COALESCE(started_at,?1),
                revision=revision+1,updated_at=?1
          WHERE operation_id=?2 AND state='queued'
            AND ordinal=(SELECT MIN(ordinal) FROM operation_steps
                          WHERE operation_id=?2 AND state='queued')",
        rusqlite::params![now, operation_id],
    )?;
    transaction.commit()?;
    Ok(Some(operation_id))
}

pub fn is_queued_for_dispatch(
    conn: &Connection,
    operation_id: &str,
    capability: &str,
) -> AppResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM operations o
             WHERE o.operation_id=?1 AND o.capability=?2
               AND o.state='queued' AND o.phase='ready'
               AND EXISTS (SELECT 1 FROM operation_steps s
                            WHERE s.operation_id=o.operation_id
                              AND s.state='queued')
               AND o.operation_id=(
                    SELECT queued.operation_id FROM operations queued
                     WHERE queued.state='queued' AND queued.phase='ready'
                       AND (queued.capability != 'tagging.submit'
                            OR queued.operation_id=?1)
                       AND EXISTS (SELECT 1 FROM operation_steps s
                                    WHERE s.operation_id=queued.operation_id
                                      AND s.state='queued')
                     ORDER BY queued.created_at,queued.operation_id LIMIT 1
               )
        )",
        rusqlite::params![operation_id, capability],
        |row| row.get(0),
    )?)
}

pub fn request_cancel(conn: &Connection, operation_id: &str, now: i64) -> AppResult<bool> {
    let transaction = conn.unchecked_transaction()?;
    let changed = transaction.execute(
        "UPDATE operations
            SET phase='cancelling',revision=revision+1,updated_at=?1
          WHERE operation_id=?2 AND state IN ('queued','running')
            AND phase != 'cancelling'",
        rusqlite::params![now, operation_id],
    )?;
    if changed == 1 {
        transaction.execute(
            "UPDATE operation_steps
                SET phase='cancelling',revision=revision+1,updated_at=?1
              WHERE operation_id=?2 AND state IN ('queued','running')
                AND phase != 'cancelling'",
            rusqlite::params![now, operation_id],
        )?;
        // Tagging sends are a persistent side effect. Close their durable
        // authorization gate in the same transaction as the operation cancel
        // request so a racing worker cannot spend another request slot.
        transaction.execute(
            "UPDATE tagging_batch_links SET state='cancelled',updated_at=?1
              WHERE operation_id=?2
                AND state IN ('awaiting_user_start','started','interrupted')",
            rusqlite::params![now, operation_id],
        )?;
    }
    transaction.commit()?;
    Ok(changed == 1)
}

pub fn finish_cancel(conn: &Connection, operation_id: &str, now: i64) -> AppResult<()> {
    let transaction = conn.unchecked_transaction()?;
    resolve_open_items(
        &transaction,
        operation_id,
        None,
        "cancelled",
        "item_cancelled",
        None,
        now,
    )?;
    let has_unknown_items: bool = transaction.query_row(
        "SELECT EXISTS(SELECT 1 FROM operation_items
                       WHERE operation_id=?1 AND state='outcome_unknown')",
        [operation_id],
        |row| row.get(0),
    )?;
    let terminal_state = if has_unknown_items {
        "outcome_unknown"
    } else {
        "cancelled"
    };
    transaction.execute(
        "UPDATE operation_steps
            SET state=CASE WHEN EXISTS(
                    SELECT 1 FROM operation_items i
                     WHERE i.operation_id=operation_steps.operation_id
                       AND i.step_id=operation_steps.step_id
                       AND i.state='outcome_unknown'
                ) THEN 'outcome_unknown' ELSE 'cancelled' END,
                phase='complete',finished_at=?1,
                revision=revision+1,updated_at=?1
          WHERE operation_id=?2 AND state IN ('queued','running')",
        rusqlite::params![now, operation_id],
    )?;
    transaction.execute(
        "UPDATE operations
            SET state=?2,phase='complete',finished_at=?1,
                revision=revision+1,updated_at=?1
          WHERE operation_id=?3 AND state IN ('queued','running')",
        rusqlite::params![now, terminal_state, operation_id],
    )?;
    transaction.execute(
        "UPDATE ai_batches SET status='cancelled'
          WHERE id IN (
              SELECT batch_id FROM tagging_batch_links
               WHERE operation_id=?1 AND state='cancelled'
          )",
        [operation_id],
    )?;
    transaction.commit()?;
    Ok(())
}

pub fn refresh_progress(conn: &Connection, operation_id: &str, now: i64) -> AppResult<()> {
    let (total, completed, failed): (Option<i64>, i64, i64) = conn.query_row(
        "SELECT COALESCE(
                    (SELECT SUM(total) FROM operation_steps WHERE operation_id=?1),
                    (SELECT total FROM operations WHERE operation_id=?1)
                ),
                COALESCE(SUM(CASE WHEN state='succeeded' THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state IN ('failed','outcome_unknown') THEN 1 ELSE 0 END),0)
           FROM operation_items WHERE operation_id=?1",
        [operation_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    conn.execute(
        "UPDATE operations
            SET total=?1,completed=?2,failed=?3,revision=revision+1,updated_at=?4
          WHERE operation_id=?5",
        rusqlite::params![total, completed, failed, now, operation_id],
    )?;
    Ok(())
}

pub fn complete_operation(
    conn: &Connection,
    operation_id: &str,
    state: &str,
    now: i64,
) -> AppResult<()> {
    let transaction = conn.unchecked_transaction()?;
    transaction.execute(
        "UPDATE operation_steps
            SET state=?1,phase='complete',finished_at=?2,
                revision=revision+1,updated_at=?2
          WHERE operation_id=?3 AND state NOT IN ('succeeded','partially_succeeded','failed','cancelled')",
        rusqlite::params![state, now, operation_id],
    )?;
    transaction.execute(
        "UPDATE operations
            SET state=?1,phase='complete',finished_at=?2,
                revision=revision+1,updated_at=?2
          WHERE operation_id=?3",
        rusqlite::params![state, now, operation_id],
    )?;
    transaction.commit()?;
    Ok(())
}

/// A3-03：在调用方已持有的事务里结算操作与步骤。
///
/// `complete_operation` 会自行开启事务，不能用于计划提交的 effect 闭包；
/// 这里只执行同样的两条 UPDATE，由外层事务保证原子性。
pub fn settle_operation(
    conn: &Connection,
    operation_id: &str,
    state: &str,
    now: i64,
) -> AppResult<()> {
    conn.execute(
        "UPDATE operation_steps
            SET state=?1,phase='complete',finished_at=?2,
                revision=revision+1,updated_at=?2
          WHERE operation_id=?3 AND state NOT IN ('succeeded','partially_succeeded','failed','cancelled')",
        rusqlite::params![state, now, operation_id],
    )?;
    conn.execute(
        "UPDATE operations
            SET state=?1,phase='complete',finished_at=?2,
                revision=revision+1,updated_at=?2
          WHERE operation_id=?3",
        rusqlite::params![state, now, operation_id],
    )?;
    Ok(())
}

/// Stop a running operation after its authorization generation changes while
/// preserving it as an explicitly resumable interruption. This is scoped to
/// one operation so an unrelated worker is not interrupted with it.
pub fn interrupt_for_reauthorization(
    conn: &Connection,
    operation_id: &str,
    now: i64,
) -> AppResult<bool> {
    let transaction = conn.unchecked_transaction()?;
    let changed = transaction.execute(
        "UPDATE operations
            SET state='interrupted',phase='reconciling',revision=revision+1,updated_at=?1
          WHERE operation_id=?2 AND state IN ('queued','running') AND phase != 'complete'",
        rusqlite::params![now, operation_id],
    )?;
    if changed == 0 {
        transaction.commit()?;
        return Ok(false);
    }
    transaction.execute(
        "UPDATE operation_steps
            SET state='interrupted',phase='reconciling',revision=revision+1,updated_at=?1
          WHERE operation_id=?2 AND state IN ('queued','running')",
        rusqlite::params![now, operation_id],
    )?;
    let (pending_steps, running_steps, unknown_items): (i64, i64, i64) = transaction.query_row(
        "SELECT
                (SELECT COUNT(*) FROM operation_steps
                  WHERE operation_id=?1 AND state='interrupted'),
                0,
                (SELECT COUNT(*) FROM operation_items
                  WHERE operation_id=?1 AND effect_state='unknown')",
        [operation_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    transaction.execute(
        "INSERT INTO operation_recovery(
            operation_id,pending_steps,running_steps,unknown_items,
            requires_resume,created_at,resolved_at
         ) VALUES(?1,?2,?3,?4,1,?5,NULL)
         ON CONFLICT(operation_id) DO UPDATE SET
            pending_steps=excluded.pending_steps,
            running_steps=excluded.running_steps,
            unknown_items=excluded.unknown_items,
            requires_resume=1,
            created_at=excluded.created_at,
            resolved_at=NULL",
        rusqlite::params![
            operation_id,
            pending_steps,
            running_steps,
            unknown_items,
            now
        ],
    )?;
    transaction.commit()?;
    Ok(true)
}

pub fn reconcile_startup(conn: &Connection, now: i64) -> AppResult<Vec<RecoverySummary>> {
    let transaction = conn.unchecked_transaction()?;
    // A cancellation is durable before the worker necessarily reaches its
    // finish callback. Preserve that accepted decision across a crash instead
    // of turning the operation into a resumable interruption while leaving
    // its child tagging link cancelled.
    let cancelled_tagging_operations = {
        let mut statement = transaction.prepare(
            "SELECT DISTINCT operation_id FROM tagging_batch_links WHERE state='cancelled'",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for operation_id in &cancelled_tagging_operations {
        resolve_open_items(
            &transaction,
            operation_id,
            None,
            "cancelled",
            "item_cancelled_during_recovery",
            None,
            now,
        )?;
    }
    transaction.execute(
        "UPDATE operation_steps
            SET state=CASE WHEN EXISTS(
                    SELECT 1 FROM operation_items i
                     WHERE i.operation_id=operation_steps.operation_id
                       AND i.step_id=operation_steps.step_id
                       AND i.state='outcome_unknown'
                ) THEN 'outcome_unknown' ELSE 'cancelled' END,
                phase='complete',finished_at=COALESCE(finished_at,?1),
                revision=revision+1,updated_at=?1
          WHERE state IN ('queued','running','interrupted')
            AND operation_id IN (
                SELECT operation_id FROM tagging_batch_links WHERE state='cancelled'
            )",
        [now],
    )?;
    transaction.execute(
        "UPDATE operations
            SET state=CASE WHEN EXISTS(
                    SELECT 1 FROM operation_items i
                     WHERE i.operation_id=operations.operation_id
                       AND i.state='outcome_unknown'
                ) THEN 'outcome_unknown' ELSE 'cancelled' END,
                phase='complete',finished_at=COALESCE(finished_at,?1),
                revision=revision+1,updated_at=?1
          WHERE state IN ('queued','running','interrupted')
            AND operation_id IN (
                SELECT operation_id FROM tagging_batch_links WHERE state='cancelled'
            )",
        [now],
    )?;
    for operation_id in &cancelled_tagging_operations {
        let unknown_items: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM operation_items
              WHERE operation_id=?1 AND state='outcome_unknown'",
            [operation_id],
            |row| row.get(0),
        )?;
        transaction.execute(
            "INSERT INTO operation_recovery(
                operation_id,pending_steps,running_steps,unknown_items,
                requires_resume,created_at,resolved_at
             ) VALUES(?1,0,0,?2,0,?3,?4)
             ON CONFLICT(operation_id) DO UPDATE SET
                pending_steps=0,running_steps=0,
                unknown_items=excluded.unknown_items,
                requires_resume=0,created_at=excluded.created_at,
                resolved_at=excluded.resolved_at",
            rusqlite::params![
                operation_id,
                unknown_items,
                now,
                (unknown_items == 0).then_some(now),
            ],
        )?;
    }
    transaction.execute(
        "UPDATE ai_batches SET status='cancelled'
          WHERE id IN (
              SELECT batch_id FROM tagging_batch_links WHERE state='cancelled'
          )",
        [],
    )?;
    let operation_ids = {
        let mut statement = transaction.prepare(&format!(
            "SELECT operation_id FROM operations
              WHERE state NOT IN ({TERMINAL_STATES},'outcome_unknown')
                AND phase != 'complete'"
        ))?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    let mut summaries = Vec::new();
    for operation_id in operation_ids {
        let (pending_steps, running_steps, unknown_items): (i64, i64, i64) = transaction
            .query_row(
                "SELECT
                    (SELECT COUNT(*) FROM operation_steps
                      WHERE operation_id=?1 AND state IN ('queued','running')),
                    (SELECT COUNT(*) FROM operation_steps
                      WHERE operation_id=?1 AND state='running'),
                    (SELECT COUNT(*) FROM operation_items
                      WHERE operation_id=?1 AND effect_state='unknown')",
                [operation_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )?;
        transaction.execute(
            "UPDATE operations
                SET state='interrupted',phase='reconciling',
                    revision=revision+1,updated_at=?1
              WHERE operation_id=?2 AND phase != 'complete'",
            rusqlite::params![now, operation_id],
        )?;
        transaction.execute(
            "UPDATE operation_steps
                SET state='interrupted',phase='reconciling',
                    revision=revision+1,updated_at=?1
              WHERE operation_id=?2 AND state IN ('queued','running')",
            rusqlite::params![now, operation_id],
        )?;
        transaction.execute(
            "INSERT INTO operation_recovery(
                operation_id,pending_steps,running_steps,unknown_items,
                requires_resume,created_at,resolved_at
             ) VALUES(?1,?2,?3,?4,1,?5,NULL)
             ON CONFLICT(operation_id) DO UPDATE SET
                pending_steps=excluded.pending_steps,
                running_steps=excluded.running_steps,
                unknown_items=excluded.unknown_items,
                requires_resume=1,
                created_at=excluded.created_at,
                resolved_at=NULL",
            rusqlite::params![
                operation_id,
                pending_steps,
                running_steps,
                unknown_items,
                now,
            ],
        )?;
        summaries.push(RecoverySummary {
            operation_id,
            pending_steps,
            running_steps,
            unknown_items,
            requires_resume: true,
            created_at: now,
        });
    }
    transaction.commit()?;
    Ok(summaries)
}

pub struct ResumeOwner<'a> {
    pub principal_id: &'a str,
    pub session_id: Option<&'a str>,
    pub instance_id: &'a str,
    pub grant_epoch: &'a str,
}

pub fn mark_resumed(
    conn: &Connection,
    operation_id: &str,
    owner: &ResumeOwner<'_>,
    now: i64,
) -> AppResult<()> {
    let transaction = conn.unchecked_transaction()?;
    mark_resumed_in_transaction(&transaction, operation_id, owner, now)?;
    transaction.commit()?;
    Ok(())
}

pub fn mark_resumed_in_transaction(
    transaction: &Transaction<'_>,
    operation_id: &str,
    owner: &ResumeOwner<'_>,
    now: i64,
) -> AppResult<()> {
    let changed = transaction.execute(
        "UPDATE operations
            SET state='queued',phase='ready',revision=revision+1,updated_at=?1
          WHERE operation_id=?2 AND state='interrupted' AND phase='reconciling'",
        rusqlite::params![now, operation_id],
    )?;
    if changed != 1 {
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::ResourceConflict,
        ));
    }
    let rebound = transaction.execute(
        "UPDATE plans SET session_id=?1,instance_id=?2,grant_epoch=?3
          WHERE plan_id=(SELECT plan_id FROM operations WHERE operation_id=?4)
            AND principal_id=?5",
        rusqlite::params![
            owner.session_id,
            owner.instance_id,
            owner.grant_epoch,
            operation_id,
            owner.principal_id
        ],
    )?;
    if rebound != 1 {
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::Unauthorized,
        ));
    }
    transaction.execute(
        "UPDATE operation_steps
            SET state='queued',phase='ready',revision=revision+1,updated_at=?1
          WHERE operation_id=?2 AND state='interrupted'",
        rusqlite::params![now, operation_id],
    )?;
    transaction.execute(
        "UPDATE operation_recovery SET resolved_at=?1 WHERE operation_id=?2",
        rusqlite::params![now, operation_id],
    )?;
    Ok(())
}

pub fn recovery_summaries(conn: &Connection) -> AppResult<Vec<RecoverySummary>> {
    let mut statement = conn.prepare(
        "SELECT operation_id,pending_steps,running_steps,unknown_items,
                requires_resume,created_at
           FROM operation_recovery
          WHERE resolved_at IS NULL ORDER BY created_at DESC",
    )?;
    let rows = statement.query_map([], |row| {
        Ok(RecoverySummary {
            operation_id: row.get(0)?,
            pending_steps: row.get(1)?,
            running_steps: row.get(2)?,
            unknown_items: row.get(3)?,
            requires_resume: row.get::<_, i64>(4)? != 0,
            created_at: row.get(5)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// List interrupted operations for a domain reconciler without exposing SQL
/// joins to service modules.
pub fn interrupted_operation_ids_for_capability(
    conn: &Connection,
    capability: &str,
) -> AppResult<Vec<String>> {
    let mut statement = conn.prepare(
        "SELECT operation_id FROM operations
          WHERE capability=?1 AND state='interrupted'
          ORDER BY updated_at,operation_id",
    )?;
    let rows = statement.query_map([capability], |row| row.get(0))?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Refresh the unknown count after a domain reconciler has inspected its
/// durable artifacts. Older tagging batches may have a sent request but no
/// per-item rows; keep that unresolved request visible as one unknown item.
pub fn refresh_recovery_unknown_items(conn: &Connection, operation_id: &str) -> AppResult<()> {
    conn.execute(
        "UPDATE operation_recovery
            SET unknown_items=(
                SELECT COUNT(*) FROM operation_items
                 WHERE operation_id=?1
                   AND (state='outcome_unknown' OR effect_state='unknown')
            ) + (
                SELECT COUNT(*) FROM tagging_batch_links l
                 WHERE l.operation_id=?1 AND l.requests_attempted>0
                   AND NOT EXISTS (
                       SELECT 1 FROM operation_items i
                        WHERE i.operation_id=l.operation_id AND i.step_id=l.step_id
                   )
            )
          WHERE operation_id=?1 AND resolved_at IS NULL",
        [operation_id],
    )?;
    Ok(())
}

fn child_task_in_tx(
    transaction: &Transaction<'_>,
    operation_id: &str,
    step_id: &str,
) -> AppResult<Option<ChildTaskRecord>> {
    Ok(transaction
        .query_row(
            "SELECT operation_id,step_id,task_kind,task_id,created_at
               FROM operation_child_tasks
              WHERE operation_id=?1 AND step_id=?2",
            rusqlite::params![operation_id, step_id],
            |row| {
                Ok(ChildTaskRecord {
                    operation_id: row.get(0)?,
                    step_id: row.get(1)?,
                    task_kind: row.get(2)?,
                    task_id: row.get(3)?,
                    created_at: row.get(4)?,
                })
            },
        )
        .optional()?)
}

fn ensure_operation_column(conn: &Connection, name: &str, definition: &str) -> AppResult<()> {
    let columns: Vec<String> = {
        let mut statement = conn.prepare("PRAGMA table_info(operations)")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    if !columns.iter().any(|column| column == name) {
        conn.execute(
            &format!("ALTER TABLE operations ADD COLUMN {name} {definition}"),
            [],
        )?;
    }
    Ok(())
}
