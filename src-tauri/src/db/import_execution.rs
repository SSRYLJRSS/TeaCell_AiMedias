//! Durable per-item publication intent. File IO stays in the importer.
use crate::error::{AppError, AppResult};
use rusqlite::{Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationIntent {
    pub target_path: String,
    pub content_sha256: String,
    /// Legacy identity for imports written directly to their final path.
    pub created_file_identity: Option<String>,
    pub staging_path: Option<String>,
    /// Stable filesystem-object identity, independent of mutable mtime/ctime.
    pub staging_file_identity: Option<String>,
    pub published_file_identity: Option<String>,
}

pub fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS import_execution_journal (
            task_id TEXT PRIMARY KEY,
            target_path TEXT NOT NULL,
            content_sha256 TEXT NOT NULL,
            staging_path TEXT,
            staging_file_identity TEXT,
            published_file_identity TEXT
        );",
    )?;
    let mut statement = conn.prepare("PRAGMA table_info(import_execution_journal)")?;
    let columns = statement
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    for (name, declaration) in [
        ("created_file_identity", "TEXT"),
        ("staging_path", "TEXT"),
        ("staging_file_identity", "TEXT"),
        ("published_file_identity", "TEXT"),
    ] {
        if !columns.iter().any(|column| column == name) {
            conn.execute_batch(&format!(
                "ALTER TABLE import_execution_journal ADD COLUMN {name} {declaration};"
            ))?;
        }
    }
    Ok(())
}

pub fn record_created_file(conn: &Connection, task: &str, identity: &str) -> AppResult<()> {
    let changed = conn.execute("UPDATE import_execution_journal SET created_file_identity=?1 WHERE task_id=?2 AND created_file_identity IS NULL", rusqlite::params![identity,task])?;
    if changed != 1 {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

/// Unix ctime is part of the legacy identity and changes while the held file is written.
pub fn finish_created_file(
    conn: &Connection,
    task: &str,
    before: &str,
    after: &str,
) -> AppResult<()> {
    let changed = conn.execute("UPDATE import_execution_journal SET created_file_identity=?1 WHERE task_id=?2 AND created_file_identity=?3", rusqlite::params![after,task,before])?;
    if changed != 1 {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

pub fn record_staging_file(conn: &Connection, task: &str, identity: &str) -> AppResult<()> {
    let changed = conn.execute(
        "UPDATE import_execution_journal
            SET staging_file_identity=?1
          WHERE task_id=?2 AND staging_path IS NOT NULL
            AND (staging_file_identity IS NULL OR staging_file_identity=?1)",
        rusqlite::params![identity, task],
    )?;
    if changed != 1 {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

pub fn record_published_file(
    conn: &Connection,
    task: &str,
    staging_identity: &str,
    published_identity: &str,
) -> AppResult<()> {
    let changed = conn.execute(
        "UPDATE import_execution_journal
            SET published_file_identity=?1
          WHERE task_id=?2 AND staging_file_identity=?3
            AND (published_file_identity IS NULL OR published_file_identity=?1)",
        rusqlite::params![published_identity, task, staging_identity],
    )?;
    if changed != 1 {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

pub fn update_unpublished_target(
    conn: &Connection,
    task: &str,
    old_target: &str,
    new_target: &str,
) -> AppResult<()> {
    let changed = conn.execute(
        "UPDATE import_execution_journal SET target_path=?1
          WHERE task_id=?2 AND target_path=?3 AND published_file_identity IS NULL",
        rusqlite::params![new_target, task, old_target],
    )?;
    if changed != 1 {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

pub fn reset_missing_unpublished_stage(
    conn: &Connection,
    task: &str,
    old_path: &str,
    new_path: &str,
) -> AppResult<()> {
    let changed = conn.execute(
        "UPDATE import_execution_journal
            SET staging_path=?1,staging_file_identity=NULL
          WHERE task_id=?2 AND staging_path=?3 AND published_file_identity IS NULL",
        rusqlite::params![new_path, task, old_path],
    )?;
    if changed != 1 {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

pub fn attach_missing_unpublished_stage(
    conn: &Connection,
    task: &str,
    target_path: &str,
    staging_path: &str,
) -> AppResult<()> {
    let changed = conn.execute(
        "UPDATE import_execution_journal SET staging_path=?1
          WHERE task_id=?2 AND target_path=?3 AND staging_path IS NULL
            AND created_file_identity IS NULL",
        rusqlite::params![staging_path, task, target_path],
    )?;
    if changed != 1 {
        return Err(AppError::conflict("OUTCOME_UNKNOWN"));
    }
    Ok(())
}

pub fn intent(conn: &Connection, task: &str) -> AppResult<Option<PublicationIntent>> {
    Ok(conn
        .query_row(
            "SELECT target_path,content_sha256,created_file_identity,staging_path,
                    staging_file_identity,published_file_identity
               FROM import_execution_journal WHERE task_id=?1",
            [task],
            |row| {
                Ok(PublicationIntent {
                    target_path: row.get(0)?,
                    content_sha256: row.get(1)?,
                    created_file_identity: row.get(2)?,
                    staging_path: row.get(3)?,
                    staging_file_identity: row.get(4)?,
                    published_file_identity: row.get(5)?,
                })
            },
        )
        .optional()?)
}

pub fn record_intent(
    conn: &Connection,
    task: &str,
    target_path: &str,
    staging_path: &str,
    hash: &str,
) -> AppResult<()> {
    conn.execute(
        "INSERT INTO import_execution_journal(
            task_id,target_path,content_sha256,staging_path
         ) VALUES(?1,?2,?3,?4)",
        rusqlite::params![task, target_path, hash, staging_path],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_is_idempotent_and_preserves_legacy_and_staged_intents() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE import_execution_journal (
                task_id TEXT PRIMARY KEY,target_path TEXT NOT NULL,content_sha256 TEXT NOT NULL
             );
             INSERT INTO import_execution_journal(task_id,target_path,content_sha256)
             VALUES('legacy','old-target','old-digest');",
        )
        .unwrap();
        migrate_structure(&conn).unwrap();
        record_intent(&conn, "item", "target", "stage", "digest").unwrap();
        migrate_structure(&conn).unwrap();
        assert_eq!(
            intent(&conn, "legacy").unwrap(),
            Some(PublicationIntent {
                target_path: "old-target".into(),
                content_sha256: "old-digest".into(),
                created_file_identity: None,
                staging_path: None,
                staging_file_identity: None,
                published_file_identity: None,
            })
        );
        assert_eq!(
            intent(&conn, "item").unwrap(),
            Some(PublicationIntent {
                target_path: "target".into(),
                content_sha256: "digest".into(),
                created_file_identity: None,
                staging_path: Some("stage".into()),
                staging_file_identity: None,
                published_file_identity: None,
            })
        );
    }
}
