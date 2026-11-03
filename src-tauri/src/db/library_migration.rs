//! A4-05: durable journal for whole-library migration and relocation.
//!
//! A library move crosses two systems a SQL transaction cannot cover: the file
//! system and the library locator row. Implementation contract §4.3 fixes the
//! order — freeze conflicting work, snapshot the manifest, preflight the target,
//! copy and verify, switch the locator in one short transaction, rebuild
//! projections, report residue. Each of those steps is persisted here so a crash
//! at any point is recoverable instead of leaving a half-migrated library.
//!
//! Run ladder:
//!
//! `preparing` → `copying` → `switched` → `cleaning` → `completed`
//!
//! with `failed` and `aborted` as terminal exits:
//!
//! - `failed`   a step failed and the source still holds the authoritative copy.
//! - `aborted`  the run was cancelled before the locator switched.
//!
//! `switched` is the point of no return for the locator: the target root becomes
//! the current generation (new epoch) while the source copy is still intact, so
//! "switched but source not cleaned" is a legitimate resumable state, never a
//! silent data loss.
//!
//! Item ladder: `pending` → `copied` → `verified`, with `failed` and `skipped`
//! as exits. The run is only allowed to switch the locator when every item is
//! `verified` (or has a recorded, reported failure that the caller chose to
//! accept — the service never does).

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{AppError, AppResult};

pub const KIND_MIGRATE: &str = "migrate";
pub const KIND_RELOCATE: &str = "relocate";

pub const PHASE_PREPARING: &str = "preparing";
pub const PHASE_COPYING: &str = "copying";
pub const PHASE_SWITCHED: &str = "switched";
pub const PHASE_CLEANING: &str = "cleaning";
pub const PHASE_COMPLETED: &str = "completed";
pub const PHASE_FAILED: &str = "failed";
pub const PHASE_ABORTED: &str = "aborted";

pub const ITEM_PENDING: &str = "pending";
pub const ITEM_COPIED: &str = "copied";
pub const ITEM_VERIFIED: &str = "verified";
pub const ITEM_FAILED: &str = "failed";
pub const ITEM_SKIPPED: &str = "skipped";

pub const CODE_TARGET_NOT_EMPTY: &str = "TARGET_NOT_EMPTY";
pub const CODE_TARGET_INSIDE_SOURCE: &str = "TARGET_INSIDE_SOURCE";
pub const CODE_TARGET_IS_LINK: &str = "TARGET_IS_LINK";
pub const CODE_SOURCE_MISSING: &str = "SOURCE_MISSING";
pub const CODE_SOURCE_CHANGED: &str = "SOURCE_CHANGED";
pub const CODE_INSUFFICIENT_SPACE: &str = "INSUFFICIENT_SPACE";
pub const CODE_WRONG_LIBRARY: &str = "WRONG_LIBRARY";
pub const CODE_FILE_MISSING: &str = "FILE_MISSING";
pub const CODE_CHECKSUM_MISMATCH: &str = "CHECKSUM_MISMATCH";
pub const CODE_PERMISSION_DENIED: &str = "PERMISSION_DENIED";
pub const CODE_LIBRARY_CHANGED: &str = "LIBRARY_CHANGED";
pub const CODE_INTERRUPTED: &str = "INTERRUPTED";
pub const CODE_IO_ERROR: &str = "IO_ERROR";

pub fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS library_migration_runs (
  run_id         TEXT PRIMARY KEY,
  library_id     TEXT NOT NULL,
  source_epoch   TEXT NOT NULL,
  source_root    TEXT NOT NULL,
  target_root    TEXT NOT NULL,
  kind           TEXT NOT NULL CHECK(kind IN ('migrate','relocate')),
  phase          TEXT NOT NULL
                 CHECK(phase IN ('preparing','copying','switched','cleaning','completed','failed','aborted')),
  target_epoch   TEXT,
  total_items    INTEGER NOT NULL DEFAULT 0,
  copied_items   INTEGER NOT NULL DEFAULT 0,
  failed_items   INTEGER NOT NULL DEFAULT 0,
  error_code     TEXT,
  error_message  TEXT,
  created_at     INTEGER NOT NULL,
  updated_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_library_migration_runs_phase ON library_migration_runs(phase);
CREATE TABLE IF NOT EXISTS library_migration_items (
  run_id         TEXT NOT NULL,
  item_id        TEXT NOT NULL,
  relative_path  TEXT NOT NULL,
  byte_size      INTEGER NOT NULL,
  content_sha256 TEXT,
  state          TEXT NOT NULL CHECK(state IN ('pending','copied','verified','failed','skipped')),
  error_code     TEXT,
  updated_at     INTEGER NOT NULL,
  PRIMARY KEY(run_id,item_id)
);
CREATE INDEX IF NOT EXISTS ix_library_migration_items_state ON library_migration_items(run_id,state);
"#,
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationRun {
    pub run_id: String,
    pub library_id: String,
    pub source_epoch: String,
    pub source_root: String,
    pub target_root: String,
    pub kind: String,
    pub phase: String,
    pub target_epoch: Option<String>,
    pub total_items: i64,
    pub copied_items: i64,
    pub failed_items: i64,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationItem {
    pub run_id: String,
    pub item_id: String,
    pub relative_path: String,
    pub byte_size: i64,
    pub content_sha256: Option<String>,
    pub state: String,
    pub error_code: Option<String>,
    pub updated_at: i64,
}

const RUN_COLS: &str = "run_id, library_id, source_epoch, source_root, target_root, kind, phase, \
                        target_epoch, total_items, copied_items, failed_items, error_code, \
                        error_message, created_at, updated_at";

const ITEM_COLS: &str = "run_id, item_id, relative_path, byte_size, content_sha256, state, \
                         error_code, updated_at";

fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MigrationRun> {
    Ok(MigrationRun {
        run_id: row.get(0)?,
        library_id: row.get(1)?,
        source_epoch: row.get(2)?,
        source_root: row.get(3)?,
        target_root: row.get(4)?,
        kind: row.get(5)?,
        phase: row.get(6)?,
        target_epoch: row.get(7)?,
        total_items: row.get(8)?,
        copied_items: row.get(9)?,
        failed_items: row.get(10)?,
        error_code: row.get(11)?,
        error_message: row.get(12)?,
        created_at: row.get(13)?,
        updated_at: row.get(14)?,
    })
}

fn item_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<MigrationItem> {
    Ok(MigrationItem {
        run_id: row.get(0)?,
        item_id: row.get(1)?,
        relative_path: row.get(2)?,
        byte_size: row.get(3)?,
        content_sha256: row.get(4)?,
        state: row.get(5)?,
        error_code: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

/// Open a run in `preparing`. The caller has already frozen conflicting work.
// Nine fields are the run's identity, not a knob; grouping them into a struct would
// only move the same list to every call site.
#[allow(clippy::too_many_arguments)]
pub fn open_run(
    conn: &Connection,
    run_id: &str,
    library_id: &str,
    source_epoch: &str,
    source_root: &str,
    target_root: &str,
    kind: &str,
    now: i64,
) -> AppResult<()> {
    if !matches!(kind, KIND_MIGRATE | KIND_RELOCATE) {
        return Err(AppError::invalid_arg("未知的总仓迁移类型"));
    }
    let changed = conn.execute(
        "INSERT INTO library_migration_runs(
            run_id,library_id,source_epoch,source_root,target_root,kind,phase,
            created_at,updated_at)
         VALUES(?1,?2,?3,?4,?5,?6,'preparing',?7,?7)
         ON CONFLICT(run_id) DO NOTHING",
        params![
            run_id,
            library_id,
            source_epoch,
            source_root,
            target_root,
            kind,
            now
        ],
    )?;
    if changed == 0 {
        let existing =
            run(conn, run_id)?.ok_or_else(|| AppError::internal("总仓迁移日志写入后不可读"))?;
        if existing.source_epoch != source_epoch
            || existing.source_root != source_root
            || existing.target_root != target_root
            || existing.kind != kind
        {
            return Err(AppError::conflict("总仓迁移计划在执行中发生变化"));
        }
    }
    Ok(())
}

/// Freeze the manifest. Items are inserted once; re-running a prepared run
/// keeps the first snapshot so a resumed run copies exactly what it planned —
/// a growing manifest would let a resumed run touch files the user never
/// confirmed.
pub fn seed_items(
    conn: &Connection,
    run_id: &str,
    items: &[(String, String, i64)],
    now: i64,
) -> AppResult<()> {
    let existing: i64 = conn.query_row(
        "SELECT COUNT(*) FROM library_migration_items WHERE run_id=?1",
        [run_id],
        |row| row.get(0),
    )?;
    if existing > 0 {
        return Ok(());
    }
    let mut statement = conn.prepare(
        "INSERT INTO library_migration_items(
            run_id,item_id,relative_path,byte_size,state,updated_at)
         VALUES(?1,?2,?3,?4,'pending',?5)
         ON CONFLICT(run_id,item_id) DO NOTHING",
    )?;
    for (item_id, relative_path, byte_size) in items {
        statement.execute(params![run_id, item_id, relative_path, byte_size, now])?;
    }
    drop(statement);
    let total: i64 = conn.query_row(
        "SELECT COUNT(*) FROM library_migration_items WHERE run_id=?1",
        [run_id],
        |row| row.get(0),
    )?;
    conn.execute(
        "UPDATE library_migration_runs SET total_items=?2,updated_at=?3 WHERE run_id=?1",
        params![run_id, total, now],
    )?;
    Ok(())
}

pub fn advance_phase(
    conn: &Connection,
    run_id: &str,
    from: &str,
    to: &str,
    now: i64,
) -> AppResult<()> {
    let changed = conn.execute(
        "UPDATE library_migration_runs SET phase=?3,updated_at=?4 WHERE run_id=?1 AND phase=?2",
        params![run_id, from, to, now],
    )?;
    if changed != 1 {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

/// Record the new library generation established by the switch. Only meaningful
/// in `switched` and later, and only once: the epoch is what invalidates every
/// plan, handle and remembered grant from the previous root.
pub fn record_switch(
    conn: &Connection,
    run_id: &str,
    target_epoch: &str,
    now: i64,
) -> AppResult<()> {
    let changed = conn.execute(
        "UPDATE library_migration_runs SET target_epoch=?2,phase='switched',updated_at=?3
          WHERE run_id=?1 AND phase='copying' AND target_epoch IS NULL",
        params![run_id, target_epoch, now],
    )?;
    if changed != 1 {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

pub fn finish(conn: &Connection, run_id: &str, phase: &str, now: i64) -> AppResult<()> {
    if !matches!(phase, PHASE_COMPLETED | PHASE_FAILED | PHASE_ABORTED) {
        return Err(AppError::invalid_arg("总仓迁移收尾阶段无效"));
    }
    // `completed` is only reachable once the locator has actually switched:
    // finishing a run that never changed the root would report success for a
    // migration that copied nothing into place.
    let allowed_from: &str = if phase == PHASE_COMPLETED {
        "phase IN ('switched','cleaning')"
    } else {
        "phase IN ('preparing','copying','switched','cleaning')"
    };
    let changed = conn.execute(
        &format!(
            "UPDATE library_migration_runs SET phase=?2,updated_at=?3
              WHERE run_id=?1 AND {allowed_from}"
        ),
        params![run_id, phase, now],
    )?;
    if changed != 1 {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

/// Record a run-level failure. The message is kept verbatim so the settings page
/// can explain the exact step that stopped instead of "迁移失败".
pub fn record_failure(
    conn: &Connection,
    run_id: &str,
    code: &str,
    message: &str,
    now: i64,
) -> AppResult<()> {
    let changed = conn.execute(
        "UPDATE library_migration_runs
            SET phase='failed',error_code=?2,error_message=?3,updated_at=?4
          WHERE run_id=?1 AND phase IN ('preparing','copying','switched','cleaning')",
        params![run_id, code, message, now],
    )?;
    if changed != 1 {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

/// `pending` → `copied`. Only the worker that copied the bytes owns the item.
pub fn record_copied(conn: &Connection, run_id: &str, item_id: &str, now: i64) -> AppResult<()> {
    transition(
        conn,
        "UPDATE library_migration_items SET state='copied',error_code=NULL,updated_at=?3
          WHERE run_id=?1 AND item_id=?2 AND state='pending'",
        params![run_id, item_id, now],
    )
}

/// `copied` → `verified`, binding the digest that proved the copy.
pub fn record_verified(
    conn: &Connection,
    run_id: &str,
    item_id: &str,
    content_sha256: &str,
    now: i64,
) -> AppResult<()> {
    transition(
        conn,
        "UPDATE library_migration_items
            SET state='verified',content_sha256=?3,error_code=NULL,updated_at=?4
          WHERE run_id=?1 AND item_id=?2 AND state='copied'",
        params![run_id, item_id, content_sha256, now],
    )
}

pub fn record_item_failure(
    conn: &Connection,
    run_id: &str,
    item_id: &str,
    code: &str,
    now: i64,
) -> AppResult<()> {
    transition(
        conn,
        "UPDATE library_migration_items SET state='failed',error_code=?3,updated_at=?4
          WHERE run_id=?1 AND item_id=?2 AND state IN ('pending','copied')",
        params![run_id, item_id, code, now],
    )?;
    refresh_counts(conn, run_id, now)
}

/// Reset a `failed` item back to `pending` so a resumed run retries it instead
/// of copying a stack of half files forever.
pub fn reset_item(conn: &Connection, run_id: &str, item_id: &str, now: i64) -> AppResult<()> {
    transition(
        conn,
        "UPDATE library_migration_items SET state='pending',error_code=NULL,updated_at=?3
          WHERE run_id=?1 AND item_id=?2 AND state IN ('failed','skipped')",
        params![run_id, item_id, now],
    )
}

fn transition(conn: &Connection, sql: &str, params: impl rusqlite::Params) -> AppResult<()> {
    let changed = conn.execute(sql, params)?;
    if changed != 1 {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

pub fn refresh_counts(conn: &Connection, run_id: &str, now: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE library_migration_runs
            SET copied_items=(SELECT COUNT(*) FROM library_migration_items
                               WHERE run_id=?1 AND state='verified'),
                failed_items=(SELECT COUNT(*) FROM library_migration_items
                               WHERE run_id=?1 AND state IN ('failed','skipped')),
                updated_at=?2
          WHERE run_id=?1",
        params![run_id, now],
    )?;
    Ok(())
}

pub fn run(conn: &Connection, run_id: &str) -> AppResult<Option<MigrationRun>> {
    Ok(conn
        .query_row(
            &format!("SELECT {RUN_COLS} FROM library_migration_runs WHERE run_id=?1"),
            [run_id],
            run_from_row,
        )
        .optional()?)
}

/// The run that still owns the library, if any. At most one run may be open at a
/// time: two concurrent migrations would race the locator row.
pub fn active_run(conn: &Connection) -> AppResult<Option<MigrationRun>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {RUN_COLS} FROM library_migration_runs
                  WHERE phase IN ('preparing','copying','switched','cleaning')
                  ORDER BY updated_at DESC, run_id LIMIT 1"
            ),
            [],
            run_from_row,
        )
        .optional()?)
}

pub fn items(conn: &Connection, run_id: &str) -> AppResult<Vec<MigrationItem>> {
    let mut statement = conn.prepare(&format!(
        "SELECT {ITEM_COLS} FROM library_migration_items WHERE run_id=?1 ORDER BY relative_path"
    ))?;
    let rows = statement.query_map([run_id], item_from_row)?;
    let mut items = Vec::new();
    for row in rows {
        items.push(row?);
    }
    Ok(items)
}

pub fn item(conn: &Connection, run_id: &str, item_id: &str) -> AppResult<Option<MigrationItem>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {ITEM_COLS} FROM library_migration_items WHERE run_id=?1 AND item_id=?2"
            ),
            params![run_id, item_id],
            item_from_row,
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    fn open(conn: &Connection) {
        migrate_structure(conn).unwrap();
        open_run(
            conn,
            "run_1",
            "lib_1",
            "epoch_old",
            "C:/old",
            "D:/new",
            KIND_MIGRATE,
            100,
        )
        .unwrap();
    }

    #[test]
    fn structure_is_idempotent_and_the_phase_ladder_is_enforced() {
        let conn = init_memory().unwrap();
        open(&conn);
        migrate_structure(&conn).unwrap();

        // Cannot switch before copying was recorded.
        assert!(record_switch(&conn, "run_1", "epoch_new", 200).is_err());
        advance_phase(&conn, "run_1", PHASE_PREPARING, PHASE_COPYING, 200).unwrap();
        // Cannot skip from copying straight to completed.
        assert!(finish(&conn, "run_1", PHASE_COMPLETED, 300).is_err());
        record_switch(&conn, "run_1", "epoch_new", 300).unwrap();
        let switched = run(&conn, "run_1").unwrap().unwrap();
        assert_eq!(switched.phase, PHASE_SWITCHED);
        assert_eq!(switched.target_epoch.as_deref(), Some("epoch_new"));

        advance_phase(&conn, "run_1", PHASE_SWITCHED, PHASE_CLEANING, 400).unwrap();
        finish(&conn, "run_1", PHASE_COMPLETED, 500).unwrap();
        assert_eq!(run(&conn, "run_1").unwrap().unwrap().phase, PHASE_COMPLETED);
        // A finished run is no longer active, and cannot be finished twice.
        assert!(active_run(&conn).unwrap().is_none());
        assert!(finish(&conn, "run_1", PHASE_COMPLETED, 600).is_err());
    }

    #[test]
    fn item_states_advance_once_and_only_verified_items_count_as_copied() {
        let conn = init_memory().unwrap();
        open(&conn);
        seed_items(
            &conn,
            "run_1",
            &[
                ("item_a".to_string(), "a.jpg".to_string(), 10),
                ("item_b".to_string(), "b.jpg".to_string(), 20),
            ],
            150,
        )
        .unwrap();
        assert_eq!(run(&conn, "run_1").unwrap().unwrap().total_items, 2);
        // Re-seeding keeps the frozen manifest.
        seed_items(
            &conn,
            "run_1",
            &[("item_c".to_string(), "c.jpg".to_string(), 30)],
            160,
        )
        .unwrap();
        assert_eq!(items(&conn, "run_1").unwrap().len(), 2);

        // Verifying before copying is impossible.
        assert!(record_verified(&conn, "run_1", "item_a", &"a".repeat(64), 200).is_err());
        record_copied(&conn, "run_1", "item_a", 200).unwrap();
        record_verified(&conn, "run_1", "item_a", &"a".repeat(64), 210).unwrap();
        record_copied(&conn, "run_1", "item_b", 200).unwrap();
        record_item_failure(&conn, "run_1", "item_b", CODE_CHECKSUM_MISMATCH, 220).unwrap();
        refresh_counts(&conn, "run_1", 230).unwrap();

        let counts = run(&conn, "run_1").unwrap().unwrap();
        assert_eq!(counts.copied_items, 1);
        assert_eq!(counts.failed_items, 1);
        assert_eq!(
            item(&conn, "run_1", "item_a")
                .unwrap()
                .unwrap()
                .content_sha256
                .as_deref(),
            Some(&"a".repeat(64)[..])
        );
        // A failed item can be retried, but a verified one must not regress.
        reset_item(&conn, "run_1", "item_b", 240).unwrap();
        assert_eq!(
            item(&conn, "run_1", "item_b").unwrap().unwrap().state,
            ITEM_PENDING
        );
        assert!(reset_item(&conn, "run_1", "item_a", 240).is_err());
    }

    #[test]
    fn a_changed_plan_is_rejected_and_failures_keep_the_message() {
        let conn = init_memory().unwrap();
        open(&conn);
        // Re-opening with a different target is a changed plan, not a new run.
        assert!(open_run(
            &conn,
            "run_1",
            "lib_1",
            "epoch_old",
            "C:/old",
            "E:/elsewhere",
            KIND_MIGRATE,
            110
        )
        .is_err());
        advance_phase(&conn, "run_1", PHASE_PREPARING, PHASE_COPYING, 120).unwrap();
        record_failure(
            &conn,
            "run_1",
            CODE_INSUFFICIENT_SPACE,
            "目标盘剩余空间不足",
            130,
        )
        .unwrap();
        let failed = run(&conn, "run_1").unwrap().unwrap();
        assert_eq!(failed.phase, PHASE_FAILED);
        assert_eq!(failed.error_code.as_deref(), Some(CODE_INSUFFICIENT_SPACE));
        assert_eq!(failed.error_message.as_deref(), Some("目标盘剩余空间不足"));
        assert!(active_run(&conn).unwrap().is_none());
    }
}
