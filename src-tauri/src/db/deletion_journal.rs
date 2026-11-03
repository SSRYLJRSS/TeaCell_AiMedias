//! A4-04: per-file journal for permanent deletions.
//!
//! Permanent deletion is the one effect this app cannot undo, and a SQL
//! transaction never rolls back the file system (implementation contract §4.2).
//! So every item gets durable evidence before the bytes go away and durable
//! evidence after they are gone. The ladder is:
//!
//! `intent` → `file_deleted` → `committed`
//!
//! with `failed`, `missing` and `unknown` as terminal exits:
//!
//! - `failed`  the file is still on disk; the database row stays in the trash.
//! - `missing` the file was already absent before we touched it. Report only.
//! - `unknown` a crash or a foreign change means we cannot prove whether the
//!   file we deleted was the file the plan froze. Report only, never widen.
//!
//! Both entry points share this journal: the trusted-GUI "empty the trash"
//! operation (`purpose = trash_empty`, scoped by the operation id) and the
//! scheduled expiry sweep (`purpose = trash_expiry`, scoped by a synthetic
//! `expiry_<unix>` id) run the same deletion service and produce the same
//! evidence.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{AppError, AppResult};

pub const STATE_INTENT: &str = "intent";
pub const STATE_FILE_DELETED: &str = "file_deleted";
pub const STATE_COMMITTED: &str = "committed";
pub const STATE_FAILED: &str = "failed";
pub const STATE_MISSING: &str = "missing";
pub const STATE_UNKNOWN: &str = "unknown";

pub const PURPOSE_EMPTY: &str = "trash_empty";
pub const PURPOSE_EXPIRY: &str = "trash_expiry";

pub fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS deletion_journal (
  scope_id        TEXT NOT NULL,
  item_id         TEXT NOT NULL,
  asset_id        INTEGER NOT NULL,
  library_id      TEXT NOT NULL,
  library_epoch   TEXT NOT NULL,
  purpose         TEXT NOT NULL CHECK(purpose IN ('trash_empty','trash_expiry')),
  source_path     TEXT NOT NULL,
  source_identity TEXT,
  state           TEXT NOT NULL
                  CHECK(state IN ('intent','file_deleted','committed','failed','missing','unknown')),
  error_code      TEXT,
  updated_at      INTEGER NOT NULL,
  PRIMARY KEY(scope_id, item_id)
);
CREATE INDEX IF NOT EXISTS ix_deletion_journal_state ON deletion_journal(state);
CREATE INDEX IF NOT EXISTS ix_deletion_journal_asset ON deletion_journal(asset_id);
"#,
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletionEntry {
    pub scope_id: String,
    pub item_id: String,
    pub asset_id: i64,
    pub library_id: String,
    pub library_epoch: String,
    pub purpose: String,
    pub source_path: String,
    pub source_identity: Option<String>,
    pub state: String,
    pub error_code: Option<String>,
    pub updated_at: i64,
}

pub struct NewDeletionIntent<'a> {
    pub scope_id: &'a str,
    pub item_id: &'a str,
    pub asset_id: i64,
    pub library_id: &'a str,
    pub library_epoch: &'a str,
    pub purpose: &'a str,
    pub source_path: &'a str,
    pub source_identity: Option<&'a str>,
    pub updated_at: i64,
}

const COLS: &str = "scope_id, item_id, asset_id, library_id, library_epoch, purpose, \
                    source_path, source_identity, state, error_code, updated_at";

fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeletionEntry> {
    Ok(DeletionEntry {
        scope_id: row.get(0)?,
        item_id: row.get(1)?,
        asset_id: row.get(2)?,
        library_id: row.get(3)?,
        library_epoch: row.get(4)?,
        purpose: row.get(5)?,
        source_path: row.get(6)?,
        source_identity: row.get(7)?,
        state: row.get(8)?,
        error_code: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

/// Record the intent before touching the file system.
///
/// Re-recording the same intent is idempotent; a different path for the same
/// item is a conflict because the frozen plan changed under a running sweep.
pub fn record_intent(conn: &Connection, intent: &NewDeletionIntent<'_>) -> AppResult<()> {
    conn.execute(
        "INSERT INTO deletion_journal(
            scope_id,item_id,asset_id,library_id,library_epoch,purpose,
            source_path,source_identity,state,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,?7,?8,'intent',?9)
         ON CONFLICT(scope_id,item_id) DO NOTHING",
        params![
            intent.scope_id,
            intent.item_id,
            intent.asset_id,
            intent.library_id,
            intent.library_epoch,
            intent.purpose,
            intent.source_path,
            intent.source_identity,
            intent.updated_at,
        ],
    )?;
    let existing = entry(conn, intent.scope_id, intent.item_id)?
        .ok_or_else(|| AppError::internal("删除日志写入后不可读"))?;
    if existing.source_path != intent.source_path || existing.purpose != intent.purpose {
        return Err(AppError::conflict("清空计划在执行中发生变化"));
    }
    Ok(())
}

/// `intent` → `file_deleted`. Only the winner owns the following database
/// delete, so a duplicate worker cannot claim the same permanent effect twice.
pub fn record_file_deleted(
    conn: &Connection,
    scope_id: &str,
    item_id: &str,
    now: i64,
) -> AppResult<()> {
    transition(
        conn,
        "UPDATE deletion_journal SET state='file_deleted', error_code=NULL, updated_at=?3
         WHERE scope_id=?1 AND item_id=?2 AND state='intent'",
        params![scope_id, item_id, now],
    )
}

/// `file_deleted` → `committed`. Called only in the same transaction that
/// removes the asset row, so the row and the evidence commit together.
pub fn record_committed(
    conn: &Connection,
    scope_id: &str,
    item_id: &str,
    now: i64,
) -> AppResult<()> {
    transition(
        conn,
        "UPDATE deletion_journal SET state='committed', error_code=NULL, updated_at=?3
         WHERE scope_id=?1 AND item_id=?2 AND state='file_deleted'",
        params![scope_id, item_id, now],
    )
}

/// The file is still there (or could not be removed): keep the database record.
pub fn record_failed(
    conn: &Connection,
    scope_id: &str,
    item_id: &str,
    error_code: &str,
    now: i64,
) -> AppResult<()> {
    transition(
        conn,
        "UPDATE deletion_journal SET state='failed', error_code=?3, updated_at=?4
         WHERE scope_id=?1 AND item_id=?2 AND state IN ('intent','file_deleted')",
        params![scope_id, item_id, error_code, now],
    )
}

/// The file was already absent before this run. Nothing was deleted by us.
pub fn record_missing(conn: &Connection, scope_id: &str, item_id: &str, now: i64) -> AppResult<()> {
    transition(
        conn,
        "UPDATE deletion_journal SET state='missing', error_code='FILE_MISSING', updated_at=?3
         WHERE scope_id=?1 AND item_id=?2 AND state='intent'",
        params![scope_id, item_id, now],
    )
}

/// Cannot prove which file the plan froze. Reported, never widened.
pub fn record_unknown(
    conn: &Connection,
    scope_id: &str,
    item_id: &str,
    error_code: &str,
    now: i64,
) -> AppResult<()> {
    transition(
        conn,
        "UPDATE deletion_journal SET state='unknown', error_code=?3, updated_at=?4
         WHERE scope_id=?1 AND item_id=?2 AND state IN ('intent','file_deleted')",
        params![scope_id, item_id, error_code, now],
    )
}

fn transition(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> AppResult<()> {
    let changed = conn.execute(sql, params)?;
    if changed != 1 {
        // Another attempt already moved this item, or the journal row belongs
        // to a different plan. Callers must not treat this as success.
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

pub fn entry(conn: &Connection, scope_id: &str, item_id: &str) -> AppResult<Option<DeletionEntry>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM deletion_journal WHERE scope_id=?1 AND item_id=?2"),
            params![scope_id, item_id],
            from_row,
        )
        .optional()?)
}

pub fn entries(conn: &Connection, scope_id: &str) -> AppResult<Vec<DeletionEntry>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLS} FROM deletion_journal WHERE scope_id=?1 ORDER BY item_id"
    ))?;
    let rows = statement.query_map(params![scope_id], from_row)?;
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

/// Rows that still need reconciliation because we cannot tell whether the
/// permanent effect happened.
pub fn unfinished(conn: &Connection) -> AppResult<Vec<DeletionEntry>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {COLS} FROM deletion_journal
         WHERE state IN ('intent','file_deleted')
         ORDER BY updated_at, scope_id, item_id"
    ))?;
    let rows = statement.query_map([], from_row)?;
    let mut entries = Vec::new();
    for row in rows {
        entries.push(row?);
    }
    Ok(entries)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    fn intent<'a>(scope_id: &'a str, item_id: &'a str, asset_id: i64) -> NewDeletionIntent<'a> {
        NewDeletionIntent {
            scope_id,
            item_id,
            asset_id,
            library_id: "lib_test",
            library_epoch: "epoch_test",
            purpose: PURPOSE_EMPTY,
            source_path: "C:/photos/a.jpg",
            source_identity: Some("windows:1:2:3:4:5"),
            updated_at: 100,
        }
    }

    #[test]
    fn structure_is_idempotent_and_the_ladder_is_enforced() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        migrate_structure(&conn).unwrap();

        record_intent(&conn, &intent("op_1", "item_1", 7)).unwrap();
        // Committing before the file is deleted must not be possible.
        assert!(record_committed(&conn, "op_1", "item_1", 200).is_err());
        assert_eq!(
            entry(&conn, "op_1", "item_1").unwrap().unwrap().state,
            STATE_INTENT
        );

        record_file_deleted(&conn, "op_1", "item_1", 200).unwrap();
        record_committed(&conn, "op_1", "item_1", 300).unwrap();
        let stored = entry(&conn, "op_1", "item_1").unwrap().unwrap();
        assert_eq!(stored.state, STATE_COMMITTED);
        assert_eq!(stored.error_code, None);
        // A second commit is a replay, not a second effect.
        assert!(record_committed(&conn, "op_1", "item_1", 400).is_err());
    }

    #[test]
    fn a_changed_intent_is_rejected_and_unfinished_lists_only_open_rows() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        record_intent(&conn, &intent("op_1", "item_1", 7)).unwrap();
        let mut moved = intent("op_1", "item_1", 7);
        moved.source_path = "C:/photos/other.jpg";
        assert!(record_intent(&conn, &moved).is_err(), "计划变化必须拒绝");

        record_intent(&conn, &intent("op_1", "item_2", 8)).unwrap();
        record_missing(&conn, "op_1", "item_2", 200).unwrap();
        record_intent(&conn, &intent("op_1", "item_3", 9)).unwrap();
        record_file_deleted(&conn, "op_1", "item_3", 200).unwrap();

        let open: Vec<String> = unfinished(&conn)
            .unwrap()
            .into_iter()
            .map(|entry| entry.item_id)
            .collect();
        assert_eq!(open, vec!["item_1".to_string(), "item_3".to_string()]);
    }

    #[test]
    fn the_same_asset_can_be_tracked_by_two_scopes() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        record_intent(&conn, &intent("op_1", "item_1", 7)).unwrap();
        let mut sweep = intent("expiry_1", "item_1", 7);
        sweep.purpose = PURPOSE_EXPIRY;
        record_intent(&conn, &sweep).unwrap();
        assert_eq!(entries(&conn, "op_1").unwrap().len(), 1);
        assert_eq!(
            entries(&conn, "expiry_1").unwrap()[0].purpose,
            PURPOSE_EXPIRY
        );
    }
}
