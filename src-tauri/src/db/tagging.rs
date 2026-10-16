//! SQL persistence for approved AI tagging child tasks and request budgets.

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaggingBatchLink {
    pub operation_id: String,
    pub step_id: String,
    pub plan_id: String,
    pub batch_id: i64,
    pub source_operation_id: Option<String>,
    pub source_step_id: Option<String>,
    pub source_fingerprint: String,
    pub config_hash: String,
    pub request_limit: i64,
    pub requests_attempted: i64,
    pub deadline_at: Option<i64>,
    pub state: String,
}

pub fn operation_item_id(asset_id: i64) -> String {
    format!("asset:{asset_id}")
}

pub fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tagging_batch_links (
            operation_id TEXT NOT NULL,
            step_id TEXT NOT NULL,
            plan_id TEXT NOT NULL REFERENCES plans(plan_id) ON DELETE CASCADE,
            batch_id INTEGER NOT NULL REFERENCES ai_batches(id) ON DELETE CASCADE,
            source_operation_id TEXT,
            source_step_id TEXT,
            source_fingerprint TEXT NOT NULL,
            config_hash TEXT NOT NULL,
            request_limit INTEGER NOT NULL CHECK(request_limit >= 0),
            requests_attempted INTEGER NOT NULL DEFAULT 0 CHECK(requests_attempted >= 0),
            deadline_at INTEGER,
            state TEXT NOT NULL CHECK(state IN (
                'awaiting_user_start','started','interrupted','completed','cancelled'
            )),
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(operation_id,step_id),
            UNIQUE(source_operation_id,source_step_id),
            CHECK((source_operation_id IS NULL) = (source_step_id IS NULL)),
            FOREIGN KEY(operation_id,step_id)
                REFERENCES operation_steps(operation_id,step_id) ON DELETE CASCADE
        );
        CREATE INDEX IF NOT EXISTS idx_tagging_batch_links_source
            ON tagging_batch_links(source_operation_id,source_step_id);
        CREATE INDEX IF NOT EXISTS idx_tagging_batch_links_batch
            ON tagging_batch_links(batch_id);",
    )?;
    ensure_deadline_column(conn)?;
    upgrade_tagging_batch_links(conn)?;
    Ok(())
}

fn ensure_deadline_column(conn: &Connection) -> AppResult<()> {
    let mut statement = conn.prepare("PRAGMA table_info(tagging_batch_links)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    if !columns.iter().any(|name| name == "deadline_at") {
        conn.execute_batch("ALTER TABLE tagging_batch_links ADD COLUMN deadline_at INTEGER")?;
    }
    Ok(())
}

/// A batch may be run through several separately approved scopes. Earlier
/// development builds enforced one link per batch; rebuild that table without
/// the obsolete uniqueness constraint while preserving its durable state.
fn upgrade_tagging_batch_links(conn: &Connection) -> AppResult<()> {
    let mut statement = conn.prepare("PRAGMA index_list('tagging_batch_links')")?;
    let indexes = statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, bool>(2)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    drop(statement);
    let mut has_unique_batch = false;
    for (name, unique) in indexes {
        if !unique {
            continue;
        }
        let quoted = name.replace('\'', "''");
        let mut statement = conn.prepare(&format!("PRAGMA index_info('{quoted}')"))?;
        let columns = statement
            .query_map([], |row| row.get::<_, String>(2))?
            .collect::<Result<Vec<_>, _>>()?;
        if columns.len() == 1 && columns[0] == "batch_id" {
            has_unique_batch = true;
            break;
        }
    }
    let schema: Option<String> = conn
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type='table' AND name='tagging_batch_links'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let missing_cancelled_state = schema
        .as_deref()
        .is_some_and(|sql| !sql.contains("'cancelled'"));
    if !has_unique_batch && !missing_cancelled_state {
        return Ok(());
    }
    let tx = conn.unchecked_transaction()?;
    tx.execute_batch(
        "DROP INDEX IF EXISTS idx_tagging_batch_links_source;
         DROP INDEX IF EXISTS idx_tagging_batch_links_batch;
         ALTER TABLE tagging_batch_links RENAME TO tagging_batch_links_previous;
         CREATE TABLE tagging_batch_links (
            operation_id TEXT NOT NULL,
            step_id TEXT NOT NULL,
            plan_id TEXT NOT NULL REFERENCES plans(plan_id) ON DELETE CASCADE,
            batch_id INTEGER NOT NULL REFERENCES ai_batches(id) ON DELETE CASCADE,
            source_operation_id TEXT,
            source_step_id TEXT,
            source_fingerprint TEXT NOT NULL,
            config_hash TEXT NOT NULL,
            request_limit INTEGER NOT NULL CHECK(request_limit >= 0),
            requests_attempted INTEGER NOT NULL DEFAULT 0 CHECK(requests_attempted >= 0),
            deadline_at INTEGER,
            state TEXT NOT NULL CHECK(state IN (
                'awaiting_user_start','started','interrupted','completed','cancelled'
            )),
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(operation_id,step_id),
            UNIQUE(source_operation_id,source_step_id),
            CHECK((source_operation_id IS NULL) = (source_step_id IS NULL)),
            FOREIGN KEY(operation_id,step_id)
                REFERENCES operation_steps(operation_id,step_id) ON DELETE CASCADE
         );
         INSERT INTO tagging_batch_links(
            operation_id,step_id,plan_id,batch_id,source_operation_id,source_step_id,
            source_fingerprint,config_hash,request_limit,requests_attempted,deadline_at,
            state,created_at,updated_at
         ) SELECT operation_id,step_id,plan_id,batch_id,source_operation_id,source_step_id,
                   source_fingerprint,config_hash,request_limit,requests_attempted,deadline_at,
                   state,created_at,updated_at
             FROM tagging_batch_links_previous;
         DROP TABLE tagging_batch_links_previous;
         CREATE INDEX idx_tagging_batch_links_source
            ON tagging_batch_links(source_operation_id,source_step_id);
         CREATE INDEX idx_tagging_batch_links_batch
            ON tagging_batch_links(batch_id);",
    )?;
    tx.commit()?;
    Ok(())
}

pub fn insert_link_in_tx(
    tx: &Transaction<'_>,
    link: &TaggingBatchLink,
    now: i64,
) -> AppResult<TaggingBatchLink> {
    if let Some(existing) = by_operation(tx, &link.operation_id, &link.step_id)? {
        return if same_identity(&existing, link) {
            Ok(existing)
        } else {
            Err(AppError::from(
                crate::contracts::agent::AgentErrorCode::ResourceConflict,
            ))
        };
    }
    tx.execute(
        "INSERT INTO tagging_batch_links(
            operation_id,step_id,plan_id,batch_id,source_operation_id,source_step_id,
            source_fingerprint,config_hash,request_limit,requests_attempted,deadline_at,state,
            created_at,updated_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,0,NULL,'awaiting_user_start',?10,?10)",
        rusqlite::params![
            link.operation_id,
            link.step_id,
            link.plan_id,
            link.batch_id,
            link.source_operation_id,
            link.source_step_id,
            link.source_fingerprint,
            link.config_hash,
            link.request_limit,
            now,
        ],
    )?;
    by_operation(tx, &link.operation_id, &link.step_id)?
        .ok_or_else(|| AppError::internal("打标操作映射写入后无法读取"))
}

pub fn by_operation(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
) -> AppResult<Option<TaggingBatchLink>> {
    query_link(
        conn,
        "WHERE operation_id=?1 AND step_id=?2",
        rusqlite::params![operation_id, step_id],
    )
}

pub fn by_batch(conn: &Connection, batch_id: i64) -> AppResult<Option<TaggingBatchLink>> {
    query_link(
        conn,
        "WHERE batch_id=?1 ORDER BY updated_at DESC,rowid DESC LIMIT 1",
        [batch_id],
    )
}

pub fn by_batch_and_fingerprint(
    conn: &Connection,
    batch_id: i64,
    fingerprint: &str,
) -> AppResult<Option<TaggingBatchLink>> {
    query_link(
        conn,
        "WHERE batch_id=?1 AND source_fingerprint=?2 ORDER BY rowid DESC LIMIT 1",
        rusqlite::params![batch_id, fingerprint],
    )
}

pub fn by_source(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
) -> AppResult<Option<TaggingBatchLink>> {
    query_link(
        conn,
        "WHERE source_operation_id=?1 AND source_step_id=?2",
        rusqlite::params![operation_id, step_id],
    )
}

fn query_link<P: rusqlite::Params>(
    conn: &Connection,
    where_clause: &str,
    params: P,
) -> AppResult<Option<TaggingBatchLink>> {
    let sql = format!(
        "SELECT operation_id,step_id,plan_id,batch_id,source_operation_id,source_step_id,
                source_fingerprint,config_hash,request_limit,requests_attempted,deadline_at,state
           FROM tagging_batch_links {where_clause}"
    );
    Ok(conn
        .query_row(&sql, params, |row| {
            Ok(TaggingBatchLink {
                operation_id: row.get(0)?,
                step_id: row.get(1)?,
                plan_id: row.get(2)?,
                batch_id: row.get(3)?,
                source_operation_id: row.get(4)?,
                source_step_id: row.get(5)?,
                source_fingerprint: row.get(6)?,
                config_hash: row.get(7)?,
                request_limit: row.get(8)?,
                requests_attempted: row.get(9)?,
                deadline_at: row.get(10)?,
                state: row.get(11)?,
            })
        })
        .optional()?)
}

fn same_identity(existing: &TaggingBatchLink, wanted: &TaggingBatchLink) -> bool {
    existing.plan_id == wanted.plan_id
        && existing.batch_id == wanted.batch_id
        && existing.source_operation_id == wanted.source_operation_id
        && existing.source_step_id == wanted.source_step_id
        && existing.source_fingerprint == wanted.source_fingerprint
        && existing.config_hash == wanted.config_hash
        && existing.request_limit == wanted.request_limit
}

pub fn begin(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    now: i64,
    max_seconds: i64,
) -> AppResult<bool> {
    let tx = conn.unchecked_transaction()?;
    let changed = begin_in_tx(&tx, operation_id, step_id, now, max_seconds)?;
    tx.commit()?;
    Ok(changed)
}

/// Begin a tagging step in a caller-owned transaction so snapshot validation
/// and the transition to `started` can be committed atomically.
pub fn begin_in_tx(
    tx: &Transaction<'_>,
    operation_id: &str,
    step_id: &str,
    now: i64,
    max_seconds: i64,
) -> AppResult<bool> {
    let changed = tx.execute(
        "UPDATE tagging_batch_links
            SET state='started',deadline_at=COALESCE(
                    deadline_at,
                    COALESCE((SELECT started_at FROM operation_steps
                               WHERE operation_id=?3 AND step_id=?4),?1)+?2
                ),updated_at=?1
          WHERE operation_id=?3 AND step_id=?4
            AND state IN ('awaiting_user_start','interrupted')
            AND COALESCE(
                deadline_at,
                COALESCE((SELECT started_at FROM operation_steps
                           WHERE operation_id=?3 AND step_id=?4),?1)+?2
            )>?1",
        rusqlite::params![now, max_seconds, operation_id, step_id],
    )?;
    if changed == 1 {
        let operation_changed = tx.execute(
            "UPDATE operations SET state='running',phase='executing',revision=revision+1,
                    updated_at=?1 WHERE operation_id=?2 AND state IN ('queued','interrupted')",
            rusqlite::params![now, operation_id],
        )?;
        let step_changed = tx.execute(
            "UPDATE operation_steps SET state='running',phase='executing',
                    started_at=COALESCE(started_at,?1),revision=revision+1,updated_at=?1
              WHERE operation_id=?2 AND step_id=?3 AND state IN ('queued','interrupted')",
            rusqlite::params![now, operation_id, step_id],
        )?;
        if operation_changed != 1 || step_changed != 1 {
            return Err(AppError::from(
                crate::contracts::agent::AgentErrorCode::ResourceConflict,
            ));
        }
    }
    Ok(changed == 1)
}

/// Persist the attempt immediately before a network send. A crash after this
/// point conservatively consumes one slot because the provider may have seen it.
pub fn consume_request_attempt(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    asset_id: i64,
    now: i64,
) -> AppResult<bool> {
    let tx = conn.unchecked_transaction()?;
    let changed = tx.execute(
        "UPDATE tagging_batch_links
            SET requests_attempted=requests_attempted+1,updated_at=?1
          WHERE operation_id=?2 AND step_id=?3
            AND state='started' AND requests_attempted<request_limit
            AND deadline_at>?1
            AND EXISTS (
                SELECT 1 FROM operations o
                JOIN operation_steps s ON s.operation_id=o.operation_id
               WHERE o.operation_id=?2 AND s.step_id=?3
                 AND o.state='running' AND o.phase='executing'
                 AND s.state='running' AND s.phase='executing'
            )",
        rusqlite::params![now, operation_id, step_id],
    )?;
    if changed != 1 {
        tx.commit()?;
        return Ok(false);
    }
    let item_id = operation_item_id(asset_id);
    let Some(existing) =
        crate::db::operations::operation_item(&tx, operation_id, step_id, &item_id)?
    else {
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::ReferenceExpired,
        ));
    };
    if !matches!(existing.state.as_str(), "pending" | "running") {
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::ResourceConflict,
        ));
    }
    let item = crate::db::operations::update_item(
        &tx,
        operation_id,
        step_id,
        &item_id,
        "running",
        "unknown",
        None,
        now,
    )?;
    crate::db::operations::insert_receipt(
        &tx,
        &crate::db::operations::NewOperationReceipt {
            receipt_id: format!(
                "system:item:{}:{}:{}:r{}",
                operation_id, step_id, item_id, item.revision
            ),
            operation_id: operation_id.to_string(),
            step_id: step_id.to_string(),
            item_id: Some(item_id),
            revision: item.revision,
            event: "tagging_request_authorized".to_string(),
            input_fingerprint: item.input_fingerprint,
            target_ref: item.target_ref,
            child_task_kind: item.child_task_kind,
            child_task_id: item.child_task_id,
            db_ref: None,
            effect_state: "unknown".to_string(),
            error_code: None,
            recorded_at: now,
        },
    )?;
    tx.commit()?;
    Ok(true)
}

pub fn record_request_outcome(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    asset_id: i64,
    succeeded: bool,
    error_code: Option<&str>,
    now: i64,
) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    let item_id = operation_item_id(asset_id);
    let Some(existing) =
        crate::db::operations::operation_item(&tx, operation_id, step_id, &item_id)?
    else {
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::ReferenceExpired,
        ));
    };
    if existing.state != "running" {
        tx.commit()?;
        return Ok(());
    }
    let (state, effect_state, event, error_code) = if succeeded {
        ("succeeded", "registered", "tagging_suggestion_saved", None)
    } else {
        (
            "outcome_unknown",
            "unknown",
            "tagging_request_outcome_unknown",
            error_code.or(Some("REQUEST_OUTCOME_UNKNOWN")),
        )
    };
    let item = crate::db::operations::update_item(
        &tx,
        operation_id,
        step_id,
        &item_id,
        state,
        effect_state,
        error_code,
        now,
    )?;
    crate::db::operations::insert_receipt(
        &tx,
        &crate::db::operations::NewOperationReceipt {
            receipt_id: format!(
                "system:item:{}:{}:{}:r{}",
                operation_id, step_id, item_id, item.revision
            ),
            operation_id: operation_id.to_string(),
            step_id: step_id.to_string(),
            item_id: Some(item_id),
            revision: item.revision,
            event: event.to_string(),
            input_fingerprint: item.input_fingerprint,
            target_ref: item.target_ref,
            child_task_kind: item.child_task_kind,
            child_task_id: item.child_task_id,
            db_ref: None,
            effect_state: effect_state.to_string(),
            error_code: error_code.map(str::to_string),
            recorded_at: now,
        },
    )?;
    tx.commit()?;
    Ok(())
}

pub fn set_state(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    state: &str,
    now: i64,
) -> AppResult<()> {
    if !matches!(state, "interrupted" | "completed" | "cancelled") {
        return Err(AppError::invalid_arg("非法打标操作状态"));
    }
    conn.execute(
        "UPDATE tagging_batch_links SET state=?1,updated_at=?2
          WHERE operation_id=?3 AND step_id=?4",
        rusqlite::params![state, now, operation_id, step_id],
    )?;
    Ok(())
}

/// Close an inactive approval whose frozen scope can no longer be resumed.
/// A later preparation may create a new operation and require fresh approval.
pub fn cancel_inactive_for_replan(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    now: i64,
) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    let operation_state: String = tx.query_row(
        "SELECT state FROM operations WHERE operation_id=?1",
        [operation_id],
        |row| row.get(0),
    )?;
    if operation_state == "outcome_unknown" {
        let link_changed = tx.execute(
            "UPDATE tagging_batch_links SET state='cancelled',updated_at=?1
              WHERE operation_id=?2 AND step_id=?3 AND state='interrupted'",
            rusqlite::params![now, operation_id, step_id],
        )?;
        if link_changed != 1 {
            return Err(AppError::from(
                crate::contracts::agent::AgentErrorCode::ResourceConflict,
            ));
        }
        // A fresh approval may cover still-pending suggestions, while the old
        // operation keeps its terminal unknown receipt and cannot be replayed.
        tx.execute(
            "UPDATE ai_batches SET status='pending'
              WHERE id=(SELECT batch_id FROM tagging_batch_links
                         WHERE operation_id=?1 AND step_id=?2)",
            rusqlite::params![operation_id, step_id],
        )?;
        tx.execute(
            "UPDATE operation_recovery SET requires_resume=0
              WHERE operation_id=?1 AND resolved_at IS NULL",
            [operation_id],
        )?;
        tx.commit()?;
        return Ok(());
    }
    let link_changed = tx.execute(
        "UPDATE tagging_batch_links SET state='cancelled',updated_at=?1
          WHERE operation_id=?2 AND step_id=?3
            AND state IN ('awaiting_user_start','interrupted')",
        rusqlite::params![now, operation_id, step_id],
    )?;
    let step_changed = tx.execute(
        "UPDATE operation_steps SET state='cancelled',phase='complete',
                finished_at=?1,revision=revision+1,updated_at=?1
          WHERE operation_id=?2 AND step_id=?3 AND state IN ('queued','interrupted')",
        rusqlite::params![now, operation_id, step_id],
    )?;
    let operation_changed = tx.execute(
        "UPDATE operations SET state='cancelled',phase='complete',
                finished_at=?1,revision=revision+1,updated_at=?1
          WHERE operation_id=?2 AND state IN ('queued','interrupted')",
        rusqlite::params![now, operation_id],
    )?;
    if link_changed != 1 || step_changed != 1 || operation_changed != 1 {
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::ResourceConflict,
        ));
    }
    crate::db::operations::resolve_open_items(
        &tx,
        operation_id,
        Some(step_id),
        "cancelled",
        "tagging_item_cancelled_for_replan",
        None,
        now,
    )?;
    let has_unknown_items: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM operation_items
                       WHERE operation_id=?1 AND step_id=?2
                         AND state='outcome_unknown')",
        rusqlite::params![operation_id, step_id],
        |row| row.get(0),
    )?;
    if has_unknown_items {
        tx.execute(
            "UPDATE operation_steps SET state='outcome_unknown',revision=revision+1,
                    updated_at=?1 WHERE operation_id=?2 AND step_id=?3
                      AND state='cancelled'",
            rusqlite::params![now, operation_id, step_id],
        )?;
        tx.execute(
            "UPDATE operations SET state='outcome_unknown',revision=revision+1,
                    updated_at=?1 WHERE operation_id=?2 AND state='cancelled'",
            rusqlite::params![now, operation_id],
        )?;
    }
    tx.execute(
        "UPDATE ai_batches SET status='pending'
          WHERE id=(SELECT batch_id FROM tagging_batch_links
                     WHERE operation_id=?1 AND step_id=?2)",
        rusqlite::params![operation_id, step_id],
    )?;
    if has_unknown_items {
        tx.execute(
            "UPDATE operation_recovery SET unknown_items=(
                    SELECT COUNT(*) FROM operation_items
                     WHERE operation_id=?2 AND state='outcome_unknown'
                ),requires_resume=0,created_at=?1,resolved_at=NULL
              WHERE operation_id=?2",
            rusqlite::params![now, operation_id],
        )?;
    } else {
        tx.execute(
            "UPDATE operation_recovery SET resolved_at=?1,requires_resume=0
              WHERE operation_id=?2 AND resolved_at IS NULL",
            rusqlite::params![now, operation_id],
        )?;
    }
    tx.commit()?;
    Ok(())
}

/// Startup recovery marks operations/steps interrupted first. Bring the child
/// mapping into the same resumable state so a trusted GUI can explicitly resume.
pub fn recover_interrupted(conn: &Connection, now: i64) -> AppResult<usize> {
    let tx = conn.unchecked_transaction()?;
    let interrupted_steps = {
        let mut statement = tx.prepare(
            "SELECT operation_id,step_id,batch_id,requests_attempted FROM tagging_batch_links
              WHERE state='started' AND requests_attempted>0
                AND EXISTS (SELECT 1 FROM operations o
                             WHERE o.operation_id=tagging_batch_links.operation_id
                               AND o.state='interrupted')",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    for (operation_id, step_id, batch_id, requests_attempted) in &interrupted_steps {
        let suggestions = crate::db::ai::list_suggestions(&tx, *batch_id)?;
        let running_items = crate::db::operations::items(&tx, operation_id, step_id)?
            .into_iter()
            .filter(|item| item.state == "running")
            .collect::<Vec<_>>();
        for item in running_items {
            let saved_suggestion_id = item
                .input_fingerprint
                .as_deref()
                .and_then(|fingerprint| fingerprint.strip_prefix("suggestion:"))
                .and_then(|fingerprint| fingerprint.split_once(":asset:"))
                .and_then(|(suggestion_id, fingerprint_asset_id)| {
                    let suggestion_id = suggestion_id.parse::<i64>().ok()?;
                    let asset_id = item
                        .target_ref
                        .as_deref()?
                        .strip_prefix("asset:")?
                        .parse::<i64>()
                        .ok()?;
                    (fingerprint_asset_id.parse::<i64>().ok() == Some(asset_id)
                        && suggestions.iter().any(|suggestion| {
                            suggestion.id == suggestion_id
                                && suggestion.asset_id == asset_id
                                && suggestion.has_analysis
                        }))
                    .then_some(suggestion_id)
                });
            let Some(suggestion_id) = saved_suggestion_id else {
                continue;
            };
            let updated = crate::db::operations::update_item(
                &tx,
                operation_id,
                step_id,
                &item.item_id,
                "succeeded",
                "registered",
                None,
                now,
            )?;
            crate::db::operations::insert_receipt(
                &tx,
                &crate::db::operations::NewOperationReceipt {
                    receipt_id: format!(
                        "system:item:{}:{}:{}:r{}",
                        operation_id, step_id, item.item_id, updated.revision
                    ),
                    operation_id: operation_id.clone(),
                    step_id: step_id.clone(),
                    item_id: Some(item.item_id),
                    revision: updated.revision,
                    event: "tagging_suggestion_saved_reconciled".to_string(),
                    input_fingerprint: updated.input_fingerprint,
                    target_ref: updated.target_ref,
                    child_task_kind: updated.child_task_kind,
                    child_task_id: updated.child_task_id,
                    db_ref: Some(format!("suggestion:{suggestion_id}")),
                    effect_state: "registered".to_string(),
                    error_code: None,
                    recorded_at: now,
                },
            )?;
            crate::db::operations::refresh_progress(&tx, operation_id, now)?;
        }
        crate::db::operations::resolve_running_items_unknown(
            &tx,
            operation_id,
            step_id,
            "tagging_request_outcome_unknown_after_restart",
            "REQUEST_OUTCOME_UNKNOWN",
            now,
        )?;
        reconcile_terminal_tagging_step(
            &tx,
            operation_id,
            step_id,
            *batch_id,
            *requests_attempted,
            now,
        )?;
    }
    let changed = tx.execute(
        "UPDATE tagging_batch_links
            SET state='interrupted',updated_at=?1
          WHERE state='started'
            AND EXISTS (
                SELECT 1 FROM operations o
                 WHERE o.operation_id=tagging_batch_links.operation_id
                   AND o.state='interrupted'
            )
            AND EXISTS (
                SELECT 1 FROM operation_steps s
                 WHERE s.operation_id=tagging_batch_links.operation_id
                   AND s.step_id=tagging_batch_links.step_id
                   AND s.state='interrupted'
            )",
        [now],
    )?;
    tx.execute(
        "UPDATE ai_batches SET status='interrupted'
          WHERE id IN (
              SELECT batch_id FROM tagging_batch_links
               WHERE state='interrupted'
          )
            AND status IN ('pending','processing','interrupted')",
        [],
    )?;
    tx.commit()?;
    Ok(changed)
}

/// Fold a fully reconciled tagging step into its terminal operation state.
/// A saved suggestion is a known database effect and can finish during startup;
/// an unresolved sent request becomes terminal `outcome_unknown` when there is
/// no remaining item that an explicit resume could safely process.
fn reconcile_terminal_tagging_step(
    tx: &Transaction<'_>,
    operation_id: &str,
    step_id: &str,
    batch_id: i64,
    requests_attempted: i64,
    now: i64,
) -> AppResult<()> {
    let (pending, running, succeeded, failed, unknown, total): (i64, i64, i64, i64, i64, i64) = tx
        .query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN state='pending' THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state='running' THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state='succeeded' THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state='failed' THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state='outcome_unknown' THEN 1 ELSE 0 END),0),
                COUNT(*)
               FROM operation_items WHERE operation_id=?1 AND step_id=?2",
            rusqlite::params![operation_id, step_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )?;
    if pending > 0 || running > 0 {
        return Ok(());
    }
    // Older builds only stored a batch-level attempt counter. That evidence is
    // one unknown result; it must never be replayed as though no request ran.
    let legacy_unknown = if total == 0 && requests_attempted > 0 {
        1
    } else {
        0
    };
    let step_state = terminal_tagging_state(succeeded, failed, unknown + legacy_unknown);
    let Some(step_state) = step_state else {
        return Ok(());
    };
    let step_failed = failed + unknown + legacy_unknown;
    let step_changed = tx.execute(
        "UPDATE operation_steps
            SET state=?1,phase='complete',finished_at=?2,completed=?3,failed=?4,
                revision=revision+1,updated_at=?2
          WHERE operation_id=?5 AND step_id=?6 AND state='interrupted'",
        rusqlite::params![
            step_state,
            now,
            succeeded,
            step_failed,
            operation_id,
            step_id
        ],
    )?;
    if step_changed != 1 {
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::ResourceConflict,
        ));
    }
    let unknown_result = step_state == "outcome_unknown";
    tx.execute(
        "UPDATE tagging_batch_links SET state=?1,updated_at=?2
          WHERE operation_id=?3 AND step_id=?4 AND state='started'",
        rusqlite::params![
            if unknown_result {
                "interrupted"
            } else {
                "completed"
            },
            now,
            operation_id,
            step_id
        ],
    )?;
    tx.execute(
        "UPDATE ai_batches SET status=?1 WHERE id=?2
          AND status IN ('pending','processing','interrupted')",
        rusqlite::params![
            if unknown_result {
                "interrupted"
            } else {
                "done"
            },
            batch_id
        ],
    )?;

    let (pending_steps, running_steps): (i64, i64) = tx.query_row(
        "SELECT
            COALESCE(SUM(CASE WHEN state IN ('queued','running','interrupted') THEN 1 ELSE 0 END),0),
            COALESCE(SUM(CASE WHEN state='running' THEN 1 ELSE 0 END),0)
           FROM operation_steps WHERE operation_id=?1",
        [operation_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let (operation_succeeded, operation_failed, operation_unknown): (i64, i64, i64) = tx
        .query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN state='succeeded' THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state='failed' THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state='outcome_unknown' OR effect_state='unknown'
                                  THEN 1 ELSE 0 END),0)
               FROM operation_items WHERE operation_id=?1",
            [operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let legacy_operation_unknown: i64 = tx.query_row(
        "SELECT COUNT(*) FROM tagging_batch_links l
          WHERE l.operation_id=?1 AND l.requests_attempted>0
            AND NOT EXISTS (SELECT 1 FROM operation_items i
                             WHERE i.operation_id=l.operation_id AND i.step_id=l.step_id)",
        [operation_id],
        |row| row.get(0),
    )?;
    let unknown_items = operation_unknown + legacy_operation_unknown;
    if pending_steps == 0 {
        let operation_state =
            terminal_tagging_state(operation_succeeded, operation_failed, unknown_items)
                .unwrap_or(step_state);
        tx.execute(
            "UPDATE operations
                SET state=?1,phase='complete',finished_at=?2,completed=?3,failed=?4,
                    revision=revision+1,updated_at=?2
              WHERE operation_id=?5 AND state='interrupted'",
            rusqlite::params![
                operation_state,
                now,
                operation_succeeded,
                operation_failed + unknown_items,
                operation_id
            ],
        )?;
    }
    tx.execute(
        "INSERT INTO operation_recovery(
            operation_id,pending_steps,running_steps,unknown_items,
            requires_resume,created_at,resolved_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7)
         ON CONFLICT(operation_id) DO UPDATE SET
            pending_steps=excluded.pending_steps,
            running_steps=excluded.running_steps,
            unknown_items=excluded.unknown_items,
            requires_resume=excluded.requires_resume,
            created_at=excluded.created_at,
            resolved_at=excluded.resolved_at",
        rusqlite::params![
            operation_id,
            pending_steps,
            running_steps,
            unknown_items,
            if pending_steps > 0 { 1 } else { 0 },
            now,
            if pending_steps == 0 && unknown_items == 0 {
                Some(now)
            } else {
                None
            }
        ],
    )?;
    Ok(())
}

fn terminal_tagging_state(succeeded: i64, failed: i64, unknown: i64) -> Option<&'static str> {
    if unknown > 0 {
        Some("outcome_unknown")
    } else if failed > 0 && succeeded > 0 {
        Some("partially_succeeded")
    } else if failed > 0 {
        Some("failed")
    } else if succeeded > 0 {
        Some("succeeded")
    } else {
        None
    }
}

pub fn finish_operation_step(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    completed: bool,
    now: i64,
) -> AppResult<()> {
    finish_operation_step_as(
        conn,
        operation_id,
        step_id,
        if completed {
            FinishState::Completed
        } else {
            FinishState::Interrupted
        },
        now,
    )
}

pub fn finish_cancelled_operation_step(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    now: i64,
) -> AppResult<()> {
    finish_operation_step_as(conn, operation_id, step_id, FinishState::Cancelled, now)
}

#[derive(Clone, Copy)]
enum FinishState {
    Completed,
    Interrupted,
    Cancelled,
}

fn finish_operation_step_as(
    conn: &Connection,
    operation_id: &str,
    step_id: &str,
    state: FinishState,
    now: i64,
) -> AppResult<()> {
    let tx = conn.unchecked_transaction()?;
    let cancellation_requested: bool = tx.query_row(
        "SELECT
            EXISTS(SELECT 1 FROM operations
                    WHERE operation_id=?1 AND phase='cancelling')
            OR EXISTS(SELECT 1 FROM tagging_batch_links
                       WHERE operation_id=?1 AND step_id=?2 AND state='cancelled')",
        rusqlite::params![operation_id, step_id],
        |row| row.get(0),
    )?;
    let state = if cancellation_requested && !matches!(state, FinishState::Cancelled) {
        FinishState::Cancelled
    } else {
        state
    };
    let completed_operation_state = if matches!(state, FinishState::Completed) {
        let (succeeded, failed, unknown): (i64, i64, i64) = tx.query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN state='succeeded' THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state='failed' THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state='outcome_unknown' OR effect_state='unknown'
                                  THEN 1 ELSE 0 END),0)
               FROM operation_items WHERE operation_id=?1",
            [operation_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let legacy_unknown: i64 = tx.query_row(
            "SELECT COUNT(*) FROM tagging_batch_links l
              WHERE l.operation_id=?1 AND l.requests_attempted>0
                AND NOT EXISTS (SELECT 1 FROM operation_items i
                                 WHERE i.operation_id=l.operation_id AND i.step_id=l.step_id)",
            [operation_id],
            |row| row.get(0),
        )?;
        terminal_tagging_state(succeeded, failed, unknown + legacy_unknown)
            .unwrap_or("succeeded")
            .to_string()
    } else {
        "succeeded".to_string()
    };
    let (link_state, mut operation_state, phase, finished_at) = match state {
        FinishState::Completed => (
            "completed",
            completed_operation_state,
            "complete",
            Some(now),
        ),
        FinishState::Interrupted => (
            "interrupted",
            "interrupted".to_string(),
            "reconciling",
            None,
        ),
        FinishState::Cancelled => ("cancelled", "cancelled".to_string(), "complete", Some(now)),
    };
    if matches!(state, FinishState::Cancelled) {
        crate::db::operations::resolve_open_items(
            &tx,
            operation_id,
            Some(step_id),
            "cancelled",
            "tagging_item_cancelled",
            None,
            now,
        )?;
        let has_unknown_items: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM operation_items
                           WHERE operation_id=?1 AND step_id=?2
                             AND state='outcome_unknown')",
            rusqlite::params![operation_id, step_id],
            |row| row.get(0),
        )?;
        if has_unknown_items {
            operation_state = "outcome_unknown".to_string();
        }
    }
    let link_changed = if matches!(state, FinishState::Cancelled) {
        tx.execute(
            "UPDATE tagging_batch_links SET state=?1,updated_at=?2
              WHERE operation_id=?3 AND step_id=?4
                AND state IN ('started','cancelled')",
            rusqlite::params![link_state, now, operation_id, step_id],
        )?
    } else {
        tx.execute(
            "UPDATE tagging_batch_links SET state=?1,updated_at=?2
              WHERE operation_id=?3 AND step_id=?4 AND state='started'",
            rusqlite::params![link_state, now, operation_id, step_id],
        )?
    };
    let step_changed = tx.execute(
        "UPDATE operation_steps SET state=?1,phase=?2,finished_at=?3,
                revision=revision+1,updated_at=?4
          WHERE operation_id=?5 AND step_id=?6 AND state='running'",
        rusqlite::params![
            operation_state,
            phase,
            finished_at,
            now,
            operation_id,
            step_id
        ],
    )?;
    let operation_changed = tx.execute(
        "UPDATE operations SET state=?1,phase=?2,finished_at=?3,
                revision=revision+1,updated_at=?4
          WHERE operation_id=?5 AND state='running'",
        rusqlite::params![operation_state, phase, finished_at, now, operation_id],
    )?;
    if matches!(state, FinishState::Interrupted) {
        tx.execute(
            "UPDATE ai_batches SET status='interrupted'
              WHERE id=(SELECT batch_id FROM tagging_batch_links
                         WHERE operation_id=?1 AND step_id=?2)",
            rusqlite::params![operation_id, step_id],
        )?;
    } else if matches!(state, FinishState::Cancelled) {
        tx.execute(
            "UPDATE ai_batches SET status='cancelled'
              WHERE id=(SELECT batch_id FROM tagging_batch_links
                         WHERE operation_id=?1 AND step_id=?2)",
            rusqlite::params![operation_id, step_id],
        )?;
    }
    if operation_state == "outcome_unknown" {
        let (pending_steps, running_steps): (i64, i64) = tx.query_row(
            "SELECT
                COALESCE(SUM(CASE WHEN state IN ('queued','running','interrupted') THEN 1 ELSE 0 END),0),
                COALESCE(SUM(CASE WHEN state='running' THEN 1 ELSE 0 END),0)
               FROM operation_steps WHERE operation_id=?1",
            [operation_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        let unknown_items: i64 = tx.query_row(
            "SELECT
                (SELECT COUNT(*) FROM operation_items
                  WHERE operation_id=?1
                    AND (state='outcome_unknown' OR effect_state='unknown'))
                + (SELECT COUNT(*) FROM tagging_batch_links l
                    WHERE l.operation_id=?1 AND l.requests_attempted>0
                      AND NOT EXISTS (SELECT 1 FROM operation_items i
                                       WHERE i.operation_id=l.operation_id
                                         AND i.step_id=l.step_id))",
            [operation_id],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO operation_recovery(
                operation_id,pending_steps,running_steps,unknown_items,
                requires_resume,created_at,resolved_at
             ) VALUES(?1,?2,?3,?4,?5,?6,NULL)
             ON CONFLICT(operation_id) DO UPDATE SET
                pending_steps=excluded.pending_steps,
                running_steps=excluded.running_steps,
                unknown_items=excluded.unknown_items,
                requires_resume=excluded.requires_resume,
                created_at=excluded.created_at,
                resolved_at=NULL",
            rusqlite::params![
                operation_id,
                pending_steps,
                running_steps,
                unknown_items,
                if pending_steps > 0 { 1 } else { 0 },
                now
            ],
        )?;
    } else if matches!(state, FinishState::Completed | FinishState::Cancelled) {
        tx.execute(
            "UPDATE operation_recovery SET resolved_at=?1,requires_resume=0
              WHERE operation_id=?2 AND resolved_at IS NULL",
            rusqlite::params![now, operation_id],
        )?;
    }
    if link_changed != 1 || step_changed != 1 || operation_changed != 1 {
        return Err(AppError::from(
            crate::contracts::agent::AgentErrorCode::ResourceConflict,
        ));
    }
    tx.commit()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_upgrade_removes_single_batch_constraint_and_preserves_source_uniqueness() {
        let conn = crate::db::init_memory().unwrap();
        conn.execute_batch(
            "DROP TABLE tagging_batch_links;
             CREATE TABLE tagging_batch_links (
                operation_id TEXT NOT NULL,
                step_id TEXT NOT NULL,
                plan_id TEXT NOT NULL REFERENCES plans(plan_id) ON DELETE CASCADE,
                batch_id INTEGER NOT NULL REFERENCES ai_batches(id) ON DELETE CASCADE,
                source_operation_id TEXT,
                source_step_id TEXT,
                source_fingerprint TEXT NOT NULL,
                config_hash TEXT NOT NULL,
                request_limit INTEGER NOT NULL CHECK(request_limit >= 0),
                requests_attempted INTEGER NOT NULL DEFAULT 0 CHECK(requests_attempted >= 0),
                state TEXT NOT NULL CHECK(state IN (
                    'awaiting_user_start','started','interrupted','completed'
                )),
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL,
                PRIMARY KEY(operation_id,step_id),
                UNIQUE(batch_id),
                UNIQUE(source_operation_id,source_step_id),
                CHECK((source_operation_id IS NULL) = (source_step_id IS NULL)),
                FOREIGN KEY(operation_id,step_id)
                    REFERENCES operation_steps(operation_id,step_id) ON DELETE CASCADE
             );",
        )
        .unwrap();

        migrate_structure(&conn).unwrap();
        migrate_structure(&conn).unwrap();
        let mut statement = conn
            .prepare("PRAGMA index_list('tagging_batch_links')")
            .unwrap();
        let indexes = statement
            .query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, bool>(2)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        drop(statement);
        let mut unique_columns = Vec::new();
        for (name, _) in indexes.into_iter().filter(|(_, unique)| *unique) {
            let quoted = name.replace('\'', "''");
            let mut statement = conn
                .prepare(&format!("PRAGMA index_info('{quoted}')"))
                .unwrap();
            let columns = statement
                .query_map([], |row| row.get::<_, String>(2))
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            unique_columns.push(columns);
        }
        assert!(!unique_columns
            .iter()
            .any(|columns| columns.len() == 1 && columns[0] == "batch_id"));
        assert!(unique_columns
            .iter()
            .any(|columns| columns == &["operation_id", "step_id"]));
        assert!(unique_columns
            .iter()
            .any(|columns| { columns == &["source_operation_id", "source_step_id"] }));
        let schema: String = conn
            .query_row(
                "SELECT sql FROM sqlite_master WHERE type='table' AND name='tagging_batch_links'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(schema.contains("'cancelled'"));
    }
}
