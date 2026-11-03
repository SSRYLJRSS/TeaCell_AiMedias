//! A4-02: per-file relocation journal for in-library moves.
//!
//! A SQL transaction never rolls back the file system (implementation contract
//! §4.2), so a move needs durable intent *before* the bytes move and durable
//! evidence *after* they land. Each row walks one strict ladder:
//!
//! `intent` → `moved` → `committed`
//!
//! with `failed` / `rolled_back` as terminal exits. Every transition is a
//! conditional update on the previous state, so two attempts cannot both claim
//! the same item and a replay after a crash can tell "the file never moved"
//! apart from "the file moved but the row did not".

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{AppError, AppResult};

pub const STATE_INTENT: &str = "intent";
pub const STATE_MOVED: &str = "moved";
pub const STATE_COMMITTED: &str = "committed";
pub const STATE_FAILED: &str = "failed";
pub const STATE_ROLLED_BACK: &str = "rolled_back";

pub const CODE_TARGET_EXISTS: &str = "TARGET_EXISTS";
pub const CODE_TARGET_OCCUPIED: &str = "TARGET_OCCUPIED";
pub const CODE_SOURCE_MISSING: &str = "SOURCE_MISSING";
pub const CODE_SOURCE_CHANGED: &str = "SOURCE_CHANGED";
pub const CODE_LIBRARY_CHANGED: &str = "LIBRARY_CHANGED";
pub const CODE_PERMISSION_DENIED: &str = "PERMISSION_DENIED";
pub const CODE_CANCELLED: &str = "CANCELLED";
pub const CODE_IO_ERROR: &str = "IO_ERROR";

pub fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS move_journal (
  operation_id    TEXT NOT NULL,
  item_id         TEXT NOT NULL,
  asset_id        INTEGER NOT NULL,
  source_path     TEXT NOT NULL,
  target_path     TEXT NOT NULL,
  source_identity TEXT NOT NULL,
  target_identity TEXT,
  target_modified_at INTEGER,
  state           TEXT NOT NULL
                  CHECK(state IN ('intent','moved','committed','failed','rolled_back')),
  error_code      TEXT,
  updated_at      INTEGER NOT NULL,
  PRIMARY KEY(operation_id, item_id)
);
CREATE INDEX IF NOT EXISTS ix_move_journal_state ON move_journal(state);
"#,
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveJournalEntry {
    pub operation_id: String,
    pub item_id: String,
    pub asset_id: i64,
    pub source_path: String,
    pub target_path: String,
    pub source_identity: String,
    pub target_identity: Option<String>,
    pub target_modified_at: Option<i64>,
    pub state: String,
    pub error_code: Option<String>,
    pub updated_at: i64,
}

pub struct NewMoveIntent<'a> {
    pub operation_id: &'a str,
    pub item_id: &'a str,
    pub asset_id: i64,
    pub source_path: &'a str,
    pub target_path: &'a str,
    pub source_identity: &'a str,
    pub updated_at: i64,
}

/// Record the intent before touching the file system. Re-recording the same
/// intent is idempotent; a different intent for the same item is a conflict
/// because it means the plan changed under a running operation.
pub fn record_intent(conn: &Connection, intent: &NewMoveIntent<'_>) -> AppResult<()> {
    conn.execute(
        "INSERT INTO move_journal(
            operation_id,item_id,asset_id,source_path,target_path,
            source_identity,state,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,'intent',?7)
         ON CONFLICT(operation_id,item_id) DO NOTHING",
        params![
            intent.operation_id,
            intent.item_id,
            intent.asset_id,
            intent.source_path,
            intent.target_path,
            intent.source_identity,
            intent.updated_at,
        ],
    )?;
    let existing = entry(conn, intent.operation_id, intent.item_id)?
        .ok_or_else(|| AppError::internal("移动日志写入后不可读"))?;
    if existing.source_path != intent.source_path
        || existing.target_path != intent.target_path
        || existing.source_identity != intent.source_identity
    {
        return Err(AppError::conflict("移动计划在执行中发生变化"));
    }
    Ok(())
}

/// `intent` → `moved`. Only the winner of this transition owns the following
/// database update, so a duplicate worker cannot commit twice.
pub fn record_moved(
    conn: &Connection,
    operation_id: &str,
    item_id: &str,
    target_identity: &str,
    target_modified_at: i64,
    now: i64,
) -> AppResult<()> {
    transition(
        conn,
        "UPDATE move_journal
            SET state='moved',target_identity=?3,target_modified_at=?4,error_code=NULL,updated_at=?5
          WHERE operation_id=?1 AND item_id=?2 AND state='intent'",
        params![
            operation_id,
            item_id,
            target_identity,
            target_modified_at,
            now
        ],
    )
}

/// `moved` → `committed`. Called only after the asset row and its verified
/// location evidence have been written in the same transaction.
pub fn record_committed(
    conn: &Connection,
    operation_id: &str,
    item_id: &str,
    now: i64,
) -> AppResult<()> {
    transition(
        conn,
        "UPDATE move_journal
            SET state='committed',error_code=NULL,updated_at=?3
          WHERE operation_id=?1 AND item_id=?2 AND state='moved'",
        params![operation_id, item_id, now],
    )
}

/// Terminal failure. Allowed from any non-committed state.
pub fn record_failed(
    conn: &Connection,
    operation_id: &str,
    item_id: &str,
    error_code: &str,
    now: i64,
) -> AppResult<()> {
    transition(
        conn,
        "UPDATE move_journal
            SET state='failed',error_code=?3,updated_at=?4
          WHERE operation_id=?1 AND item_id=?2 AND state IN ('intent','moved')",
        params![operation_id, item_id, error_code, now],
    )
}

/// The file never left its source (or was put back). Nothing to reconcile.
pub fn record_rolled_back(
    conn: &Connection,
    operation_id: &str,
    item_id: &str,
    now: i64,
) -> AppResult<()> {
    transition(
        conn,
        "UPDATE move_journal
            SET state='rolled_back',updated_at=?3
          WHERE operation_id=?1 AND item_id=?2 AND state='intent'",
        params![operation_id, item_id, now],
    )
}

fn transition(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> AppResult<()> {
    let changed = conn.execute(sql, params)?;
    if changed != 1 {
        // The row was not in the expected state: another attempt already moved
        // it, or the journal was written by a different plan.
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

pub fn entry(
    conn: &Connection,
    operation_id: &str,
    item_id: &str,
) -> AppResult<Option<MoveJournalEntry>> {
    Ok(conn
        .query_row(
            "SELECT operation_id,item_id,asset_id,source_path,target_path,source_identity,
                    target_identity,target_modified_at,state,error_code,updated_at
               FROM move_journal WHERE operation_id=?1 AND item_id=?2",
            params![operation_id, item_id],
            |row| {
                Ok(MoveJournalEntry {
                    operation_id: row.get(0)?,
                    item_id: row.get(1)?,
                    asset_id: row.get(2)?,
                    source_path: row.get(3)?,
                    target_path: row.get(4)?,
                    source_identity: row.get(5)?,
                    target_identity: row.get(6)?,
                    target_modified_at: row.get(7)?,
                    state: row.get(8)?,
                    error_code: row.get(9)?,
                    updated_at: row.get(10)?,
                })
            },
        )
        .optional()?)
}

pub fn entries(conn: &Connection, operation_id: &str) -> AppResult<Vec<MoveJournalEntry>> {
    let mut stmt = conn.prepare(
        "SELECT operation_id,item_id,asset_id,source_path,target_path,source_identity,
                target_identity,target_modified_at,state,error_code,updated_at
           FROM move_journal WHERE operation_id=?1 ORDER BY item_id",
    )?;
    let rows = stmt.query_map(params![operation_id], |row| {
        Ok(MoveJournalEntry {
            operation_id: row.get(0)?,
            item_id: row.get(1)?,
            asset_id: row.get(2)?,
            source_path: row.get(3)?,
            target_path: row.get(4)?,
            source_identity: row.get(5)?,
            target_identity: row.get(6)?,
            target_modified_at: row.get(7)?,
            state: row.get(8)?,
            error_code: row.get(9)?,
            updated_at: row.get(10)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Every row that still needs reconciliation (a move that was interrupted
/// between the file system and the database).
pub fn unfinished(conn: &Connection) -> AppResult<Vec<MoveJournalEntry>> {
    let mut stmt = conn.prepare(
        "SELECT operation_id,item_id,asset_id,source_path,target_path,source_identity,
                target_identity,target_modified_at,state,error_code,updated_at
           FROM move_journal WHERE state IN ('intent','moved') ORDER BY updated_at",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(MoveJournalEntry {
            operation_id: row.get(0)?,
            item_id: row.get(1)?,
            asset_id: row.get(2)?,
            source_path: row.get(3)?,
            target_path: row.get(4)?,
            source_identity: row.get(5)?,
            target_identity: row.get(6)?,
            target_modified_at: row.get(7)?,
            state: row.get(8)?,
            error_code: row.get(9)?,
            updated_at: row.get(10)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    fn intent<'a>(operation_id: &'a str, item_id: &'a str, target: &'a str) -> NewMoveIntent<'a> {
        NewMoveIntent {
            operation_id,
            item_id,
            asset_id: 7,
            source_path: "d:/library/in/a.jpg",
            target_path: target,
            source_identity: "windows:1:2:3:4:5",
            updated_at: 100,
        }
    }

    #[test]
    fn structure_is_idempotent_and_the_state_ladder_is_enforced() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        migrate_structure(&conn).unwrap();

        record_intent(&conn, &intent("op", "item", "d:/library/out/a.jpg")).unwrap();
        // Replaying the same intent is fine; a different target is a conflict.
        record_intent(&conn, &intent("op", "item", "d:/library/out/a.jpg")).unwrap();
        assert!(record_intent(&conn, &intent("op", "item", "d:/library/other/a.jpg")).is_err());

        // Cannot commit before the bytes moved.
        assert!(record_committed(&conn, "op", "item", 110)
            .unwrap_err()
            .to_string()
            .contains("OUTCOME_UNKNOWN"));

        record_moved(&conn, "op", "item", "windows:1:2:3:4:6", 999, 120).unwrap();
        assert!(record_moved(&conn, "op", "item", "windows:1:2:3:4:6", 999, 121).is_err());
        // A moved row can no longer be rolled back.
        assert!(record_rolled_back(&conn, "op", "item", 122).is_err());

        record_committed(&conn, "op", "item", 130).unwrap();
        let stored = entry(&conn, "op", "item").unwrap().unwrap();
        assert_eq!(stored.state, STATE_COMMITTED);
        assert_eq!(stored.target_modified_at, Some(999));
        assert_eq!(stored.target_identity.as_deref(), Some("windows:1:2:3:4:6"));
        assert!(unfinished(&conn).unwrap().is_empty());
    }

    #[test]
    fn unfinished_only_lists_rows_that_still_need_reconciliation() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        record_intent(&conn, &intent("op", "a", "d:/library/out/a.jpg")).unwrap();
        record_intent(&conn, &intent("op", "b", "d:/library/out/b.jpg")).unwrap();
        record_intent(&conn, &intent("op", "c", "d:/library/out/c.jpg")).unwrap();

        record_moved(&conn, "op", "b", "windows:1:2:3:4:7", 5, 200).unwrap();
        record_failed(&conn, "op", "c", CODE_TARGET_EXISTS, 201).unwrap();

        let pending = unfinished(&conn).unwrap();
        let ids: Vec<&str> = pending.iter().map(|row| row.item_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "b"]);
        assert_eq!(entries(&conn, "op").unwrap().len(), 3);
    }
}
