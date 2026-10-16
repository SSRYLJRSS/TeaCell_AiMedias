//! Persisted import preparation plans.
//!
//! An import plan binds a fixed candidate set with per-item fingerprints,
//! preview classification, target collection and the backend-computed rename
//! preview. Scanning/checking never copies a file or writes an asset row; this
//! module only stores the plan so a later submit can detect file changes.

use rusqlite::{Connection, OptionalExtension};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::AppResult;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPlanRecord {
    pub plan_id: String,
    pub principal_id: String,
    pub source_ref: Option<String>,
    pub target_collection: Option<String>,
    pub rename_pattern: Option<String>,
    pub library_id: String,
    pub library_epoch: String,
    pub item_count: i64,
    pub ready_count: i64,
    pub limited_count: i64,
    pub unsupported_count: i64,
    pub total_size: i64,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportPlanItemRecord {
    pub item_id: String,
    pub plan_id: String,
    pub ordinal: i64,
    pub file_ref: Option<String>,
    pub path: String,
    pub display_name: String,
    pub kind: String,
    pub size: i64,
    pub modified_at: i64,
    pub file_identity: String,
    pub content_sha256: Option<String>,
    pub preview_status: String,
    pub preview_message: Option<String>,
    pub rename_preview: String,
    pub excluded: bool,
}

#[derive(Debug, Clone)]
pub struct NewImportPlan {
    pub plan_id: String,
    pub principal_id: String,
    pub source_ref: Option<String>,
    pub target_collection: Option<String>,
    pub rename_pattern: Option<String>,
    pub library_id: String,
    pub library_epoch: String,
    pub item_count: i64,
    pub ready_count: i64,
    pub limited_count: i64,
    pub unsupported_count: i64,
    pub total_size: i64,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone)]
pub struct NewImportPlanItem {
    pub item_id: String,
    pub plan_id: String,
    pub ordinal: i64,
    pub file_ref: Option<String>,
    pub path: String,
    pub display_name: String,
    pub kind: String,
    pub size: i64,
    pub modified_at: i64,
    pub file_identity: String,
    pub content_sha256: Option<String>,
    pub preview_status: String,
    pub preview_message: Option<String>,
    pub rename_preview: String,
    pub excluded: bool,
}

pub(super) fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS import_plans (
            plan_id TEXT PRIMARY KEY,
            principal_id TEXT NOT NULL REFERENCES authorization_principals(principal_id)
                ON DELETE CASCADE,
            source_ref TEXT,
            target_collection TEXT,
            rename_pattern TEXT,
            library_id TEXT NOT NULL,
            library_epoch TEXT NOT NULL,
            item_count INTEGER NOT NULL,
            ready_count INTEGER NOT NULL,
            limited_count INTEGER NOT NULL,
            unsupported_count INTEGER NOT NULL,
            total_size INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_import_plans_principal
            ON import_plans(principal_id, created_at DESC);
        CREATE TABLE IF NOT EXISTS import_plan_items (
            item_id TEXT NOT NULL,
            plan_id TEXT NOT NULL REFERENCES import_plans(plan_id) ON DELETE CASCADE,
            ordinal INTEGER NOT NULL,
            file_ref TEXT,
            path TEXT NOT NULL,
            display_name TEXT NOT NULL,
            kind TEXT NOT NULL,
            size INTEGER NOT NULL,
            modified_at INTEGER NOT NULL,
            file_identity TEXT NOT NULL,
            preview_status TEXT NOT NULL
                CHECK(preview_status IN ('ready','limited','unsupported')),
            preview_message TEXT,
            rename_preview TEXT NOT NULL,
            excluded INTEGER NOT NULL DEFAULT 0 CHECK(excluded IN (0,1)),
            PRIMARY KEY(plan_id, item_id),
            UNIQUE(plan_id, ordinal)
        );
        ",
    )?;
    let has_digest: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('import_plan_items') WHERE name='content_sha256')",
        [], |row| row.get(0),
    )?;
    if !has_digest {
        conn.execute_batch("ALTER TABLE import_plan_items ADD COLUMN content_sha256 TEXT;")?;
    }
    Ok(())
}

pub fn insert_plan(
    conn: &Connection,
    plan: &NewImportPlan,
    items: &[NewImportPlanItem],
) -> AppResult<()> {
    insert_plan_with_cancel(conn, plan, items, &AtomicBool::new(false))
}

pub fn insert_plan_with_cancel(
    conn: &Connection,
    plan: &NewImportPlan,
    items: &[NewImportPlanItem],
    cancel: &AtomicBool,
) -> AppResult<()> {
    insert_plan_checked(conn, plan, items, || {
        if cancel.load(Ordering::Relaxed) {
            Err(crate::error::AppError::cancelled("导入准备已取消"))
        } else {
            Ok(())
        }
    })
}

fn insert_plan_checked(
    conn: &Connection,
    plan: &NewImportPlan,
    items: &[NewImportPlanItem],
    mut check_cancel: impl FnMut() -> AppResult<()>,
) -> AppResult<()> {
    check_cancel()?;
    let transaction = conn.unchecked_transaction()?;
    transaction.execute(
        "INSERT INTO import_plans(
            plan_id,principal_id,source_ref,target_collection,rename_pattern,
            library_id,library_epoch,item_count,ready_count,limited_count,
            unsupported_count,total_size,created_at,expires_at
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",
        rusqlite::params![
            plan.plan_id,
            plan.principal_id,
            plan.source_ref,
            plan.target_collection,
            plan.rename_pattern,
            plan.library_id,
            plan.library_epoch,
            plan.item_count,
            plan.ready_count,
            plan.limited_count,
            plan.unsupported_count,
            plan.total_size,
            plan.created_at,
            plan.expires_at,
        ],
    )?;
    for item in items {
        check_cancel()?;
        transaction.execute(
            "INSERT INTO import_plan_items(
                item_id,plan_id,ordinal,file_ref,path,display_name,kind,size,
                modified_at,file_identity,preview_status,preview_message,
                rename_preview,excluded,content_sha256
             ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
            rusqlite::params![
                item.item_id,
                item.plan_id,
                item.ordinal,
                item.file_ref,
                item.path,
                item.display_name,
                item.kind,
                item.size,
                item.modified_at,
                item.file_identity,
                item.preview_status,
                item.preview_message,
                item.rename_preview,
                item.excluded,
                item.content_sha256,
            ],
        )?;
    }
    check_cancel()?;
    transaction.commit()?;
    Ok(())
}

pub fn plan(conn: &Connection, plan_id: &str) -> AppResult<Option<ImportPlanRecord>> {
    Ok(conn
        .query_row(
            "SELECT plan_id,principal_id,source_ref,target_collection,rename_pattern,
                    library_id,library_epoch,item_count,ready_count,limited_count,
                    unsupported_count,total_size,created_at,expires_at
               FROM import_plans WHERE plan_id=?1",
            [plan_id],
            |row| {
                Ok(ImportPlanRecord {
                    plan_id: row.get(0)?,
                    principal_id: row.get(1)?,
                    source_ref: row.get(2)?,
                    target_collection: row.get(3)?,
                    rename_pattern: row.get(4)?,
                    library_id: row.get(5)?,
                    library_epoch: row.get(6)?,
                    item_count: row.get(7)?,
                    ready_count: row.get(8)?,
                    limited_count: row.get(9)?,
                    unsupported_count: row.get(10)?,
                    total_size: row.get(11)?,
                    created_at: row.get(12)?,
                    expires_at: row.get(13)?,
                })
            },
        )
        .optional()?)
}

pub fn items(conn: &Connection, plan_id: &str) -> AppResult<Vec<ImportPlanItemRecord>> {
    let mut statement = conn.prepare(
        "SELECT item_id,plan_id,ordinal,file_ref,path,display_name,kind,size,
                modified_at,file_identity,preview_status,preview_message,
                rename_preview,excluded,content_sha256
           FROM import_plan_items WHERE plan_id=?1 ORDER BY ordinal",
    )?;
    let rows = statement.query_map([plan_id], |row| {
        Ok(ImportPlanItemRecord {
            item_id: row.get(0)?,
            plan_id: row.get(1)?,
            ordinal: row.get(2)?,
            file_ref: row.get(3)?,
            path: row.get(4)?,
            display_name: row.get(5)?,
            kind: row.get(6)?,
            size: row.get(7)?,
            modified_at: row.get(8)?,
            file_identity: row.get(9)?,
            preview_status: row.get(10)?,
            preview_message: row.get(11)?,
            rename_preview: row.get(12)?,
            excluded: row.get::<_, i64>(13)? != 0,
            content_sha256: row.get(14)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Replace only the session-bound source handles for a fixed import plan.
/// The operation's frozen paths and fingerprints are unchanged; callers must
/// verify those fingerprints against the newly authorized directory first.
pub fn rebind_file_handles(
    conn: &Connection,
    plan_id: &str,
    handles: &[(String, String)],
) -> AppResult<()> {
    for (item_id, file_ref) in handles {
        let changed = conn.execute(
            "UPDATE import_plan_items SET file_ref=?1 WHERE plan_id=?2 AND item_id=?3 AND excluded=0",
            rusqlite::params![file_ref, plan_id, item_id],
        )?;
        if changed != 1 {
            return Err(crate::error::AppError::from(
                crate::contracts::agent::AgentErrorCode::PlanChanged,
            ));
        }
    }
    Ok(())
}

/// Paths from a base plan that the user removed before re-preparing. Excluded
/// items never reappear in the next closed candidate set.
pub fn excluded_paths(
    conn: &Connection,
    plan_id: &str,
    item_ids: &[String],
) -> AppResult<Vec<String>> {
    if item_ids.is_empty() {
        return Ok(Vec::new());
    }
    let mut statement =
        conn.prepare("SELECT path FROM import_plan_items WHERE plan_id=?1 AND item_id=?2")?;
    let mut out = Vec::new();
    for item_id in item_ids {
        if let Some(path) = statement
            .query_row(rusqlite::params![plan_id, item_id], |row| {
                row.get::<_, String>(0)
            })
            .optional()?
        {
            out.push(path);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cancellation_during_plan_insert_rolls_back_header_and_items() {
        let conn = crate::db::init_memory().unwrap();
        crate::db::authorization::ensure_principal(&conn, "principal_gui_host", 1).unwrap();
        let plan = NewImportPlan {
            plan_id: "cancelled_plan".into(),
            principal_id: "principal_gui_host".into(),
            source_ref: None,
            target_collection: None,
            rename_pattern: None,
            library_id: "library_cancelled".into(),
            library_epoch: "epoch_cancelled".into(),
            item_count: 2,
            ready_count: 2,
            limited_count: 0,
            unsupported_count: 0,
            total_size: 2,
            created_at: 1,
            expires_at: 100,
        };
        let items: Vec<_> = (0..2)
            .map(|ordinal| NewImportPlanItem {
                item_id: format!("cancelled_item_{ordinal}"),
                plan_id: plan.plan_id.clone(),
                ordinal,
                file_ref: None,
                path: format!("item{ordinal}.jpg"),
                display_name: format!("item{ordinal}.jpg"),
                kind: "image".into(),
                size: 1,
                modified_at: 1,
                file_identity: "fingerprint".into(),
                content_sha256: Some("a".repeat(64)),
                preview_status: "ready".into(),
                preview_message: None,
                rename_preview: "item.jpg".into(),
                excluded: false,
            })
            .collect();
        for cancel_at in [3, 4] {
            let mut calls = 0;
            let error = insert_plan_checked(&conn, &plan, &items, || {
                calls += 1;
                if calls == cancel_at {
                    Err(crate::error::AppError::cancelled("injected cancellation"))
                } else {
                    Ok(())
                }
            })
            .unwrap_err();
            assert_eq!(error.code(), "CANCELLED");
            assert_eq!(calls, cancel_at);
            let headers: i64 = conn
                .query_row("SELECT count(*) FROM import_plans", [], |row| row.get(0))
                .unwrap();
            let entries: i64 = conn
                .query_row("SELECT count(*) FROM import_plan_items", [], |row| {
                    row.get(0)
                })
                .unwrap();
            assert_eq!((headers, entries), (0, 0));
        }
    }
    #[test]
    fn digest_column_upgrade_is_idempotent_and_keeps_legacy_items_unverified() {
        let conn = crate::db::init_memory().unwrap();
        crate::db::authorization::ensure_principal(&conn, "principal_gui_host", 1).unwrap();
        conn.execute_batch("ALTER TABLE import_plan_items DROP COLUMN content_sha256;
            INSERT INTO import_plans(plan_id,principal_id,library_id,library_epoch,item_count,ready_count,limited_count,unsupported_count,total_size,created_at,expires_at)
            VALUES('legacy_plan','principal_gui_host','library_legacy','epoch_legacy',1,1,0,0,1,1,100);
            INSERT INTO import_plan_items(item_id,plan_id,ordinal,path,display_name,kind,size,modified_at,file_identity,preview_status,rename_preview)
            VALUES('legacy_item','legacy_plan',0,'legacy.jpg','legacy.jpg','image',1,1,'legacy_identity','ready','legacy.jpg');").unwrap();
        migrate_structure(&conn).unwrap();
        migrate_structure(&conn).unwrap();
        let legacy = items(&conn, "legacy_plan").unwrap();
        assert_eq!(legacy.len(), 1);
        assert_eq!(legacy[0].file_identity, "legacy_identity");
        assert_eq!(legacy[0].content_sha256, None);
    }
}
