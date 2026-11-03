//! A3-03: persisted snapshot selections referenced by agent tools.
//!
//! A selection fixes the asset-id set that one trusted caller resolved at one
//! library epoch. The reference is opaque and never grants authority by itself:
//! every use re-checks the caller's principal, the current library epoch and the
//! expiry before the set may be used as a plan/tool target.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::AppResult;

/// A selection stays usable for one hour; longer-lived targets must be recreated.
pub const SELECTION_TTL_SECONDS: i64 = 3_600;
/// Upper bound on the persisted snapshot. Beyond this the selection is truncated
/// and callers must narrow the query instead of assuming the whole library.
pub const MAX_SELECTION_ASSETS: usize = 10_000;

pub fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS agent_selections (
  selection_ref  TEXT PRIMARY KEY,
  principal_id   TEXT NOT NULL,
  library_id     TEXT NOT NULL,
  library_epoch  TEXT NOT NULL,
  query          TEXT NOT NULL,
  asset_ids      TEXT NOT NULL,
  total          INTEGER NOT NULL,
  truncated      INTEGER NOT NULL,
  created_at     INTEGER NOT NULL,
  expires_at     INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS ix_agent_selections_expiry ON agent_selections(expires_at);
"#,
    )?;
    Ok(())
}

pub struct NewSelection<'a> {
    pub selection_ref: &'a str,
    pub principal_id: &'a str,
    pub library_id: &'a str,
    pub library_epoch: &'a str,
    pub query: &'a str,
    pub asset_ids: &'a [i64],
    pub total: i64,
    pub truncated: bool,
    pub created_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSelection {
    pub selection_ref: String,
    pub principal_id: String,
    pub library_id: String,
    pub library_epoch: String,
    pub query: String,
    pub asset_ids: Vec<i64>,
    pub total: i64,
    pub truncated: bool,
    pub expires_at: i64,
}

pub fn insert(conn: &Connection, selection: &NewSelection<'_>) -> AppResult<()> {
    let asset_ids = serde_json::to_string(selection.asset_ids)?;
    conn.execute(
        "INSERT INTO agent_selections
           (selection_ref, principal_id, library_id, library_epoch, query, asset_ids,
            total, truncated, created_at, expires_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            selection.selection_ref,
            selection.principal_id,
            selection.library_id,
            selection.library_epoch,
            selection.query,
            asset_ids,
            selection.total,
            i64::from(selection.truncated),
            selection.created_at,
            selection.expires_at,
        ],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, selection_ref: &str) -> AppResult<Option<StoredSelection>> {
    let row = conn
        .query_row(
            "SELECT selection_ref, principal_id, library_id, library_epoch, query,
                    asset_ids, total, truncated, expires_at
               FROM agent_selections WHERE selection_ref = ?1",
            params![selection_ref],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, i64>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, i64>(8)?,
                ))
            },
        )
        .optional()?;
    let Some((
        selection_ref,
        principal_id,
        library_id,
        library_epoch,
        query,
        asset_ids,
        total,
        truncated,
        expires_at,
    )) = row
    else {
        return Ok(None);
    };
    Ok(Some(StoredSelection {
        selection_ref,
        principal_id,
        library_id,
        library_epoch,
        query,
        asset_ids: serde_json::from_str(&asset_ids)?,
        total,
        truncated: truncated != 0,
        expires_at,
    }))
}

pub fn delete_expired(conn: &Connection, now: i64) -> AppResult<u64> {
    let deleted = conn.execute(
        "DELETE FROM agent_selections WHERE expires_at <= ?1",
        params![now],
    )?;
    Ok(deleted as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    fn selection<'a>(asset_ids: &'a [i64], expires_at: i64) -> NewSelection<'a> {
        NewSelection {
            selection_ref: "selection_fixture_0000000000000000",
            principal_id: "principal_fixture_0001",
            library_id: "library_fixture_000001",
            library_epoch: "epoch_fixture_00000001",
            query: "photo",
            asset_ids,
            total: 3,
            truncated: true,
            created_at: 100,
            expires_at,
        }
    }

    #[test]
    fn structure_is_idempotent_and_round_trips_a_selection() {
        let conn = init_memory().unwrap();
        migrate_structure(&conn).unwrap();
        migrate_structure(&conn).unwrap();
        let ids = [4i64, 5];
        insert(&conn, &selection(&ids, 500)).unwrap();

        let stored = get(&conn, "selection_fixture_0000000000000000")
            .unwrap()
            .unwrap();
        assert_eq!(stored.asset_ids, ids);
        assert_eq!(stored.total, 3);
        assert!(stored.truncated);
        assert_eq!(stored.expires_at, 500);
        assert_eq!(delete_expired(&conn, 499).unwrap(), 0);
        assert_eq!(delete_expired(&conn, 500).unwrap(), 1);
        assert!(get(&conn, "selection_fixture_0000000000000000")
            .unwrap()
            .is_none());
    }
}
