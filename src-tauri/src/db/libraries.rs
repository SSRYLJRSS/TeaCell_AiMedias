//! Managed-library protection data. No filesystem IO or ownership inference occurs here.
use rusqlite::{Connection, OptionalExtension};

use crate::error::{AppError, AppResult};

/// Fixed alias `a` is shared by asset queries. Apply before pagination/counting.
pub const VERIFIED_ASSET_SCOPE: &str = "EXISTS (SELECT 1 FROM managed_asset_locations ml JOIN libraries l ON l.library_id = ml.library_id WHERE ml.asset_id = a.id AND l.singleton = 1 AND l.health = 'ready' AND ml.status = 'verified' AND ml.library_epoch = l.library_epoch AND ml.observed_path = a.file_path AND ml.observed_modified_at = a.modified_at)";

/// Persisted ownership evidence remains a verified relation while the root is
/// offline. This prevents offline libraries from projecting every managed row
/// into the historical legacy queue.
pub const VERIFIED_LOCATION_EVIDENCE: &str = "EXISTS (SELECT 1 FROM managed_asset_locations ml JOIN libraries l ON l.library_id = ml.library_id WHERE ml.asset_id = a.id AND l.singleton = 1 AND ml.status = 'verified' AND ml.library_epoch = l.library_epoch AND ml.observed_path = a.file_path AND ml.observed_modified_at = a.modified_at)";

/// Reject ordinary mutation requests that still carry IDs from an old,
/// unverified, or otherwise no-longer-managed asset scope.
pub fn ensure_managed_assets(conn: &Connection, asset_ids: &[i64]) -> AppResult<()> {
    let unique = asset_ids
        .iter()
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    if unique.is_empty() {
        return Ok(());
    }
    let placeholders = unique.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let visible: i64 = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM assets a
              WHERE a.id IN ({placeholders}) AND {VERIFIED_ASSET_SCOPE}"
        ),
        rusqlite::params_from_iter(unique.iter()),
        |row| row.get(0),
    )?;
    if visible == unique.len() as i64 {
        Ok(())
    } else {
        Err(AppError::conflict("素材范围已变化，请刷新素材库后重新操作"))
    }
}

pub(super) fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS libraries (
            singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
            library_id TEXT NOT NULL UNIQUE,
            library_epoch TEXT NOT NULL,
            root_path TEXT NOT NULL,
            root_identity TEXT,
            health TEXT NOT NULL CHECK(health IN ('unconfigured','unverified','ready','offline','wrong_root')),
            marker_state TEXT NOT NULL DEFAULT 'untrusted'
                CHECK(marker_state IN ('untrusted','initial_candidate','present')),
            CHECK(health != 'ready' OR (length(root_path) > 0 AND coalesce(length(root_identity),0) > 0))
        );
        CREATE TABLE IF NOT EXISTS managed_asset_locations (
            asset_id INTEGER PRIMARY KEY REFERENCES assets(id) ON DELETE CASCADE,
            location_revision TEXT NOT NULL DEFAULT (lower(hex(randomblob(16)))),
            library_id TEXT REFERENCES libraries(library_id),
            library_epoch TEXT,
            relative_path TEXT,
            observed_path TEXT NOT NULL,
            observed_modified_at INTEGER NOT NULL,
            file_identity TEXT,
            content_sha256 TEXT,
            ownership_receipt TEXT,
            status TEXT NOT NULL DEFAULT 'unknown' CHECK(status IN ('unknown','external_pending','missing','verified')),
            reason TEXT NOT NULL DEFAULT 'verification_required',
            CHECK(status != 'verified' OR (
                library_id IS NOT NULL AND library_epoch IS NOT NULL AND
                coalesce(length(relative_path),0) > 0 AND coalesce(length(file_identity),0) > 0 AND
                coalesce(length(content_sha256),0) = 64 AND coalesce(length(ownership_receipt),0) > 0))
        );
        CREATE INDEX IF NOT EXISTS idx_managed_asset_status ON managed_asset_locations(status,asset_id);
        CREATE TRIGGER IF NOT EXISTS trg_library_locator_requires_new_epoch
            BEFORE UPDATE OF root_path,root_identity ON libraries
            WHEN (old.root_path IS NOT new.root_path OR old.root_identity IS NOT new.root_identity)
                AND old.library_epoch = new.library_epoch BEGIN
            SELECT RAISE(ABORT,'library_locator_requires_new_epoch');
        END;
        CREATE TRIGGER IF NOT EXISTS trg_managed_asset_insert AFTER INSERT ON assets BEGIN
            INSERT INTO managed_asset_locations(asset_id,observed_path,observed_modified_at)
                VALUES(new.id,new.file_path,new.modified_at);
        END;
        CREATE TRIGGER IF NOT EXISTS trg_managed_asset_location_changed
            AFTER UPDATE OF file_path,modified_at ON assets
            WHEN old.file_path != new.file_path OR old.modified_at != new.modified_at BEGIN
            INSERT INTO managed_asset_locations(asset_id,observed_path,observed_modified_at,reason)
                VALUES(new.id,new.file_path,new.modified_at,'asset_changed')
            ON CONFLICT(asset_id) DO UPDATE SET status='unknown',reason='asset_changed',
                observed_path=excluded.observed_path,observed_modified_at=excluded.observed_modified_at,
                location_revision=lower(hex(randomblob(16))),
                library_id=NULL,library_epoch=NULL,relative_path=NULL,
                file_identity=NULL,content_sha256=NULL,ownership_receipt=NULL;
        END;",
    )?;
    let has_marker_state: bool = conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM pragma_table_info('libraries') WHERE name='marker_state'
         )",
        [],
        |row| row.get(0),
    )?;
    if !has_marker_state {
        conn.execute(
            "ALTER TABLE libraries
             ADD COLUMN marker_state TEXT NOT NULL DEFAULT 'untrusted'",
            [],
        )?;
    }
    // Existing ready roots were already inspected by the old locator flow.
    // Do not infer marker creation permission for offline/restored candidates.
    conn.execute(
        "UPDATE libraries
            SET marker_state='present'
          WHERE health='ready' AND root_identity IS NOT NULL
            AND marker_state='untrusted'",
        [],
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LibrarySnapshot {
    pub library_id: String,
    pub library_epoch: String,
    pub root_path: String,
    pub root_identity: Option<String>,
    pub health: LibraryHealth,
    pub marker_state: MarkerState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerState {
    /// Do not create a marker; an existing marker must be verified.
    Untrusted,
    /// One-time compatibility state for an existing pre-V27 root candidate.
    InitialCandidate,
    /// The root marker was verified in a prior startup.
    Present,
}

impl rusqlite::types::FromSql for MarkerState {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        match value.as_str()? {
            "untrusted" => Ok(Self::Untrusted),
            "initial_candidate" => Ok(Self::InitialCandidate),
            "present" => Ok(Self::Present),
            _ => Err(rusqlite::types::FromSqlError::InvalidType),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LibraryHealth {
    Unconfigured,
    Unverified,
    Ready,
    Offline,
    WrongRoot,
}

impl rusqlite::types::FromSql for LibraryHealth {
    fn column_result(value: rusqlite::types::ValueRef<'_>) -> rusqlite::types::FromSqlResult<Self> {
        match value.as_str()? {
            "unconfigured" => Ok(Self::Unconfigured),
            "unverified" => Ok(Self::Unverified),
            "ready" => Ok(Self::Ready),
            "offline" => Ok(Self::Offline),
            "wrong_root" => Ok(Self::WrongRoot),
            _ => Err(rusqlite::types::FromSqlError::InvalidType),
        }
    }
}

pub fn current(conn: &Connection) -> AppResult<Option<LibrarySnapshot>> {
    Ok(conn
        .query_row(
            "SELECT library_id,library_epoch,root_path,root_identity,health,marker_state
           FROM libraries WHERE singleton=1",
            [],
            |r| {
                Ok(LibrarySnapshot {
                    library_id: r.get(0)?,
                    library_epoch: r.get(1)?,
                    root_path: r.get(2)?,
                    root_identity: r.get(3)?,
                    health: r.get(4)?,
                    marker_state: r.get(5)?,
                })
            },
        )
        .optional()?)
}

/// One-way legacy root candidate adoption; does not certify the directory or files.
/// Ordinary startup preserves epoch. Restore installation must explicitly rotate it.
pub fn initialize_candidate(conn: &Connection, legacy_root: &str) -> AppResult<LibrarySnapshot> {
    conn.execute(
        "INSERT OR IGNORE INTO libraries(
            singleton,library_id,library_epoch,root_path,health,marker_state
         )
         VALUES(1,?1,?2,?3,?4,?5)",
        rusqlite::params![
            uuid::Uuid::new_v4().simple().to_string(),
            uuid::Uuid::new_v4().simple().to_string(),
            legacy_root,
            if legacy_root.trim().is_empty() {
                "unconfigured"
            } else {
                "unverified"
            },
            if legacy_root.trim().is_empty() {
                "untrusted"
            } else {
                "initial_candidate"
            },
        ],
    )?;
    if !legacy_root.trim().is_empty() {
        conn.execute(
            "UPDATE libraries
                SET root_path=?1,health='unverified',library_epoch=?2,
                    marker_state='initial_candidate'
              WHERE singleton=1 AND trim(root_path)=''",
            rusqlite::params![
                legacy_root.trim(),
                uuid::Uuid::new_v4().simple().to_string()
            ],
        )?;
    }
    current(conn)?.ok_or_else(|| AppError::internal("总仓保护状态缺失"))
}

/// Restore-only installation boundary; invoke before exposing the replacement connection.
/// Retains historical evidence for audit but makes it unusable by rotating the epoch.
/// No filesystem ownership/health is inferred from a backup's old records.
pub fn invalidate_restored_locator(conn: &Connection) -> AppResult<LibrarySnapshot> {
    let previous = match current(conn)? {
        Some(snapshot) => snapshot,
        None => initialize_candidate(conn, &super::settings::get_settings(conn)?.library_root)?,
    };
    let changed = conn.execute(
        "UPDATE libraries
            SET library_epoch=?1,root_identity=NULL,health=?2,marker_state='untrusted'
         WHERE singleton=1 AND library_id=?3 AND library_epoch=?4",
        rusqlite::params![
            uuid::Uuid::new_v4().simple().to_string(),
            if previous.root_path.trim().is_empty() {
                "unconfigured"
            } else {
                "unverified"
            },
            previous.library_id,
            previous.library_epoch
        ],
    )?;
    if changed != 1 {
        return Err(AppError::conflict("恢复期间总仓定位发生变化"));
    }
    current(conn)?.ok_or_else(|| AppError::internal("恢复后总仓保护状态缺失"))
}

/// Persist a root identity only with a new epoch. The trigger forbids replacing
/// root identity in place, and CAS prevents a stale startup inspection from
/// overwriting a concurrent maintenance transition.
pub fn mark_root_ready(
    conn: &Connection,
    expected: &LibrarySnapshot,
    canonical_root: &str,
    root_identity: &str,
) -> AppResult<LibrarySnapshot> {
    if canonical_root.trim().is_empty() || root_identity.trim().is_empty() {
        return Err(AppError::invalid_arg("总仓身份不能为空"));
    }
    if expected.root_path == canonical_root
        && expected.root_identity.as_deref() == Some(root_identity)
        && expected.health != LibraryHealth::Ready
    {
        let changed = conn.execute(
            "UPDATE libraries SET health='ready',marker_state='present'
              WHERE singleton=1 AND library_id=?1 AND library_epoch=?2
                AND root_path=?3 AND root_identity IS ?4",
            rusqlite::params![
                expected.library_id,
                expected.library_epoch,
                expected.root_path,
                expected.root_identity,
            ],
        )?;
        if changed != 1 {
            return Err(AppError::conflict("总仓定位在启动核验期间发生变化"));
        }
        return current(conn)?.ok_or_else(|| AppError::internal("总仓保护状态缺失"));
    }
    let changed = conn.execute(
        "UPDATE libraries
            SET root_path=?1,root_identity=?2,health='ready',library_epoch=?3,
                marker_state='present'
          WHERE singleton=1 AND library_id=?4 AND library_epoch=?5
            AND root_path=?6 AND root_identity IS ?7",
        rusqlite::params![
            canonical_root,
            root_identity,
            uuid::Uuid::new_v4().simple().to_string(),
            expected.library_id,
            expected.library_epoch,
            expected.root_path,
            expected.root_identity,
        ],
    )?;
    if changed != 1 {
        return Err(AppError::conflict("总仓定位在启动核验期间发生变化"));
    }
    current(conn)?.ok_or_else(|| AppError::internal("总仓保护状态缺失"))
}

/// Change only the health projection for a root that is unavailable or whose
/// marker does not match. Path and identity remain untouched.
pub fn set_health(
    conn: &Connection,
    expected: &LibrarySnapshot,
    health: LibraryHealth,
) -> AppResult<LibrarySnapshot> {
    if matches!(health, LibraryHealth::Ready) {
        return Err(AppError::invalid_arg("ready 必须与根身份一起提交"));
    }
    let changed = conn.execute(
        "UPDATE libraries SET health=?1
          WHERE singleton=1 AND library_id=?2 AND library_epoch=?3
            AND root_path=?4 AND root_identity IS ?5",
        rusqlite::params![
            health.as_str(),
            expected.library_id,
            expected.library_epoch,
            expected.root_path,
            expected.root_identity,
        ],
    )?;
    if changed != 1 {
        return Err(AppError::conflict("总仓定位在启动核验期间发生变化"));
    }
    current(conn)?.ok_or_else(|| AppError::internal("总仓保护状态缺失"))
}

impl LibraryHealth {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unconfigured => "unconfigured",
            Self::Unverified => "unverified",
            Self::Ready => "ready",
            Self::Offline => "offline",
            Self::WrongRoot => "wrong_root",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationCandidate {
    pub asset_id: i64,
    pub observed_path: String,
    pub observed_modified_at: i64,
    pub location_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedLocation {
    pub relative_path: String,
    pub file_identity: String,
    pub content_sha256: String,
    pub observed_modified_at: i64,
}

/// Read-only location evidence for a managed asset in the current generation.
/// The caller still revalidates the opened file identity before using it.
pub fn verified_location(conn: &Connection, asset_id: i64) -> AppResult<Option<VerifiedLocation>> {
    Ok(conn
        .query_row(
            &format!(
                "SELECT ml.relative_path,ml.file_identity,ml.content_sha256,
                        ml.observed_modified_at
                   FROM assets a
                   JOIN managed_asset_locations ml ON ml.asset_id=a.id
                  WHERE a.id=?1 AND {VERIFIED_ASSET_SCOPE}"
            ),
            [asset_id],
            |row| {
                Ok(VerifiedLocation {
                    relative_path: row.get(0)?,
                    file_identity: row.get(1)?,
                    content_sha256: row.get(2)?,
                    observed_modified_at: row.get(3)?,
                })
            },
        )
        .optional()?)
}

#[derive(Debug, Clone, Copy)]
pub enum PendingClassification {
    Unknown,
    ExternalPending,
    Missing,
}

/// CAS after lock-free inspection. Offline/wrong-root never classifies each asset as external.
/// Verified ownership is deliberately not accepted by this pending-classification interface.
pub fn classify_pending(
    conn: &Connection,
    library: &LibrarySnapshot,
    candidate: &VerificationCandidate,
    classification: PendingClassification,
) -> AppResult<bool> {
    let (status, reason) = match classification {
        PendingClassification::Unknown => ("unknown", "ownership_unverified"),
        PendingClassification::ExternalPending => ("external_pending", "outside_managed_root"),
        PendingClassification::Missing => ("missing", "file_missing"),
    };
    let changed=conn.execute(
        &format!("INSERT INTO managed_asset_locations(asset_id,observed_path,observed_modified_at,status,reason)
         SELECT ?3,?4,?5,?1,?2
         WHERE EXISTS(SELECT 1 FROM assets a WHERE a.id=?3 AND a.file_path=?4 AND a.modified_at=?5)
         AND (SELECT location_revision FROM managed_asset_locations WHERE asset_id=?3) IS ?10
         AND NOT EXISTS(SELECT 1 FROM assets a WHERE a.id=?3 AND {VERIFIED_ASSET_SCOPE})
         AND EXISTS(SELECT 1 FROM libraries l WHERE l.singleton=1 AND l.library_id=?6
            AND l.library_epoch=?7 AND l.root_path=?8 AND l.root_identity IS ?9 AND l.health='ready')
         ON CONFLICT(asset_id) DO UPDATE SET status=excluded.status,reason=excluded.reason,
            location_revision=lower(hex(randomblob(16))),library_id=NULL,library_epoch=NULL,
            relative_path=NULL,file_identity=NULL,content_sha256=NULL,ownership_receipt=NULL"),
        rusqlite::params![status,reason,candidate.asset_id,candidate.observed_path,
            candidate.observed_modified_at,library.library_id,library.library_epoch,
            library.root_path,library.root_identity,candidate.location_revision],
    )?;
    Ok(changed == 1)
}

/// Register ownership evidence produced by a controlled import. The asset row,
/// library generation, observed path/version and location revision are all
/// checked in the same statement that writes the receipt.
#[allow(clippy::too_many_arguments)]
pub fn record_verified(
    conn: &Connection,
    library: &LibrarySnapshot,
    candidate: &VerificationCandidate,
    relative_path: &str,
    file_identity: &str,
    content_sha256: &str,
    ownership_receipt: &str,
) -> AppResult<bool> {
    if relative_path.trim().is_empty()
        || file_identity.trim().is_empty()
        || content_sha256.len() != 64
        || !content_sha256.bytes().all(|b| b.is_ascii_hexdigit())
        || ownership_receipt.trim().is_empty()
    {
        return Err(AppError::invalid_arg("托管素材核验证据不完整"));
    }
    let changed = conn.execute(
        "INSERT INTO managed_asset_locations(
            asset_id,location_revision,library_id,library_epoch,relative_path,
            observed_path,observed_modified_at,file_identity,content_sha256,
            ownership_receipt,status,reason)
         SELECT ?1,lower(hex(randomblob(16))),?2,?3,?4,?5,?6,?7,?8,?9,'verified','verified'
         WHERE EXISTS(
            SELECT 1 FROM assets a
            WHERE a.id=?1 AND a.file_path=?5 AND a.modified_at=?6
         )
         AND (SELECT location_revision FROM managed_asset_locations WHERE asset_id=?1) IS ?10
         AND EXISTS(
            SELECT 1 FROM libraries l
            WHERE l.singleton=1 AND l.library_id=?2 AND l.library_epoch=?3
              AND l.root_path=?11 AND l.root_identity IS ?12 AND l.health='ready'
         )
         ON CONFLICT(asset_id) DO UPDATE SET
            status='verified',reason='verified',
            library_id=excluded.library_id,library_epoch=excluded.library_epoch,
            relative_path=excluded.relative_path,
            observed_path=excluded.observed_path,
            observed_modified_at=excluded.observed_modified_at,
            file_identity=excluded.file_identity,
            content_sha256=excluded.content_sha256,
            ownership_receipt=excluded.ownership_receipt,
            location_revision=excluded.location_revision",
        rusqlite::params![
            candidate.asset_id,
            library.library_id,
            library.library_epoch,
            relative_path,
            candidate.observed_path,
            candidate.observed_modified_at,
            file_identity,
            content_sha256,
            ownership_receipt,
            candidate.location_revision,
            library.root_path,
            library.root_identity,
        ],
    )?;
    Ok(changed == 1)
}

/// Keyset page for lock-free verification IO. The caller carries the library snapshot.
pub fn pending_page(
    conn: &Connection,
    after_id: i64,
    limit: usize,
) -> AppResult<Vec<VerificationCandidate>> {
    if !(1..=100).contains(&limit) || after_id < 0 {
        return Err(AppError::invalid_arg("核验分页参数无效"));
    }
    let mut stmt=conn.prepare(&format!("SELECT a.id,a.file_path,a.modified_at,ml.location_revision FROM assets a LEFT JOIN managed_asset_locations ml ON ml.asset_id=a.id WHERE a.id>?1 AND NOT ({VERIFIED_ASSET_SCOPE}) ORDER BY a.id LIMIT ?2"))?;
    let rows = stmt.query_map(rusqlite::params![after_id, limit as i64], |r| {
        Ok(VerificationCandidate {
            asset_id: r.get(0)?,
            observed_path: r.get(1)?,
            observed_modified_at: r.get(2)?,
            location_revision: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn verification_candidate(
    conn: &Connection,
    asset_id: i64,
) -> AppResult<Option<VerificationCandidate>> {
    Ok(conn
        .query_row(
            "SELECT a.id,a.file_path,a.modified_at,ml.location_revision
               FROM assets a LEFT JOIN managed_asset_locations ml ON ml.asset_id=a.id
              WHERE a.id=?1",
            [asset_id],
            |row| {
                Ok(VerificationCandidate {
                    asset_id: row.get(0)?,
                    observed_path: row.get(1)?,
                    observed_modified_at: row.get(2)?,
                    location_revision: row.get(3)?,
                })
            },
        )
        .optional()?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn legacy_fixture() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", true).unwrap();
        conn.execute_batch("CREATE TABLE assets(id INTEGER PRIMARY KEY,file_path TEXT NOT NULL,modified_at INTEGER NOT NULL);
            CREATE TABLE legacy_links(asset_id INTEGER REFERENCES assets(id),label TEXT);
            INSERT INTO assets VALUES(41,'C:/old/outside.jpg',123);
            INSERT INTO legacy_links VALUES(41,'keep-label');").unwrap();
        conn
    }

    #[test]
    fn structure_reentry_preserves_old_ids_relations_and_does_not_claim_paths() {
        let conn = legacy_fixture();
        migrate_structure(&conn).unwrap();
        migrate_structure(&conn).unwrap();
        let snapshot = initialize_candidate(&conn, "C:/old").unwrap();
        assert_eq!(snapshot.health, LibraryHealth::Unverified);
        assert_eq!(
            initialize_candidate(&conn, "C:/different").unwrap(),
            snapshot
        );
        let page = pending_page(&conn, 0, 100).unwrap();
        assert_eq!(page.len(), 1);
        assert_eq!(page[0].asset_id, 41);
        assert_eq!(
            conn.query_row(
                "SELECT label FROM legacy_links WHERE asset_id=41",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "keep-label"
        );
        assert_eq!(
            conn.query_row(
                &format!("SELECT count(*) FROM assets a WHERE {VERIFIED_ASSET_SCOPE}"),
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
        assert!(pending_page(&conn, 0, 101).is_err());
        assert!(pending_page(&conn, -1, 1).is_err());
    }

    #[test]
    fn new_and_changed_assets_cannot_inherit_verified_ownership() {
        let conn = legacy_fixture();
        migrate_structure(&conn).unwrap();
        conn.execute("INSERT INTO managed_asset_locations(asset_id,observed_path,observed_modified_at) SELECT id,file_path,modified_at FROM assets",[]).unwrap();
        initialize_candidate(&conn, "C:/root").unwrap();
        assert!(conn
            .execute("UPDATE libraries SET health='ready'", [])
            .is_err());
        assert!(conn
            .execute("UPDATE managed_asset_locations SET status='verified'", [])
            .is_err());
        conn.execute(
            "UPDATE libraries SET health='ready',root_identity='fixture-root',library_epoch=lower(hex(randomblob(16)))",
            [],
        )
        .unwrap();
        let library = current(&conn).unwrap().unwrap();
        conn.execute("UPDATE managed_asset_locations SET status='verified',library_id=?1,library_epoch=?2,relative_path='old.jpg',file_identity='fixture-file',content_sha256=?3,ownership_receipt='fixture-receipt'",rusqlite::params![library.library_id,library.library_epoch,"a".repeat(64)]).unwrap();
        let visible = || {
            conn.query_row(
                &format!("SELECT count(*) FROM assets a WHERE {VERIFIED_ASSET_SCOPE}"),
                [],
                |r| r.get::<_, i64>(0),
            )
            .unwrap()
        };
        assert_eq!(visible(), 1);
        conn.execute("UPDATE libraries SET health='offline'", [])
            .unwrap();
        assert_eq!(visible(), 0);
        conn.execute("UPDATE libraries SET health='ready'", [])
            .unwrap();
        assert_eq!(visible(), 1);
        conn.execute("UPDATE libraries SET library_epoch='another-epoch'", [])
            .unwrap();
        assert_eq!(visible(), 0);
        conn.execute(
            "UPDATE libraries SET library_epoch=?1",
            [&library.library_epoch],
        )
        .unwrap();
        assert_eq!(visible(), 1);
        conn.execute("INSERT INTO assets VALUES(42,'C:/root/new.jpg',200)", [])
            .unwrap();
        assert_eq!(pending_page(&conn, 0, 100).unwrap()[0].asset_id, 42);
        conn.execute(
            "UPDATE assets SET file_path='C:/elsewhere/replaced.jpg' WHERE id=41",
            [],
        )
        .unwrap();
        assert_eq!(pending_page(&conn, 0, 100).unwrap().len(), 2);
        let evidence: Option<String> = conn
            .query_row(
                "SELECT ownership_receipt FROM managed_asset_locations WHERE asset_id=41",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(evidence.is_none());
        assert_eq!(visible(), 0);
    }

    #[test]
    fn partial_structure_reentry_keeps_unmapped_legacy_available_and_repairs_trigger() {
        let conn = legacy_fixture();
        migrate_structure(&conn).unwrap();
        conn.execute_batch("DROP TRIGGER trg_managed_asset_insert; DELETE FROM managed_asset_locations WHERE asset_id=41;").unwrap();
        migrate_structure(&conn).unwrap();
        conn.execute("INSERT INTO assets VALUES(42,'C:/root/new.jpg',200)", [])
            .unwrap();
        assert_eq!(pending_page(&conn, 0, 100).unwrap().len(), 2);
        let violations: i64 = conn
            .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(violations, 0);
    }

    #[test]
    fn classification_rejects_offline_and_stale_library_or_asset_snapshots() {
        let conn = legacy_fixture();
        migrate_structure(&conn).unwrap();
        let initial = initialize_candidate(&conn, "C:/root").unwrap();
        let candidate = pending_page(&conn, 0, 1).unwrap().remove(0);
        assert!(!classify_pending(
            &conn,
            &initial,
            &candidate,
            PendingClassification::ExternalPending
        )
        .unwrap());
        conn.execute(
            "UPDATE libraries SET health='ready',root_identity='fixture-root',library_epoch=lower(hex(randomblob(16)))",
            [],
        )
        .unwrap();
        let ready = current(&conn).unwrap().unwrap();
        assert!(classify_pending(
            &conn,
            &ready,
            &candidate,
            PendingClassification::ExternalPending
        )
        .unwrap());
        conn.execute("UPDATE libraries SET health='offline'", [])
            .unwrap();
        assert!(
            !classify_pending(&conn, &ready, &candidate, PendingClassification::Missing).unwrap()
        );
        conn.execute(
            "UPDATE libraries SET health='ready',library_epoch='changed-epoch'",
            [],
        )
        .unwrap();
        assert!(
            !classify_pending(&conn, &ready, &candidate, PendingClassification::Unknown).unwrap()
        );
        let changed = current(&conn).unwrap().unwrap();
        conn.execute("UPDATE assets SET modified_at=999 WHERE id=41", [])
            .unwrap();
        assert!(!classify_pending(
            &conn,
            &changed,
            &candidate,
            PendingClassification::ExternalPending
        )
        .unwrap());
        assert_eq!(
            conn.query_row(
                "SELECT status FROM managed_asset_locations WHERE asset_id=41",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
            "unknown"
        );
    }

    #[test]
    fn path_aba_and_competing_classification_invalidate_old_revision() {
        let conn = legacy_fixture();
        migrate_structure(&conn).unwrap();
        initialize_candidate(&conn, "C:/root").unwrap();
        conn.execute(
            "UPDATE libraries SET health='ready',root_identity='fixture-root',library_epoch=lower(hex(randomblob(16)))",
            [],
        )
        .unwrap();
        let root = current(&conn).unwrap().unwrap();
        let missing = pending_page(&conn, 0, 1).unwrap().remove(0);
        assert!(missing.location_revision.is_none());
        conn.execute(
            "UPDATE assets SET file_path='C:/changed.jpg' WHERE id=41",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE assets SET file_path='C:/old/outside.jpg' WHERE id=41",
            [],
        )
        .unwrap();
        assert!(!classify_pending(
            &conn,
            &root,
            &missing,
            PendingClassification::ExternalPending
        )
        .unwrap());
        let observed = pending_page(&conn, 0, 1).unwrap().remove(0);
        assert!(observed.location_revision.is_some());
        assert!(classify_pending(&conn, &root, &observed, PendingClassification::Unknown).unwrap());
        assert!(!classify_pending(
            &conn,
            &root,
            &observed,
            PendingClassification::ExternalPending
        )
        .unwrap());
        let new_observation = pending_page(&conn, 0, 1).unwrap().remove(0);
        assert_ne!(
            new_observation.location_revision,
            observed.location_revision
        );
        assert!(classify_pending(
            &conn,
            &root,
            &new_observation,
            PendingClassification::ExternalPending
        )
        .unwrap());
    }

    #[test]
    fn structure_reentry_does_not_backfill_thirty_thousand_legacy_rows() {
        let conn = legacy_fixture();
        conn.execute_batch("WITH RECURSIVE ids(n) AS (VALUES(100) UNION ALL SELECT n+1 FROM ids WHERE n<30099) INSERT INTO assets SELECT n,'C:/legacy/'||n||'.jpg',123 FROM ids;").unwrap();
        migrate_structure(&conn).unwrap();
        migrate_structure(&conn).unwrap();
        assert_eq!(
            conn.query_row("SELECT count(*) FROM managed_asset_locations", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
        let first = pending_page(&conn, 0, 100).unwrap();
        assert_eq!(first.len(), 100);
        let next = pending_page(&conn, first.last().unwrap().asset_id, 100).unwrap();
        assert_eq!(next.len(), 100);
        assert!(next[0].asset_id > first.last().unwrap().asset_id);
        assert!(first.iter().all(|item| item.location_revision.is_none()));
    }

    #[test]
    fn root_identity_changes_require_epoch_and_restore_never_reuses_it() {
        let conn = legacy_fixture();
        migrate_structure(&conn).unwrap();
        let original = initialize_candidate(&conn, "C:/root").unwrap();
        assert!(conn
            .execute("UPDATE libraries SET root_path='C:/new'", [])
            .is_err());
        assert!(conn
            .execute(
                "UPDATE libraries SET root_identity='root-id',health='ready'",
                []
            )
            .is_err());
        conn.execute("UPDATE libraries SET root_identity='root-id',health='ready',library_epoch='fresh-fixture-epoch'",[]).unwrap();
        let ready = current(&conn).unwrap().unwrap();
        let restored = invalidate_restored_locator(&conn).unwrap();
        let repeated = invalidate_restored_locator(&conn).unwrap();
        assert_eq!(restored.library_id, original.library_id);
        assert_eq!(restored.health, LibraryHealth::Unverified);
        assert!(restored.root_identity.is_none());
        assert_ne!(restored.library_epoch, ready.library_epoch);
        assert_ne!(repeated.library_epoch, restored.library_epoch);
        assert_eq!(initialize_candidate(&conn, "ignored").unwrap(), repeated);
    }

    #[test]
    fn stale_verified_evidence_is_recheckable_but_current_verified_is_not_overwritten() {
        let conn = legacy_fixture();
        migrate_structure(&conn).unwrap();
        initialize_candidate(&conn, "C:/root").unwrap();
        conn.execute("UPDATE libraries SET root_identity='fixture-root',health='ready',library_epoch=lower(hex(randomblob(16)))",[]).unwrap();
        let old = current(&conn).unwrap().unwrap();
        let candidate = pending_page(&conn, 0, 1).unwrap().remove(0);
        assert!(classify_pending(&conn, &old, &candidate, PendingClassification::Unknown).unwrap());
        let observed = pending_page(&conn, 0, 1).unwrap().remove(0);
        conn.execute("UPDATE managed_asset_locations SET status='verified',library_id=?1,library_epoch=?2,relative_path='asset.jpg',file_identity='fixture-file',content_sha256=?3,ownership_receipt='fixture-proof'",rusqlite::params![old.library_id,old.library_epoch,"a".repeat(64)]).unwrap();
        assert!(pending_page(&conn, 0, 1).unwrap().is_empty());
        assert!(!classify_pending(
            &conn,
            &old,
            &observed,
            PendingClassification::ExternalPending
        )
        .unwrap());
        invalidate_restored_locator(&conn).unwrap();
        assert_eq!(pending_page(&conn, 0, 1).unwrap().len(), 1);
        conn.execute("UPDATE libraries SET root_identity='fixture-root',health='ready',library_epoch=lower(hex(randomblob(16)))",[]).unwrap();
        let new = current(&conn).unwrap().unwrap();
        let stale = pending_page(&conn, 0, 1).unwrap().remove(0);
        assert!(!classify_pending(&conn, &old, &stale, PendingClassification::Unknown).unwrap());
        assert!(classify_pending(&conn, &new, &stale, PendingClassification::Unknown).unwrap());
        let receipt: Option<String> = conn
            .query_row(
                "SELECT ownership_receipt FROM managed_asset_locations WHERE asset_id=41",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(receipt.is_none());
    }

    #[test]
    fn verified_scope_controls_normal_reads_facets_and_expired_cleanup() {
        let conn = crate::db::init_memory().unwrap();
        let managed_id = crate::db::assets::insert(
            &conn,
            "C:/fixture-root/managed.jpg",
            "managed.jpg",
            "jpg",
            100,
            "image/jpeg",
            42,
        )
        .unwrap();
        conn.execute(
            "UPDATE assets
                SET content_description='needle managed',camera='FixtureCam'
              WHERE id=?1",
            [managed_id],
        )
        .unwrap();
        let external_id = crate::db::assets::insert(
            &conn,
            "C:/outside/external.jpg",
            "external.jpg",
            "jpg",
            100,
            "image/jpeg",
            42,
        )
        .unwrap();
        conn.execute(
            "UPDATE assets SET content_description='needle external',deleted_at=1 WHERE id=?1",
            [external_id],
        )
        .unwrap();

        let camera_visible = |conn: &Connection| {
            crate::db::assets::list_metadata_facets(conn, Some("C:/fixture-root"))
                .unwrap()
                .into_iter()
                .find(|facet| facet.key == "camera")
                .is_some_and(|facet| facet.items.iter().any(|item| item.label == "FixtureCam"))
        };
        assert!(
            !crate::db::assets::list_ids(&conn, &crate::db::assets::AssetFilter::default())
                .unwrap()
                .contains(&managed_id)
        );
        assert!(!crate::db::search::search_asset_ids_all(&conn, "needle")
            .unwrap()
            .contains(&managed_id));
        assert!(!camera_visible(&conn));
        assert_eq!(
            crate::db::assets::soft_delete(&conn, &[external_id]).unwrap(),
            0
        );
        assert!(crate::db::assets::list_expired_trash(&conn, i64::MAX)
            .unwrap()
            .is_empty());
        assert!(crate::db::assets::get_managed(&conn, managed_id).is_err());

        let initial = current(&conn).unwrap().unwrap();
        let ready =
            mark_root_ready(&conn, &initial, "C:/fixture-root", "fixture-root-identity").unwrap();
        let candidate = pending_page(&conn, 0, 100)
            .unwrap()
            .into_iter()
            .find(|item| item.asset_id == managed_id)
            .unwrap();
        let stale = LibrarySnapshot {
            library_epoch: "stale-epoch".into(),
            ..ready.clone()
        };
        assert!(!record_verified(
            &conn,
            &stale,
            &candidate,
            "managed.jpg",
            "fixture-file",
            &"a".repeat(64),
            "fixture-receipt",
        )
        .unwrap());
        assert!(record_verified(
            &conn,
            &ready,
            &candidate,
            "managed.jpg",
            "fixture-file",
            &"a".repeat(64),
            "fixture-receipt",
        )
        .unwrap());

        assert!(crate::db::assets::get_managed(&conn, managed_id).is_ok());
        assert!(
            crate::db::assets::list_ids(&conn, &crate::db::assets::AssetFilter::default())
                .unwrap()
                .contains(&managed_id)
        );
        assert!(crate::db::search::search_asset_ids_all(&conn, "needle")
            .unwrap()
            .contains(&managed_id));
        assert!(camera_visible(&conn));

        conn.execute("UPDATE assets SET deleted_at=1 WHERE id=?1", [managed_id])
            .unwrap();
        assert_eq!(
            crate::db::assets::list_expired_trash(&conn, i64::MAX)
                .unwrap()
                .into_iter()
                .map(|item| item.0)
                .collect::<Vec<_>>(),
            vec![managed_id]
        );

        set_health(&conn, &ready, LibraryHealth::Offline).unwrap();
        assert!(crate::db::assets::get_managed(&conn, managed_id).is_err());
        assert!(crate::db::assets::list_expired_trash(&conn, i64::MAX)
            .unwrap()
            .is_empty());
        assert_eq!(
            conn.query_row(
                "SELECT status FROM managed_asset_locations WHERE asset_id=?1",
                [managed_id],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "verified"
        );
    }

    #[test]
    fn verified_scope_controls_backfill_candidates_derived_cache_and_bulk_edits() {
        let conn = crate::db::init_memory().unwrap();
        let initial = current(&conn).unwrap().unwrap();
        let ready =
            mark_root_ready(&conn, &initial, "C:/fixture-root", "fixture-root-identity").unwrap();

        let managed_image = crate::db::assets::insert(
            &conn,
            "C:/fixture-root/managed.jpg",
            "managed.jpg",
            "jpg",
            100,
            "image/jpeg",
            42,
        )
        .unwrap();
        let managed_video = crate::db::assets::insert(
            &conn,
            "C:/fixture-root/managed.mp4",
            "managed.mp4",
            "mp4",
            100,
            "video/mp4",
            42,
        )
        .unwrap();
        let external_image = crate::db::assets::insert(
            &conn,
            "C:/outside/external.jpg",
            "external.jpg",
            "jpg",
            100,
            "image/jpeg",
            42,
        )
        .unwrap();
        let external_video = crate::db::assets::insert(
            &conn,
            "C:/outside/external.mp4",
            "external.mp4",
            "mp4",
            100,
            "video/mp4",
            42,
        )
        .unwrap();

        for asset_id in [managed_image, managed_video] {
            let candidate = pending_page(&conn, 0, 100)
                .unwrap()
                .into_iter()
                .find(|candidate| candidate.asset_id == asset_id)
                .unwrap();
            assert!(record_verified(
                &conn,
                &ready,
                &candidate,
                &format!("asset-{asset_id}"),
                "fixture-file",
                &"a".repeat(64),
                "fixture-receipt",
            )
            .unwrap());
        }

        conn.execute(
            "UPDATE assets
                SET placeholder_path='managed-placeholder',hd_thumbnail_path='managed-hd'
              WHERE id IN (?1,?2)",
            rusqlite::params![managed_image, managed_video],
        )
        .unwrap();
        conn.execute(
            "UPDATE assets
                SET placeholder_path='external-placeholder',hd_thumbnail_path='external-hd'
              WHERE id IN (?1,?2)",
            rusqlite::params![external_image, external_video],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO asset_palette_colors(asset_id,rank,color_bucket,ratio)
             VALUES(?1,0,0,1.0)",
            [external_image],
        )
        .unwrap();

        let palette_candidates = crate::db::assets::list_ids_needing_palette(&conn).unwrap();
        assert!(palette_candidates.contains(&managed_image));
        assert!(!palette_candidates.contains(&external_image));
        assert!(!crate::db::assets::list_video_ids(&conn)
            .unwrap()
            .contains(&external_video));
        assert!(!crate::db::assets::list_geo_taken_all_ids(&conn)
            .unwrap()
            .contains(&external_image));
        assert!(!crate::db::assets::list_image_ids(&conn)
            .unwrap()
            .contains(&external_image));

        assert!(crate::db::assets::set_rating(&conn, &[external_image], 5).is_err());
        assert!(crate::db::assets::set_user_rotation(&conn, &[external_video], 90).is_err());
        assert_eq!(
            crate::db::assets::set_rating(&conn, &[managed_image], 5).unwrap(),
            1
        );

        crate::db::assets::clear_all_placeholder_paths(&conn).unwrap();
        crate::db::assets::clear_all_hd_thumbnail_paths(&conn).unwrap();
        let (managed_placeholder, managed_hd, external_placeholder, external_hd): (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = conn
            .query_row(
                "SELECT
                    (SELECT placeholder_path FROM assets WHERE id=?1),
                    (SELECT hd_thumbnail_path FROM assets WHERE id=?1),
                    (SELECT placeholder_path FROM assets WHERE id=?2),
                    (SELECT hd_thumbnail_path FROM assets WHERE id=?2)",
                rusqlite::params![managed_image, external_image],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();
        assert!(managed_placeholder.is_none());
        assert!(managed_hd.is_none());
        assert_eq!(
            external_placeholder.as_deref(),
            Some("external-placeholder")
        );
        assert_eq!(external_hd.as_deref(), Some("external-hd"));

        conn.execute(
            "UPDATE assets
                SET palette_json=?1
              WHERE id=?2",
            rusqlite::params![
                r##"[{"hex":"#000000","r":0,"g":0,"b":0,"ratio":1.0}]"##,
                managed_image
            ],
        )
        .unwrap();
        crate::db::assets::rescan_palette_colors(&conn).unwrap();
        let external_relations: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM asset_palette_colors WHERE asset_id=?1",
                [external_image],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(external_relations, 1);
    }
}
