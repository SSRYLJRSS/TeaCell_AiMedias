//! A5-01: pairing records for external MCP clients.
//!
//! A pairing record is the only thing that turns an outside client into a
//! caller, and it deliberately stores **no secret**: the handshake token is an
//! HMAC over the connection id with a key that exists only in the running
//! host's memory and in the endpoint file it writes at startup. What the record
//! does store is *scope* (which capabilities the user ticked) and *revocation*,
//! so revoking is one timestamp rather than a credential hunt.
//!
//! Capabilities are persisted as a JSON array of wire ids. The db layer stays
//! typed as `String` here on purpose: the capability enum lives in `contracts`,
//! and the allow-list check that makes an id meaningful belongs to the service
//! layer that also owns the MCP-publishable set.

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::AppResult;

pub fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        r#"
CREATE TABLE IF NOT EXISTS mcp_connections (
  connection_id TEXT PRIMARY KEY,
  label         TEXT NOT NULL,
  principal_id  TEXT NOT NULL,
  capabilities  TEXT NOT NULL,
  created_at    INTEGER NOT NULL,
  updated_at    INTEGER NOT NULL,
  revoked_at    INTEGER,
  last_used_at  INTEGER
);
CREATE INDEX IF NOT EXISTS ix_mcp_connections_revoked
  ON mcp_connections(revoked_at);
"#,
    )?;
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingRecord {
    pub connection_id: String,
    pub label: String,
    pub principal_id: String,
    pub capabilities: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
    pub revoked_at: Option<i64>,
    pub last_used_at: Option<i64>,
}

impl PairingRecord {
    pub fn is_revoked(&self) -> bool {
        self.revoked_at.is_some()
    }
}

fn decode_capabilities(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw).unwrap_or_default()
}

fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PairingRecord> {
    let capabilities: String = row.get("capabilities")?;
    Ok(PairingRecord {
        connection_id: row.get("connection_id")?,
        label: row.get("label")?,
        principal_id: row.get("principal_id")?,
        capabilities: decode_capabilities(&capabilities),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
        revoked_at: row.get("revoked_at")?,
        last_used_at: row.get("last_used_at")?,
    })
}

pub fn insert(
    conn: &Connection,
    connection_id: &str,
    label: &str,
    principal_id: &str,
    capabilities: &[String],
    now: i64,
) -> AppResult<()> {
    let encoded = serde_json::to_string(capabilities).map_err(|error| {
        crate::error::AppError::internal(format!("能力范围序列化失败: {error}"))
    })?;
    conn.execute(
        "INSERT INTO mcp_connections(connection_id, label, principal_id, capabilities, created_at, updated_at)
         VALUES(?1, ?2, ?3, ?4, ?5, ?5)",
        params![connection_id, label, principal_id, encoded, now],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, connection_id: &str) -> AppResult<Option<PairingRecord>> {
    Ok(conn
        .query_row(
            "SELECT connection_id, label, principal_id, capabilities, created_at, updated_at,
                    revoked_at, last_used_at
             FROM mcp_connections WHERE connection_id = ?1",
            params![connection_id],
            map_row,
        )
        .optional()?)
}

pub fn list(conn: &Connection) -> AppResult<Vec<PairingRecord>> {
    let mut statement = conn.prepare(
        "SELECT connection_id, label, principal_id, capabilities, created_at, updated_at,
                revoked_at, last_used_at
         FROM mcp_connections ORDER BY created_at, connection_id",
    )?;
    let rows = statement.query_map([], map_row)?;
    let mut records = Vec::new();
    for row in rows {
        records.push(row?);
    }
    Ok(records)
}

/// A plan only stores its principal, so the review UI needs this lookup to name
/// the external client that asked for it. An active connection wins over a
/// revoked one so a re-paired client is reported under its current label.
pub fn find_by_principal(
    conn: &Connection,
    principal_id: &str,
) -> AppResult<Option<PairingRecord>> {
    Ok(conn
        .query_row(
            "SELECT connection_id, label, principal_id, capabilities, created_at, updated_at,
                    revoked_at, last_used_at
               FROM mcp_connections WHERE principal_id = ?1
              ORDER BY (revoked_at IS NULL) DESC, created_at DESC, connection_id
              LIMIT 1",
            params![principal_id],
            map_row,
        )
        .optional()?)
}

pub fn set_capabilities(
    conn: &Connection,
    connection_id: &str,
    capabilities: &[String],
    now: i64,
) -> AppResult<bool> {
    let encoded = serde_json::to_string(capabilities).map_err(|error| {
        crate::error::AppError::internal(format!("能力范围序列化失败: {error}"))
    })?;
    let changed = conn.execute(
        "UPDATE mcp_connections SET capabilities = ?2, updated_at = ?3 WHERE connection_id = ?1",
        params![connection_id, encoded, now],
    )?;
    Ok(changed == 1)
}

pub fn rename(conn: &Connection, connection_id: &str, label: &str, now: i64) -> AppResult<bool> {
    let changed = conn.execute(
        "UPDATE mcp_connections SET label = ?2, updated_at = ?3 WHERE connection_id = ?1",
        params![connection_id, label, now],
    )?;
    Ok(changed == 1)
}

/// Revocation is idempotent: the first timestamp wins so the audit trail keeps
/// the moment the user actually revoked instead of the last time we noticed.
pub fn revoke(conn: &Connection, connection_id: &str, now: i64) -> AppResult<bool> {
    let changed = conn.execute(
        "UPDATE mcp_connections SET revoked_at = COALESCE(revoked_at, ?2), updated_at = ?2
         WHERE connection_id = ?1",
        params![connection_id, now],
    )?;
    Ok(changed == 1)
}

pub fn touch(conn: &Connection, connection_id: &str, now: i64) -> AppResult<()> {
    conn.execute(
        "UPDATE mcp_connections SET last_used_at = ?2 WHERE connection_id = ?1",
        params![connection_id, now],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    fn seeded() -> Connection {
        let conn = init_memory().unwrap();
        insert(
            &conn,
            "mcpconn_0123456789abcdef",
            "本地编辑器",
            "principal_mcp_fixture",
            &["assets.search".to_string(), "operations.get".to_string()],
            1_700_000_000,
        )
        .unwrap();
        conn
    }

    #[test]
    fn a_pairing_round_trips_its_scope_and_starts_unrevoked() {
        let conn = seeded();
        let record = get(&conn, "mcpconn_0123456789abcdef").unwrap().unwrap();
        assert_eq!(record.label, "本地编辑器");
        assert_eq!(record.capabilities, vec!["assets.search", "operations.get"]);
        assert!(!record.is_revoked());
        assert!(record.last_used_at.is_none());
    }

    #[test]
    fn revocation_keeps_the_first_timestamp() {
        let conn = seeded();
        assert!(revoke(&conn, "mcpconn_0123456789abcdef", 1_700_000_100).unwrap());
        assert!(revoke(&conn, "mcpconn_0123456789abcdef", 1_700_000_900).unwrap());
        let record = get(&conn, "mcpconn_0123456789abcdef").unwrap().unwrap();
        assert_eq!(record.revoked_at, Some(1_700_000_100));
        assert!(record.is_revoked());
    }

    #[test]
    fn scope_can_be_replaced_and_unknown_rows_report_false() {
        let conn = seeded();
        assert!(set_capabilities(
            &conn,
            "mcpconn_0123456789abcdef",
            &["assets.search".to_string()],
            1_700_000_200,
        )
        .unwrap());
        let record = get(&conn, "mcpconn_0123456789abcdef").unwrap().unwrap();
        assert_eq!(record.capabilities, vec!["assets.search"]);
        assert!(!revoke(&conn, "mcpconn_ffffffffffffffff", 1).unwrap());
        assert!(!rename(&conn, "mcpconn_ffffffffffffffff", "x", 1).unwrap());
    }

    #[test]
    fn listings_are_stable_and_include_revoked_rows() {
        let conn = seeded();
        insert(
            &conn,
            "mcpconn_1111111111111111",
            "另一个客户端",
            "principal_mcp_second",
            &["assets.search".to_string()],
            1_700_000_500,
        )
        .unwrap();
        revoke(&conn, "mcpconn_1111111111111111", 1_700_000_600).unwrap();
        let records = list(&conn).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].connection_id, "mcpconn_0123456789abcdef");
        assert!(records[1].is_revoked());
    }

    #[test]
    fn corrupted_scope_json_degrades_to_an_empty_scope_not_a_panic() {
        let conn = seeded();
        conn.execute(
            "UPDATE mcp_connections SET capabilities = 'not-json' WHERE connection_id = ?1",
            params!["mcpconn_0123456789abcdef"],
        )
        .unwrap();
        let record = get(&conn, "mcpconn_0123456789abcdef").unwrap().unwrap();
        assert!(record.capabilities.is_empty());
    }
}
