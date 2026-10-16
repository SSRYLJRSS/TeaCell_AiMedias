//! Persistent state for trusted manual resolution of historical assets.

use rusqlite::{Connection, OptionalExtension};
use serde::Serialize;

use crate::db::libraries::{VERIFIED_ASSET_SCOPE, VERIFIED_LOCATION_EVIDENCE};
use crate::error::{AppError, AppResult};

pub fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS legacy_asset_resolutions (
            resolution_id TEXT PRIMARY KEY,
            asset_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
            action TEXT NOT NULL CHECK(action IN ('copy_back','unlink')),
            state TEXT NOT NULL CHECK(state IN (
                'prepared','copying','published','completed','unlinked',
                'outcome_unknown','reopened','expired','failed'
            )),
            library_id TEXT NOT NULL,
            library_epoch TEXT NOT NULL,
            source_path TEXT NOT NULL,
            source_size INTEGER NOT NULL,
            source_modified_at INTEGER NOT NULL,
            source_file_size INTEGER NOT NULL DEFAULT 0,
            source_file_modified_at INTEGER NOT NULL DEFAULT 0,
            source_file_modified_at_ns INTEGER NOT NULL DEFAULT 0,
            source_file_identity TEXT,
            source_sha256 TEXT,
            target_path TEXT,
            created_file_identity TEXT,
            published_file_identity TEXT,
            matching_assets_json TEXT NOT NULL DEFAULT '[]',
            error_code TEXT,
            created_at INTEGER NOT NULL,
            expires_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_legacy_asset_resolution_asset
            ON legacy_asset_resolutions(asset_id,created_at DESC);
        CREATE UNIQUE INDEX IF NOT EXISTS idx_legacy_asset_resolution_active
            ON legacy_asset_resolutions(asset_id)
            WHERE state IN ('prepared','copying','published','outcome_unknown');
        CREATE INDEX IF NOT EXISTS idx_legacy_asset_resolution_target
            ON legacy_asset_resolutions(target_path,state)
            WHERE target_path IS NOT NULL;
        CREATE UNIQUE INDEX IF NOT EXISTS idx_legacy_asset_resolution_active_target
            ON legacy_asset_resolutions(target_path)
            WHERE target_path IS NOT NULL
              AND state IN ('prepared','copying','published','outcome_unknown');
        CREATE TABLE IF NOT EXISTS legacy_asset_resolution_schema (
            version INTEGER PRIMARY KEY
        );
        CREATE INDEX IF NOT EXISTS idx_managed_asset_sha256
            ON managed_asset_locations(content_sha256,asset_id)
            WHERE status='verified';",
    )?;
    // Keep the column upgrades and their recovery marker in one transaction.
    // Older builds could stop after adding the final snapshot column but before
    // quarantining active receipts, so column presence alone cannot prove that
    // the quarantine migration completed.
    let tx = conn.unchecked_transaction()?;
    let has_source_file_size: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('legacy_asset_resolutions') WHERE name='source_file_size')",
        [],
        |row| row.get(0),
    )?;
    let has_source_file_modified_at: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('legacy_asset_resolutions') WHERE name='source_file_modified_at')",
        [],
        |row| row.get(0),
    )?;
    let has_source_file_modified_at_ns: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM pragma_table_info('legacy_asset_resolutions') WHERE name='source_file_modified_at_ns')",
        [],
        |row| row.get(0),
    )?;
    if !has_source_file_size {
        tx.execute_batch(
            "ALTER TABLE legacy_asset_resolutions ADD COLUMN source_file_size INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    if !has_source_file_modified_at {
        tx.execute_batch(
            "ALTER TABLE legacy_asset_resolutions ADD COLUMN source_file_modified_at INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    if !has_source_file_modified_at_ns {
        tx.execute_batch(
            "ALTER TABLE legacy_asset_resolutions ADD COLUMN source_file_modified_at_ns INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    let snapshot_migration_applied: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM legacy_asset_resolution_schema WHERE version=1)",
        [],
        |row| row.get(0),
    )?;
    if !snapshot_migration_applied {
        tx.execute(
            "UPDATE legacy_asset_resolutions SET state='outcome_unknown',error_code='SNAPSHOT_SCHEMA_UPGRADED'
              WHERE state IN ('prepared','copying','published');",
            [],
        )?;
        tx.execute(
            "INSERT INTO legacy_asset_resolution_schema(version) VALUES(1)",
            [],
        )?;
    }
    tx.commit()?;
    Ok(())
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingLegacyAsset {
    pub asset_id: i64,
    pub file_path: String,
    pub file_name: String,
    pub file_size: i64,
    pub mime_type: String,
    pub modified_at: i64,
    pub hash: Option<String>,
    pub status: String,
    pub reason: String,
    pub favorite: i64,
    pub rating: i64,
    pub active_resolution_id: Option<String>,
    pub active_resolution_action: Option<String>,
    pub active_resolution_state: Option<String>,
    pub resolution_error_code: Option<String>,
    pub matching_assets: Vec<HashMatchCandidate>,
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HashMatchCandidate {
    pub asset_id: i64,
    pub file_name: String,
    pub favorite: i64,
    pub rating: i64,
    pub content_sha256: String,
    pub tags: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ResolutionRecord {
    pub resolution_id: String,
    pub asset_id: i64,
    pub action: String,
    pub state: String,
    pub library_id: String,
    pub library_epoch: String,
    pub source_path: String,
    pub source_size: i64,
    pub source_modified_at: i64,
    pub source_file_size: i64,
    pub source_file_modified_at: i64,
    pub source_file_modified_at_ns: i64,
    pub source_file_identity: Option<String>,
    pub source_sha256: Option<String>,
    pub target_path: Option<String>,
    pub created_file_identity: Option<String>,
    pub published_file_identity: Option<String>,
    pub matching_assets_json: String,
    pub error_code: Option<String>,
    pub expires_at: i64,
}

pub struct NewResolution<'a> {
    pub resolution_id: &'a str,
    pub asset_id: i64,
    pub action: &'a str,
    pub library_id: &'a str,
    pub library_epoch: &'a str,
    pub source_path: &'a str,
    pub source_size: i64,
    pub source_modified_at: i64,
    pub source_file_size: i64,
    pub source_file_modified_at: i64,
    pub source_file_modified_at_ns: i64,
    pub source_file_identity: Option<&'a str>,
    pub source_sha256: Option<&'a str>,
    pub target_path: Option<&'a str>,
    pub matching_assets_json: &'a str,
    pub now: i64,
    pub expires_at: i64,
}

const RESOLUTION_COLUMNS: &str = "resolution_id,asset_id,action,state,library_id,library_epoch,
    source_path,source_size,source_modified_at,source_file_size,source_file_modified_at,source_file_modified_at_ns,
    source_file_identity,source_sha256,target_path,created_file_identity,published_file_identity,
    matching_assets_json,error_code,expires_at";

fn resolution_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ResolutionRecord> {
    Ok(ResolutionRecord {
        resolution_id: row.get(0)?,
        asset_id: row.get(1)?,
        action: row.get(2)?,
        state: row.get(3)?,
        library_id: row.get(4)?,
        library_epoch: row.get(5)?,
        source_path: row.get(6)?,
        source_size: row.get(7)?,
        source_modified_at: row.get(8)?,
        source_file_size: row.get(9)?,
        source_file_modified_at: row.get(10)?,
        source_file_modified_at_ns: row.get(11)?,
        source_file_identity: row.get(12)?,
        source_sha256: row.get(13)?,
        target_path: row.get(14)?,
        created_file_identity: row.get(15)?,
        published_file_identity: row.get(16)?,
        matching_assets_json: row.get(17)?,
        error_code: row.get(18)?,
        expires_at: row.get(19)?,
    })
}

fn pending_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingLegacyAsset> {
    let matching_json: String = row.get(15)?;
    let matching_assets = serde_json::from_str(&matching_json).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(15, rusqlite::types::Type::Text, Box::new(error))
    })?;
    Ok(PendingLegacyAsset {
        asset_id: row.get(0)?,
        file_path: row.get(1)?,
        file_name: row.get(2)?,
        file_size: row.get(3)?,
        mime_type: row.get(4)?,
        modified_at: row.get(5)?,
        hash: row.get(6)?,
        status: row.get(7)?,
        reason: row.get(8)?,
        favorite: row.get(9)?,
        rating: row.get(10)?,
        active_resolution_id: row.get(11)?,
        active_resolution_action: row.get(12)?,
        active_resolution_state: row.get(13)?,
        resolution_error_code: row.get(14)?,
        matching_assets,
    })
}

const PENDING_COLUMNS: &str =
    "a.id,a.file_path,a.file_name,a.file_size,a.mime_type,a.modified_at,a.hash,
    COALESCE(ml.status,'unknown'),COALESCE(ml.reason,'ownership_unverified'),a.favorite,a.rating,
    (SELECT lr.resolution_id FROM legacy_asset_resolutions lr
      WHERE lr.asset_id=a.id AND lr.state IN ('prepared','copying','published','outcome_unknown')
      ORDER BY lr.created_at DESC LIMIT 1),
    (SELECT lr.action FROM legacy_asset_resolutions lr
      WHERE lr.asset_id=a.id AND lr.state IN ('prepared','copying','published','outcome_unknown')
      ORDER BY lr.created_at DESC LIMIT 1),
    (SELECT lr.state FROM legacy_asset_resolutions lr
      WHERE lr.asset_id=a.id AND lr.state IN ('prepared','copying','published','outcome_unknown')
      ORDER BY lr.created_at DESC LIMIT 1),
    (SELECT lr.error_code FROM legacy_asset_resolutions lr
      WHERE lr.asset_id=a.id AND lr.state IN ('prepared','copying','published','outcome_unknown')
      ORDER BY lr.created_at DESC LIMIT 1),
    COALESCE((SELECT lr.matching_assets_json FROM legacy_asset_resolutions lr
      WHERE lr.asset_id=a.id AND lr.state IN ('prepared','copying','published','outcome_unknown')
      ORDER BY lr.created_at DESC LIMIT 1),'[]')";

pub fn pending_page(
    conn: &Connection,
    after_asset_id: i64,
    limit: usize,
) -> AppResult<Vec<PendingLegacyAsset>> {
    if after_asset_id < 0 || !(1..=100).contains(&limit) {
        return Err(AppError::invalid_arg("旧记录分页参数无效"));
    }
    let sql = format!(
        "SELECT {PENDING_COLUMNS}
           FROM assets a
           LEFT JOIN managed_asset_locations ml ON ml.asset_id=a.id
          WHERE a.id>?1 AND NOT ({VERIFIED_LOCATION_EVIDENCE})
            AND NOT EXISTS (
                SELECT 1 FROM legacy_asset_resolutions lr
                 WHERE lr.asset_id=a.id AND lr.state='unlinked'
            )
          ORDER BY a.id LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![after_asset_id, limit as i64],
        pending_from_row,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

pub fn pending_asset(conn: &Connection, asset_id: i64) -> AppResult<Option<PendingLegacyAsset>> {
    let sql = format!(
        "SELECT {PENDING_COLUMNS}
           FROM assets a LEFT JOIN managed_asset_locations ml ON ml.asset_id=a.id
          WHERE a.id=?1 AND NOT ({VERIFIED_LOCATION_EVIDENCE})
            AND NOT EXISTS (SELECT 1 FROM legacy_asset_resolutions lr
                             WHERE lr.asset_id=a.id AND lr.state='unlinked')"
    );
    Ok(conn
        .query_row(&sql, [asset_id], pending_from_row)
        .optional()?)
}

pub fn unlinked_page(
    conn: &Connection,
    after_asset_id: i64,
    limit: usize,
) -> AppResult<Vec<PendingLegacyAsset>> {
    if after_asset_id < 0 || !(1..=100).contains(&limit) {
        return Err(AppError::invalid_arg("解除关联记录分页参数无效"));
    }
    let sql = format!(
        "SELECT a.id,a.file_path,a.file_name,a.file_size,a.mime_type,a.modified_at,a.hash,
                COALESCE(ml.status,'unknown'),COALESCE(ml.reason,'ownership_unverified'),
                a.favorite,a.rating,NULL,NULL,NULL,NULL,'[]'
           FROM assets a LEFT JOIN managed_asset_locations ml ON ml.asset_id=a.id
          WHERE a.id>?1 AND NOT ({VERIFIED_LOCATION_EVIDENCE})
            AND EXISTS (SELECT 1 FROM legacy_asset_resolutions lr
                         WHERE lr.asset_id=a.id AND lr.state='unlinked'
                           AND lr.resolution_id=(SELECT latest.resolution_id
                               FROM legacy_asset_resolutions latest
                              WHERE latest.asset_id=a.id
                              ORDER BY latest.created_at DESC LIMIT 1))
          ORDER BY a.id LIMIT ?2"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(
        rusqlite::params![after_asset_id, limit as i64],
        pending_from_row,
    )?;
    rows.collect::<Result<Vec<_>, _>>().map_err(AppError::from)
}

pub fn exact_hash_candidates(
    conn: &Connection,
    content_sha256: &str,
) -> AppResult<Vec<HashMatchCandidate>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT a.id,a.file_name,a.favorite,a.rating,ml.content_sha256
           FROM assets a JOIN managed_asset_locations ml ON ml.asset_id=a.id
          WHERE ml.content_sha256=?1 AND a.deleted_at IS NULL AND {VERIFIED_ASSET_SCOPE}
          ORDER BY a.id"
    ))?;
    let rows = stmt.query_map([content_sha256], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut candidates = Vec::new();
    for row in rows {
        let (asset_id, file_name, favorite, rating, content_sha256) = row?;
        let mut tags_stmt = conn.prepare(
            "SELECT t.name FROM asset_tags at JOIN tags t ON t.id=at.tag_id
              WHERE at.asset_id=?1 ORDER BY t.facet_key,t.name,t.id",
        )?;
        let tags = tags_stmt
            .query_map([asset_id], |tag| tag.get(0))?
            .collect::<Result<Vec<String>, _>>()?;
        candidates.push(HashMatchCandidate {
            asset_id,
            file_name,
            favorite,
            rating,
            content_sha256,
            tags,
        });
    }
    Ok(candidates)
}

pub fn insert_prepared(conn: &Connection, input: &NewResolution<'_>) -> AppResult<()> {
    let inserted = conn.execute(
        &format!(
            "INSERT INTO legacy_asset_resolutions(
                resolution_id,asset_id,action,state,library_id,library_epoch,source_path,
                source_size,source_modified_at,source_file_size,source_file_modified_at,source_file_modified_at_ns,
                source_file_identity,source_sha256,target_path,
                matching_assets_json,created_at,expires_at,updated_at)
             SELECT ?1,?2,?3,'prepared',?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?16
              WHERE EXISTS(SELECT 1 FROM assets a
                            JOIN managed_asset_locations ml ON ml.asset_id=a.id
                           WHERE a.id=?2 AND a.file_path=?6 AND a.file_size=?7
                             AND a.modified_at=?8 AND ml.status='external_pending')
                AND NOT EXISTS(
                    SELECT 1 FROM assets a WHERE a.id=?2 AND {VERIFIED_LOCATION_EVIDENCE}
                )",
        ),
        rusqlite::params![
            input.resolution_id,
            input.asset_id,
            input.action,
            input.library_id,
            input.library_epoch,
            input.source_path,
            input.source_size,
            input.source_modified_at,
            input.source_file_size,
            input.source_file_modified_at,
            input.source_file_modified_at_ns,
            input.source_file_identity,
            input.source_sha256,
            input.target_path,
            input.matching_assets_json,
            input.now,
            input.expires_at,
        ],
    )?;
    if inserted != 1 {
        return Err(AppError::conflict("旧记录范围已变化，请刷新后重试"));
    }
    Ok(())
}

pub fn expire_prepared(conn: &Connection, now: i64) -> AppResult<usize> {
    Ok(conn.execute(
        "UPDATE legacy_asset_resolutions SET state='expired',updated_at=?1
          WHERE state='prepared' AND expires_at<=?1",
        [now],
    )?)
}

pub fn expire_resolution(conn: &Connection, id: &str, now: i64) -> AppResult<bool> {
    Ok(conn.execute(
        "UPDATE legacy_asset_resolutions SET state='expired',updated_at=?1
          WHERE resolution_id=?2 AND state='prepared' AND expires_at<=?1",
        rusqlite::params![now, id],
    )? == 1)
}

pub fn active_target_exists(conn: &Connection, target_path: &str) -> AppResult<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM legacy_asset_resolutions
          WHERE target_path=?1 AND state IN ('prepared','copying','published','outcome_unknown'))",
        [target_path],
        |row| row.get(0),
    )?)
}

pub fn get_resolution(conn: &Connection, id: &str) -> AppResult<Option<ResolutionRecord>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT {RESOLUTION_COLUMNS} FROM legacy_asset_resolutions WHERE resolution_id=?1"
            ),
            [id],
            resolution_from_row,
        )
        .optional()?)
}

pub fn mark_copy_started(
    conn: &Connection,
    id: &str,
    target_path: &str,
    now: i64,
) -> AppResult<bool> {
    Ok(conn.execute(
        "UPDATE legacy_asset_resolutions
            SET state='copying',target_path=?1,updated_at=?2
          WHERE resolution_id=?3 AND action='copy_back' AND state='prepared'",
        rusqlite::params![target_path, now, id],
    )? == 1)
}

pub fn set_created_identity(
    conn: &Connection,
    id: &str,
    object_identity: &str,
    now: i64,
) -> AppResult<bool> {
    Ok(conn.execute(
        "UPDATE legacy_asset_resolutions SET created_file_identity=?1,updated_at=?2
          WHERE resolution_id=?3 AND action='copy_back' AND state='copying'
            AND created_file_identity IS NULL",
        rusqlite::params![object_identity, now, id],
    )? == 1)
}

pub fn mark_published(
    conn: &Connection,
    id: &str,
    created_identity: &str,
    published_identity: &str,
    now: i64,
) -> AppResult<bool> {
    Ok(conn.execute(
        "UPDATE legacy_asset_resolutions
            SET state='published',created_file_identity=COALESCE(created_file_identity,?1),
                published_file_identity=?2,updated_at=?3
          WHERE resolution_id=?4 AND state='copying'
            AND (created_file_identity=?1 OR created_file_identity IS NULL)",
        rusqlite::params![created_identity, published_identity, now, id],
    )? == 1)
}

pub fn mark_unknown(conn: &Connection, id: &str, error_code: &str, now: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE legacy_asset_resolutions SET state='outcome_unknown',error_code=?1,updated_at=?2
          WHERE resolution_id=?3 AND state IN ('prepared','copying','published','outcome_unknown')",
        rusqlite::params![error_code, now, id],
    )?;
    Ok(())
}

pub fn mark_copy_unknown(conn: &Connection, id: &str, error_code: &str, now: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE legacy_asset_resolutions SET state='outcome_unknown',error_code=?1,updated_at=?2
          WHERE resolution_id=?3 AND state='copying'",
        rusqlite::params![error_code, now, id],
    )?;
    Ok(())
}

pub fn mark_failed(conn: &Connection, id: &str, error_code: &str, now: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE legacy_asset_resolutions SET state='failed',error_code=?1,updated_at=?2
          WHERE resolution_id=?3 AND (state='prepared'
            OR (state='copying' AND created_file_identity IS NULL))",
        rusqlite::params![error_code, now, id],
    )?;
    Ok(())
}

pub fn cancel_prepared(conn: &Connection, id: &str, now: i64) -> AppResult<bool> {
    Ok(conn.execute(
        "UPDATE legacy_asset_resolutions SET state='expired',updated_at=?1
          WHERE resolution_id=?2 AND state='prepared'",
        rusqlite::params![now, id],
    )? == 1)
}

pub fn finish_unlink(conn: &Connection, id: &str, now: i64) -> AppResult<bool> {
    Ok(conn.execute(
        "UPDATE legacy_asset_resolutions
            SET state='unlinked',updated_at=?1
          WHERE resolution_id=?2 AND action='unlink' AND state='prepared'
            AND EXISTS(SELECT 1 FROM assets a JOIN managed_asset_locations ml ON ml.asset_id=a.id
                        WHERE a.id=legacy_asset_resolutions.asset_id
                          AND a.file_path=legacy_asset_resolutions.source_path
                          AND a.file_size=legacy_asset_resolutions.source_size
                          AND a.modified_at=legacy_asset_resolutions.source_modified_at
                          AND ml.status='external_pending')",
        rusqlite::params![now, id],
    )? == 1)
}

pub fn finish_copy(conn: &Connection, id: &str, now: i64) -> AppResult<bool> {
    Ok(conn.execute(
        "UPDATE legacy_asset_resolutions SET state='completed',updated_at=?1,error_code=NULL
          WHERE resolution_id=?2 AND action='copy_back' AND state='published'",
        rusqlite::params![now, id],
    )? == 1)
}

pub fn update_asset_after_copy(
    conn: &Connection,
    record: &ResolutionRecord,
    target_path: &str,
    file_name: &str,
    file_size: i64,
    modified_at: i64,
    compact_hash: &str,
) -> AppResult<bool> {
    Ok(conn.execute(
        "UPDATE assets SET file_path=?1,file_name=?2,file_size=?3,modified_at=?4,hash=?5
          WHERE id=?6 AND file_path=?7 AND file_size=?8 AND modified_at=?9
            AND NOT EXISTS(SELECT 1 FROM legacy_asset_resolutions lr
                            WHERE lr.asset_id=?6 AND lr.state='unlinked')",
        rusqlite::params![
            target_path,
            file_name,
            file_size,
            modified_at,
            compact_hash,
            record.asset_id,
            record.source_path,
            record.source_size,
            record.source_modified_at,
        ],
    )? == 1)
}

pub fn reopen_unlinked(conn: &Connection, asset_id: i64, now: i64) -> AppResult<bool> {
    Ok(conn.execute(
        "UPDATE legacy_asset_resolutions SET state='reopened',updated_at=?1
          WHERE resolution_id=(SELECT resolution_id FROM legacy_asset_resolutions
                                WHERE asset_id=?2 AND state='unlinked'
                                ORDER BY created_at DESC LIMIT 1)",
        rusqlite::params![now, asset_id],
    )? == 1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    #[test]
    fn resolution_migration_is_idempotent_and_pending_rows_are_preserved_until_unlink() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        let id = crate::db::assets::insert(
            &conn,
            "C:/outside/one.jpg",
            "one.jpg",
            ".jpg",
            4,
            "image/jpeg",
            10,
        )
        .unwrap();
        let before = pending_asset(&conn, id).unwrap().unwrap();
        assert_eq!(before.file_name, "one.jpg");
        conn.execute(
            "UPDATE managed_asset_locations SET status='external_pending',reason='outside_managed_root' WHERE asset_id=?1",
            [id],
        )
        .unwrap();
        let library = crate::db::libraries::current(&conn).unwrap().unwrap();
        insert_prepared(
            &conn,
            &NewResolution {
                resolution_id: "legacy_unlink_fixture_0001",
                asset_id: id,
                action: "unlink",
                library_id: &library.library_id,
                library_epoch: &library.library_epoch,
                source_path: &before.file_path,
                source_size: before.file_size,
                source_modified_at: before.modified_at,
                source_file_size: before.file_size,
                source_file_modified_at: before.modified_at,
                source_file_modified_at_ns: before.modified_at * 1_000_000,
                source_file_identity: None,
                source_sha256: None,
                target_path: None,
                matching_assets_json: "[]",
                now: 1,
                expires_at: 100,
            },
        )
        .unwrap();
        assert!(finish_unlink(&conn, "legacy_unlink_fixture_0001", 2).unwrap());
        assert!(pending_asset(&conn, id).unwrap().is_none());
        assert!(reopen_unlinked(&conn, id, 3).unwrap());
        assert!(pending_asset(&conn, id).unwrap().is_some());
        migrate_structure(&conn).unwrap();
    }

    #[test]
    fn old_resolution_schema_adds_snapshots_and_quarantines_active_receipts_idempotently() {
        let conn = init_memory().unwrap();
        conn.execute_batch(
            "DROP TABLE legacy_asset_resolutions;
             DROP TABLE IF EXISTS legacy_asset_resolution_schema;
             CREATE TABLE legacy_asset_resolutions (
                resolution_id TEXT PRIMARY KEY,
                asset_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                action TEXT NOT NULL CHECK(action IN ('copy_back','unlink')),
                state TEXT NOT NULL CHECK(state IN (
                    'prepared','copying','published','completed','unlinked',
                    'outcome_unknown','reopened','expired','failed'
                )),
                library_id TEXT NOT NULL,
                library_epoch TEXT NOT NULL,
                source_path TEXT NOT NULL,
                source_size INTEGER NOT NULL,
                source_modified_at INTEGER NOT NULL,
                source_file_identity TEXT,
                source_sha256 TEXT,
                target_path TEXT,
                created_file_identity TEXT,
                published_file_identity TEXT,
                matching_assets_json TEXT NOT NULL DEFAULT '[]',
                error_code TEXT,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        let library = crate::db::libraries::current(&conn).unwrap().unwrap();
        let states = [
            "prepared",
            "copying",
            "published",
            "completed",
            "outcome_unknown",
        ];
        for (index, state) in states.into_iter().enumerate() {
            let asset_id = crate::db::assets::insert(
                &conn,
                &format!("C:/outside/{index}.jpg"),
                &format!("{index}.jpg"),
                ".jpg",
                4,
                "image/jpeg",
                10,
            )
            .unwrap();
            conn.execute(
                "INSERT INTO legacy_asset_resolutions(
                    resolution_id,asset_id,action,state,library_id,library_epoch,
                    source_path,source_size,source_modified_at,source_file_identity,
                    source_sha256,target_path,created_file_identity,published_file_identity,
                    matching_assets_json,error_code,created_at,expires_at,updated_at
                 ) VALUES(?1,?2,'copy_back',?3,?4,?5,?6,4,10,'source-id','source-hash',
                          ?7,'target-id',NULL,'[]',NULL,1,100,1)",
                rusqlite::params![
                    format!("legacy_migration_fixture_{index}"),
                    asset_id,
                    state,
                    library.library_id,
                    library.library_epoch,
                    format!("C:/outside/{index}.jpg"),
                    format!("C:/library/{index}.jpg"),
                ],
            )
            .unwrap();
        }

        migrate_structure(&conn).unwrap();
        migrate_structure(&conn).unwrap();

        let mut stmt = conn
            .prepare(
                "SELECT state,error_code,source_file_size,source_file_modified_at,
                        source_file_modified_at_ns
                   FROM legacy_asset_resolutions ORDER BY resolution_id",
            )
            .unwrap();
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(rows.len(), 5);
        for (state, error_code, file_size, modified_at, modified_at_ns) in &rows[..3] {
            assert_eq!(state, "outcome_unknown");
            assert_eq!(error_code.as_deref(), Some("SNAPSHOT_SCHEMA_UPGRADED"));
            assert_eq!(*file_size, 0);
            assert_eq!(*modified_at, 0);
            assert_eq!(*modified_at_ns, 0);
        }
        assert!(rows.iter().any(|row| row.0 == "completed"));
        assert!(rows
            .iter()
            .any(|row| row.0 == "outcome_unknown" && row.1.is_none()));
    }

    #[test]
    fn interrupted_snapshot_migration_quarantines_receipts_when_all_columns_already_exist() {
        let conn = init_memory().unwrap();
        conn.execute_batch(
            "DROP TABLE legacy_asset_resolutions;
             DROP TABLE IF EXISTS legacy_asset_resolution_schema;
             CREATE TABLE legacy_asset_resolutions (
                resolution_id TEXT PRIMARY KEY,
                asset_id INTEGER NOT NULL REFERENCES assets(id) ON DELETE CASCADE,
                action TEXT NOT NULL,
                state TEXT NOT NULL,
                library_id TEXT NOT NULL,
                library_epoch TEXT NOT NULL,
                source_path TEXT NOT NULL,
                source_size INTEGER NOT NULL,
                source_modified_at INTEGER NOT NULL,
                source_file_size INTEGER NOT NULL DEFAULT 0,
                source_file_modified_at INTEGER NOT NULL DEFAULT 0,
                source_file_modified_at_ns INTEGER NOT NULL DEFAULT 0,
                source_file_identity TEXT,
                source_sha256 TEXT,
                target_path TEXT,
                created_file_identity TEXT,
                published_file_identity TEXT,
                matching_assets_json TEXT NOT NULL DEFAULT '[]',
                error_code TEXT,
                created_at INTEGER NOT NULL,
                expires_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
             );",
        )
        .unwrap();
        let asset_id = crate::db::assets::insert(
            &conn,
            "C:/outside/interrupted.jpg",
            "interrupted.jpg",
            ".jpg",
            4,
            "image/jpeg",
            10,
        )
        .unwrap();
        let library = crate::db::libraries::current(&conn).unwrap().unwrap();
        conn.execute(
            "INSERT INTO legacy_asset_resolutions(
                resolution_id,asset_id,action,state,library_id,library_epoch,
                source_path,source_size,source_modified_at,created_at,expires_at,updated_at
             ) VALUES('interrupted_snapshot_upgrade',?1,'copy_back','prepared',?2,?3,
                      'C:/outside/interrupted.jpg',4,10,1,100,1)",
            rusqlite::params![asset_id, library.library_id, library.library_epoch],
        )
        .unwrap();

        migrate_structure(&conn).unwrap();

        let (state, error_code) = conn
            .query_row(
                "SELECT state,error_code FROM legacy_asset_resolutions
                  WHERE resolution_id='interrupted_snapshot_upgrade'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?)),
            )
            .unwrap();
        assert_eq!(state, "outcome_unknown");
        assert_eq!(error_code.as_deref(), Some("SNAPSHOT_SCHEMA_UPGRADED"));
        let marker_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM legacy_asset_resolution_schema WHERE version=1",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(marker_count, 1);
    }
}
