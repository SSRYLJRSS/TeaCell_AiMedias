//! A4-02: stable references for library sub-folders ("分库").
//!
//! A collection is not an entity that owns assets; it is a directory inside the
//! managed root. Assets belong to a collection only through `assets.file_path`,
//! which is why this table stores the relative directory and never an asset list
//! — the folder facet, the current result view and the file system all stay
//! derived from one source of truth.
//!
//! The reference is derived, not random: `collection_<32 hex>` over
//! `(library_id, library_epoch, relative_path)`. Deriving it means `ensure` is
//! idempotent and two windows cannot mint two ids for the same folder, while the
//! epoch component makes every reference from an older root identity stale
//! instead of silently resolving to a different directory.

use rusqlite::{params, Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use crate::error::{AppError, AppResult};

/// The root itself is a valid move target; it is addressed by an empty relative
/// path so "move back to 总库根目录" needs no special-casing at call sites.
pub const ROOT_RELATIVE_PATH: &str = "";

pub fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS library_collections (
  collection_ref TEXT PRIMARY KEY,
  library_id     TEXT NOT NULL,
  library_epoch  TEXT NOT NULL,
  relative_path  TEXT NOT NULL,
  created_at     INTEGER NOT NULL,
  UNIQUE(library_id, library_epoch, relative_path)
);
CREATE INDEX IF NOT EXISTS ix_library_collections_library
  ON library_collections(library_id, library_epoch);
"#,
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredCollection {
    pub collection_ref: String,
    pub library_id: String,
    pub library_epoch: String,
    pub relative_path: String,
}

/// Deterministic reference for one folder inside one library epoch.
pub fn collection_ref_for(library_id: &str, library_epoch: &str, relative_path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"teacell-collection-v1\0");
    hasher.update(library_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(library_epoch.as_bytes());
    hasher.update(b"\0");
    hasher.update(relative_path.as_bytes());
    let digest = hasher.finalize();
    let hex: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    format!("collection_{hex}")
}

pub fn ensure(
    conn: &Connection,
    library_id: &str,
    library_epoch: &str,
    relative_path: &str,
    now: i64,
) -> AppResult<StoredCollection> {
    let collection_ref = collection_ref_for(library_id, library_epoch, relative_path);
    conn.execute(
        "INSERT INTO library_collections(collection_ref,library_id,library_epoch,relative_path,created_at)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(collection_ref) DO NOTHING",
        params![
            collection_ref,
            library_id,
            library_epoch,
            relative_path,
            now
        ],
    )?;
    find(conn, &collection_ref)?.ok_or_else(|| AppError::internal("分库引用写入后不可读"))
}

pub fn find(conn: &Connection, collection_ref: &str) -> AppResult<Option<StoredCollection>> {
    Ok(conn
        .query_row(
            "SELECT collection_ref,library_id,library_epoch,relative_path
               FROM library_collections WHERE collection_ref=?1",
            params![collection_ref],
            |row| {
                Ok(StoredCollection {
                    collection_ref: row.get(0)?,
                    library_id: row.get(1)?,
                    library_epoch: row.get(2)?,
                    relative_path: row.get(3)?,
                })
            },
        )
        .optional()?)
}

pub fn list(conn: &Connection, library_id: &str) -> AppResult<Vec<StoredCollection>> {
    let mut stmt = conn.prepare(
        "SELECT collection_ref,library_id,library_epoch,relative_path
           FROM library_collections WHERE library_id=?1 ORDER BY relative_path",
    )?;
    let rows = stmt.query_map(params![library_id], |row| {
        Ok(StoredCollection {
            collection_ref: row.get(0)?,
            library_id: row.get(1)?,
            library_epoch: row.get(2)?,
            relative_path: row.get(3)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    #[test]
    fn structure_is_idempotent_and_references_are_derived() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        migrate_structure(&conn).unwrap();

        let first = ensure(&conn, "library_a", "epoch_1", "风景", 10).unwrap();
        let again = ensure(&conn, "library_a", "epoch_1", "风景", 11).unwrap();
        assert_eq!(first.collection_ref, again.collection_ref);
        assert!(first.collection_ref.starts_with("collection_"));
        assert_eq!(first.collection_ref.len(), "collection_".len() + 32);

        // Same folder in a new epoch is a different reference: old plans must not
        // resolve to a directory under a root that was re-bound.
        let newer = ensure(&conn, "library_a", "epoch_2", "风景", 12).unwrap();
        assert_ne!(newer.collection_ref, first.collection_ref);
        assert_eq!(list(&conn, "library_a").unwrap().len(), 2);

        // The root itself is addressable.
        let root = ensure(&conn, "library_a", "epoch_1", ROOT_RELATIVE_PATH, 13).unwrap();
        assert_eq!(root.relative_path, ROOT_RELATIVE_PATH);
        assert_ne!(root.collection_ref, first.collection_ref);
    }

    #[test]
    fn an_unknown_reference_does_not_resolve() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        assert!(find(&conn, "collection_00000000000000000000000000000000")
            .unwrap()
            .is_none());
    }
}
