//! Persisted authorization generations, directory grants, and opaque handles.
//!
//! This module owns SQL only. It never opens a user-selected path or treats a
//! stored path string as proof of authorization.

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::error::AppResult;

pub const ACCESS_READ: i64 = 1;
pub const ACCESS_WRITE: i64 = 2;
pub const ACCESS_EXPORT: i64 = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrincipalAuthorization {
    pub principal_id: String,
    pub grant_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryGrant {
    pub grant_id: String,
    pub principal_id: String,
    pub owner_session_id: Option<String>,
    pub owner_instance_id: String,
    pub purpose: String,
    pub canonical_path: String,
    pub path_identity: String,
    pub display_name: String,
    pub recursive: bool,
    pub remembered: bool,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub grant_epoch: String,
    pub library_id: String,
    pub library_epoch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceHandle {
    pub handle_id: String,
    pub principal_id: String,
    pub session_id: Option<String>,
    pub instance_id: String,
    pub grant_id: Option<String>,
    pub resource_kind: String,
    pub relative_path: Option<String>,
    pub asset_id: Option<i64>,
    pub file_identity: Option<String>,
    pub file_size: Option<i64>,
    pub modified_at: Option<i64>,
    pub purpose: String,
    pub access_mask: i64,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub grant_epoch: String,
    pub library_id: String,
    pub library_epoch: String,
}

#[derive(Debug, Clone)]
pub struct NewDirectoryGrant {
    pub grant_id: String,
    pub principal_id: String,
    pub owner_session_id: Option<String>,
    pub owner_instance_id: String,
    pub purpose: String,
    pub canonical_path: String,
    pub path_identity: String,
    pub display_name: String,
    pub recursive: bool,
    pub remembered: bool,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub grant_epoch: String,
    pub library_id: String,
    pub library_epoch: String,
}

#[derive(Debug, Clone)]
pub struct NewResourceHandle {
    pub handle_id: String,
    pub principal_id: String,
    pub session_id: Option<String>,
    pub instance_id: String,
    pub grant_id: Option<String>,
    pub resource_kind: String,
    pub relative_path: Option<String>,
    pub asset_id: Option<i64>,
    pub file_identity: Option<String>,
    pub file_size: Option<i64>,
    pub modified_at: Option<i64>,
    pub purpose: String,
    pub access_mask: i64,
    pub created_at: i64,
    pub expires_at: Option<i64>,
    pub grant_epoch: String,
    pub library_id: String,
    pub library_epoch: String,
}

pub(super) fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS authorization_principals (
            principal_id TEXT PRIMARY KEY,
            grant_epoch TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS directory_grants (
            grant_id TEXT PRIMARY KEY,
            principal_id TEXT NOT NULL REFERENCES authorization_principals(principal_id)
                ON DELETE CASCADE,
            owner_session_id TEXT,
            owner_instance_id TEXT NOT NULL,
            purpose TEXT NOT NULL CHECK(purpose IN ('import_source','export_destination')),
            canonical_path TEXT NOT NULL,
            path_identity TEXT NOT NULL,
            display_name TEXT NOT NULL,
            recursive INTEGER NOT NULL CHECK(recursive IN (0,1)),
            remembered INTEGER NOT NULL CHECK(remembered IN (0,1)),
            created_at INTEGER NOT NULL,
            expires_at INTEGER,
            revoked_at INTEGER,
            grant_epoch TEXT NOT NULL,
            library_id TEXT NOT NULL,
            library_epoch TEXT NOT NULL,
            CHECK(remembered=1 OR owner_session_id IS NOT NULL),
            CHECK(remembered=1 OR expires_at IS NOT NULL)
        );
        CREATE INDEX IF NOT EXISTS idx_directory_grants_principal
            ON directory_grants(principal_id, revoked_at, created_at DESC);
        CREATE TABLE IF NOT EXISTS resource_handles (
            handle_id TEXT PRIMARY KEY,
            principal_id TEXT NOT NULL REFERENCES authorization_principals(principal_id)
                ON DELETE CASCADE,
            session_id TEXT,
            instance_id TEXT NOT NULL,
            grant_id TEXT REFERENCES directory_grants(grant_id) ON DELETE CASCADE,
            resource_kind TEXT NOT NULL CHECK(resource_kind IN ('directory','file','asset')),
            relative_path TEXT,
            asset_id INTEGER,
            file_identity TEXT,
            file_size INTEGER,
            modified_at INTEGER,
            purpose TEXT NOT NULL CHECK(purpose IN ('import_source','export_destination')),
            access_mask INTEGER NOT NULL CHECK(access_mask BETWEEN 1 AND 7),
            created_at INTEGER NOT NULL,
            expires_at INTEGER,
            revoked_at INTEGER,
            grant_epoch TEXT NOT NULL,
            library_id TEXT NOT NULL,
            library_epoch TEXT NOT NULL,
            CHECK(
                (resource_kind='directory' AND grant_id IS NOT NULL
                    AND relative_path IS NULL AND asset_id IS NULL)
                OR
                (resource_kind='file' AND grant_id IS NOT NULL
                    AND relative_path IS NOT NULL AND asset_id IS NULL
                    AND file_identity IS NOT NULL)
                OR
                (resource_kind='asset' AND grant_id IS NULL
                    AND relative_path IS NULL AND asset_id IS NOT NULL)
            )
        );
        CREATE INDEX IF NOT EXISTS idx_resource_handles_grant
            ON resource_handles(grant_id, revoked_at);
        CREATE INDEX IF NOT EXISTS idx_resource_handles_asset
            ON resource_handles(asset_id, revoked_at);
        ",
    )?;
    Ok(())
}

pub fn ensure_principal(
    conn: &Connection,
    principal_id: &str,
    now: i64,
) -> AppResult<PrincipalAuthorization> {
    conn.execute(
        "INSERT OR IGNORE INTO authorization_principals(
            principal_id,grant_epoch,created_at,updated_at
         ) VALUES(?1,?2,?3,?3)",
        rusqlite::params![principal_id, uuid::Uuid::new_v4().simple().to_string(), now],
    )?;
    principal(conn, principal_id)?
        .ok_or_else(|| crate::error::AppError::internal("授权主体记录缺失"))
}

pub fn principal(
    conn: &Connection,
    principal_id: &str,
) -> AppResult<Option<PrincipalAuthorization>> {
    Ok(conn
        .query_row(
            "SELECT principal_id,grant_epoch
               FROM authorization_principals
              WHERE principal_id=?1",
            [principal_id],
            |row| {
                Ok(PrincipalAuthorization {
                    principal_id: row.get(0)?,
                    grant_epoch: row.get(1)?,
                })
            },
        )
        .optional()?)
}

pub fn insert_directory_grant(conn: &Connection, grant: &NewDirectoryGrant) -> AppResult<()> {
    conn.execute(
        "INSERT INTO directory_grants(
            grant_id,principal_id,owner_session_id,owner_instance_id,purpose,
            canonical_path,path_identity,display_name,recursive,remembered,
            created_at,expires_at,grant_epoch,library_id,library_epoch
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15)",
        rusqlite::params![
            grant.grant_id,
            grant.principal_id,
            grant.owner_session_id,
            grant.owner_instance_id,
            grant.purpose,
            grant.canonical_path,
            grant.path_identity,
            grant.display_name,
            grant.recursive,
            grant.remembered,
            grant.created_at,
            grant.expires_at,
            grant.grant_epoch,
            grant.library_id,
            grant.library_epoch,
        ],
    )?;
    Ok(())
}

pub fn directory_grant(conn: &Connection, grant_id: &str) -> AppResult<Option<DirectoryGrant>> {
    Ok(conn
        .query_row(
            "SELECT grant_id,principal_id,owner_session_id,owner_instance_id,purpose,
                    canonical_path,path_identity,display_name,recursive,remembered,
                    created_at,expires_at,revoked_at,grant_epoch,library_id,library_epoch
               FROM directory_grants
              WHERE grant_id=?1",
            [grant_id],
            |row| {
                Ok(DirectoryGrant {
                    grant_id: row.get(0)?,
                    principal_id: row.get(1)?,
                    owner_session_id: row.get(2)?,
                    owner_instance_id: row.get(3)?,
                    purpose: row.get(4)?,
                    canonical_path: row.get(5)?,
                    path_identity: row.get(6)?,
                    display_name: row.get(7)?,
                    recursive: row.get(8)?,
                    remembered: row.get(9)?,
                    created_at: row.get(10)?,
                    expires_at: row.get(11)?,
                    revoked_at: row.get(12)?,
                    grant_epoch: row.get(13)?,
                    library_id: row.get(14)?,
                    library_epoch: row.get(15)?,
                })
            },
        )
        .optional()?)
}

pub fn insert_handle(conn: &Connection, handle: &NewResourceHandle) -> AppResult<()> {
    conn.execute(
        "INSERT INTO resource_handles(
            handle_id,principal_id,session_id,instance_id,grant_id,resource_kind,
            relative_path,asset_id,file_identity,file_size,modified_at,purpose,
            access_mask,created_at,expires_at,grant_epoch,library_id,library_epoch
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
        rusqlite::params![
            handle.handle_id,
            handle.principal_id,
            handle.session_id,
            handle.instance_id,
            handle.grant_id,
            handle.resource_kind,
            handle.relative_path,
            handle.asset_id,
            handle.file_identity,
            handle.file_size,
            handle.modified_at,
            handle.purpose,
            handle.access_mask,
            handle.created_at,
            handle.expires_at,
            handle.grant_epoch,
            handle.library_id,
            handle.library_epoch,
        ],
    )?;
    Ok(())
}

pub fn handle(conn: &Connection, handle_id: &str) -> AppResult<Option<ResourceHandle>> {
    Ok(conn
        .query_row(
            "SELECT handle_id,principal_id,session_id,instance_id,grant_id,resource_kind,
                    relative_path,asset_id,file_identity,file_size,modified_at,purpose,
                    access_mask,created_at,expires_at,revoked_at,grant_epoch,library_id,
                    library_epoch
               FROM resource_handles
              WHERE handle_id=?1",
            [handle_id],
            |row| {
                Ok(ResourceHandle {
                    handle_id: row.get(0)?,
                    principal_id: row.get(1)?,
                    session_id: row.get(2)?,
                    instance_id: row.get(3)?,
                    grant_id: row.get(4)?,
                    resource_kind: row.get(5)?,
                    relative_path: row.get(6)?,
                    asset_id: row.get(7)?,
                    file_identity: row.get(8)?,
                    file_size: row.get(9)?,
                    modified_at: row.get(10)?,
                    purpose: row.get(11)?,
                    access_mask: row.get(12)?,
                    created_at: row.get(13)?,
                    expires_at: row.get(14)?,
                    revoked_at: row.get(15)?,
                    grant_epoch: row.get(16)?,
                    library_id: row.get(17)?,
                    library_epoch: row.get(18)?,
                })
            },
        )
        .optional()?)
}

/// Revoke one grant and advance the principal generation in one transaction.
/// The generation bump invalidates every older handle, including handles issued
/// for other grants that belong to the same principal.
pub fn revoke_grant(
    conn: &Connection,
    principal_id: &str,
    grant_id: &str,
    now: i64,
) -> AppResult<()> {
    let transaction = conn.unchecked_transaction()?;
    let changed = transaction.execute(
        "UPDATE directory_grants
            SET revoked_at=?1
          WHERE grant_id=?2 AND principal_id=?3 AND revoked_at IS NULL",
        rusqlite::params![now, grant_id, principal_id],
    )?;
    if changed != 1 {
        return Err(crate::error::AppError::from(
            crate::contracts::agent::AgentErrorCode::ReferenceExpired,
        ));
    }
    transaction.execute(
        "UPDATE resource_handles
            SET revoked_at=COALESCE(revoked_at,?1)
          WHERE grant_id=?2 AND principal_id=?3",
        rusqlite::params![now, grant_id, principal_id],
    )?;
    advance_generation(&transaction, principal_id, now)?;
    transaction.commit()?;
    Ok(())
}

pub fn revoke_all(conn: &Connection, principal_id: &str, now: i64) -> AppResult<()> {
    let transaction = conn.unchecked_transaction()?;
    transaction.execute(
        "UPDATE directory_grants
            SET revoked_at=COALESCE(revoked_at,?1)
          WHERE principal_id=?2",
        rusqlite::params![now, principal_id],
    )?;
    transaction.execute(
        "UPDATE resource_handles
            SET revoked_at=COALESCE(revoked_at,?1)
          WHERE principal_id=?2",
        rusqlite::params![now, principal_id],
    )?;
    advance_generation(&transaction, principal_id, now)?;
    transaction.commit()?;
    Ok(())
}

fn advance_generation(
    transaction: &Transaction<'_>,
    principal_id: &str,
    now: i64,
) -> AppResult<String> {
    let next = uuid::Uuid::new_v4().simple().to_string();
    let changed = transaction.execute(
        "UPDATE authorization_principals
            SET grant_epoch=?1,updated_at=?2
          WHERE principal_id=?3",
        rusqlite::params![next, now, principal_id],
    )?;
    if changed != 1 {
        return Err(crate::error::AppError::from(
            crate::contracts::agent::AgentErrorCode::Unauthorized,
        ));
    }
    Ok(next)
}

pub fn list_remembered_grants(
    conn: &Connection,
    principal_id: &str,
) -> AppResult<Vec<DirectoryGrant>> {
    let mut statement = conn.prepare(
        "SELECT grant_id,principal_id,owner_session_id,owner_instance_id,purpose,
                canonical_path,path_identity,display_name,recursive,remembered,
                created_at,expires_at,revoked_at,grant_epoch,library_id,library_epoch
           FROM directory_grants
          WHERE principal_id=?1 AND remembered=1 AND revoked_at IS NULL
          ORDER BY created_at DESC",
    )?;
    let rows = statement.query_map(rusqlite::params![principal_id], |row| {
        Ok(DirectoryGrant {
            grant_id: row.get(0)?,
            principal_id: row.get(1)?,
            owner_session_id: row.get(2)?,
            owner_instance_id: row.get(3)?,
            purpose: row.get(4)?,
            canonical_path: row.get(5)?,
            path_identity: row.get(6)?,
            display_name: row.get(7)?,
            recursive: row.get(8)?,
            remembered: row.get(9)?,
            created_at: row.get(10)?,
            expires_at: row.get(11)?,
            revoked_at: row.get(12)?,
            grant_epoch: row.get(13)?,
            library_id: row.get(14)?,
            library_epoch: row.get(15)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
