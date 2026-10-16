//! Persisted conversation metadata, drafts, attachments, and stable links.
//!
//! This module owns SQL only. The Pi transcript remains the single source of
//! conversational context; these rows are a rebuildable host projection. It
//! never stores a second copy of the transcript and never deletes assets.

use rusqlite::{Connection, OptionalExtension};

use crate::error::AppResult;

const TERMINAL_OPERATION_STATES: &str = "'succeeded','partially_succeeded','failed','cancelled'";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationRecord {
    pub conversation_id: String,
    pub principal_id: String,
    pub title: String,
    pub status: String,
    pub created_at: i64,
    pub updated_at: i64,
    pub archived_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRecord {
    pub conversation_id: String,
    pub draft_kind: String,
    pub payload: String,
    pub revision: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentRecord {
    pub attachment_id: String,
    pub conversation_id: String,
    pub asset_id: i64,
    pub display_name: String,
    pub created_at: i64,
    pub available: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRecord {
    pub conversation_id: String,
    pub link_kind: String,
    pub link_ref: String,
}

pub(super) fn migrate_structure(conn: &Connection) -> AppResult<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS conversations (
            conversation_id TEXT PRIMARY KEY,
            principal_id TEXT NOT NULL REFERENCES authorization_principals(principal_id)
                ON DELETE CASCADE,
            title TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('active','archived')),
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            archived_at INTEGER,
            deleted_at INTEGER
        );
        CREATE INDEX IF NOT EXISTS idx_conversations_principal
            ON conversations(principal_id, status, deleted_at, updated_at DESC);
        CREATE TABLE IF NOT EXISTS conversation_drafts (
            conversation_id TEXT NOT NULL REFERENCES conversations(conversation_id)
                ON DELETE CASCADE,
            draft_kind TEXT NOT NULL,
            payload TEXT NOT NULL,
            revision INTEGER NOT NULL,
            updated_at INTEGER NOT NULL,
            PRIMARY KEY(conversation_id, draft_kind)
        );
        CREATE TABLE IF NOT EXISTS conversation_attachments (
            attachment_id TEXT PRIMARY KEY,
            conversation_id TEXT NOT NULL REFERENCES conversations(conversation_id)
                ON DELETE CASCADE,
            asset_id INTEGER NOT NULL,
            display_name TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS idx_conversation_attachments_conversation
            ON conversation_attachments(conversation_id);
        CREATE TABLE IF NOT EXISTS conversation_links (
            conversation_id TEXT NOT NULL REFERENCES conversations(conversation_id)
                ON DELETE CASCADE,
            link_kind TEXT NOT NULL CHECK(link_kind IN ('plan','operation','batch')),
            link_ref TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            PRIMARY KEY(conversation_id, link_kind, link_ref),
            UNIQUE(link_kind, link_ref)
        );
        ",
    )?;
    Ok(())
}

pub fn insert_conversation(
    conn: &Connection,
    conversation_id: &str,
    principal_id: &str,
    title: &str,
    now: i64,
) -> AppResult<()> {
    conn.execute(
        "INSERT INTO conversations(
            conversation_id,principal_id,title,status,created_at,updated_at
         ) VALUES(?1,?2,?3,'active',?4,?4)",
        rusqlite::params![conversation_id, principal_id, title, now],
    )?;
    Ok(())
}

fn map_conversation(row: &rusqlite::Row<'_>) -> rusqlite::Result<ConversationRecord> {
    Ok(ConversationRecord {
        conversation_id: row.get(0)?,
        principal_id: row.get(1)?,
        title: row.get(2)?,
        status: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        archived_at: row.get(6)?,
    })
}

pub fn conversation(
    conn: &Connection,
    conversation_id: &str,
) -> AppResult<Option<ConversationRecord>> {
    Ok(conn
        .query_row(
            "SELECT conversation_id,principal_id,title,status,created_at,updated_at,archived_at
               FROM conversations
              WHERE conversation_id=?1 AND deleted_at IS NULL",
            [conversation_id],
            map_conversation,
        )
        .optional()?)
}

/// Live conversations for one principal. `query` filters the rebuildable title
/// projection; it never searches transcript content.
pub fn list_conversations(
    conn: &Connection,
    principal_id: &str,
    include_archived: bool,
    query: Option<&str>,
) -> AppResult<Vec<ConversationRecord>> {
    let pattern = query
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("%{value}%"));
    let mut statement = conn.prepare(
        "SELECT conversation_id,principal_id,title,status,created_at,updated_at,archived_at
           FROM conversations
          WHERE principal_id=?1 AND deleted_at IS NULL
            AND (?2=1 OR status='active')
            AND (?3 IS NULL OR title LIKE ?3)
          ORDER BY (status='archived'), updated_at DESC, conversation_id",
    )?;
    let rows = statement.query_map(
        rusqlite::params![principal_id, include_archived, pattern],
        map_conversation,
    )?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn rename_conversation(
    conn: &Connection,
    conversation_id: &str,
    title: &str,
    now: i64,
) -> AppResult<bool> {
    let changed = conn.execute(
        "UPDATE conversations SET title=?1,updated_at=?2
          WHERE conversation_id=?3 AND deleted_at IS NULL",
        rusqlite::params![title, now, conversation_id],
    )?;
    Ok(changed == 1)
}

pub fn set_archived(
    conn: &Connection,
    conversation_id: &str,
    archived: bool,
    now: i64,
) -> AppResult<bool> {
    let changed = conn.execute(
        "UPDATE conversations
            SET status=?1,archived_at=?2,updated_at=?3
          WHERE conversation_id=?4 AND deleted_at IS NULL",
        rusqlite::params![
            if archived { "archived" } else { "active" },
            archived.then_some(now),
            now,
            conversation_id,
        ],
    )?;
    Ok(changed == 1)
}

/// Live count of unfinished operations linked to a conversation. Used to stop
/// deletion before it would orphan an in-flight task.
pub fn unfinished_operation_count(conn: &Connection, conversation_id: &str) -> AppResult<i64> {
    Ok(conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM conversation_links cl
              JOIN operations o ON o.operation_id=cl.link_ref
             WHERE cl.conversation_id=?1 AND cl.link_kind='operation'
               AND o.state NOT IN ({TERMINAL_OPERATION_STATES})"
        ),
        [conversation_id],
        |row| row.get(0),
    )?)
}

pub fn soft_delete_conversation(
    conn: &Connection,
    conversation_id: &str,
    now: i64,
) -> AppResult<bool> {
    let changed = conn.execute(
        "UPDATE conversations SET deleted_at=?1,updated_at=?1
          WHERE conversation_id=?2 AND deleted_at IS NULL",
        rusqlite::params![now, conversation_id],
    )?;
    Ok(changed == 1)
}

pub fn upsert_draft(
    conn: &Connection,
    conversation_id: &str,
    kind: &str,
    payload: &str,
    now: i64,
) -> AppResult<i64> {
    conn.execute(
        "INSERT INTO conversation_drafts(conversation_id,draft_kind,payload,revision,updated_at)
         VALUES(?1,?2,?3,1,?4)
         ON CONFLICT(conversation_id,draft_kind) DO UPDATE SET
            payload=excluded.payload,
            revision=conversation_drafts.revision+1,
            updated_at=excluded.updated_at",
        rusqlite::params![conversation_id, kind, payload, now],
    )?;
    conn.execute(
        "UPDATE conversations SET updated_at=?1 WHERE conversation_id=?2",
        rusqlite::params![now, conversation_id],
    )?;
    let revision: i64 = conn.query_row(
        "SELECT revision FROM conversation_drafts
          WHERE conversation_id=?1 AND draft_kind=?2",
        rusqlite::params![conversation_id, kind],
        |row| row.get(0),
    )?;
    Ok(revision)
}

pub fn drafts(conn: &Connection, conversation_id: &str) -> AppResult<Vec<DraftRecord>> {
    let mut statement = conn.prepare(
        "SELECT conversation_id,draft_kind,payload,revision,updated_at
           FROM conversation_drafts
          WHERE conversation_id=?1 ORDER BY draft_kind",
    )?;
    let rows = statement.query_map([conversation_id], |row| {
        Ok(DraftRecord {
            conversation_id: row.get(0)?,
            draft_kind: row.get(1)?,
            payload: row.get(2)?,
            revision: row.get(3)?,
            updated_at: row.get(4)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

pub fn insert_attachment(
    conn: &Connection,
    attachment_id: &str,
    conversation_id: &str,
    asset_id: i64,
    display_name: &str,
    now: i64,
) -> AppResult<()> {
    conn.execute(
        "INSERT INTO conversation_attachments(
            attachment_id,conversation_id,asset_id,display_name,created_at
         ) VALUES(?1,?2,?3,?4,?5)",
        rusqlite::params![attachment_id, conversation_id, asset_id, display_name, now],
    )?;
    Ok(())
}

/// Attachments expose availability only; resolving a missing file never
/// re-grants directory access.
pub fn attachments(conn: &Connection, conversation_id: &str) -> AppResult<Vec<AttachmentRecord>> {
    use crate::db::libraries::VERIFIED_ASSET_SCOPE;
    let mut statement = conn.prepare(&format!(
        "SELECT ca.attachment_id,ca.conversation_id,ca.asset_id,ca.display_name,ca.created_at,
                EXISTS(SELECT 1 FROM assets a WHERE a.id=ca.asset_id AND {VERIFIED_ASSET_SCOPE})
           FROM conversation_attachments ca
          WHERE ca.conversation_id=?1
          ORDER BY ca.created_at,ca.attachment_id"
    ))?;
    let rows = statement.query_map([conversation_id], |row| {
        Ok(AttachmentRecord {
            attachment_id: row.get(0)?,
            conversation_id: row.get(1)?,
            asset_id: row.get(2)?,
            display_name: row.get(3)?,
            created_at: row.get(4)?,
            available: row.get::<_, i64>(5)? != 0,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}

/// Bind a plan/operation/batch to a conversation on explicit handoff. The
/// unique `(link_kind, link_ref)` guard keeps one task in one conversation.
pub fn link(
    conn: &Connection,
    conversation_id: &str,
    link_kind: &str,
    link_ref: &str,
    now: i64,
) -> AppResult<()> {
    conn.execute(
        "INSERT OR IGNORE INTO conversation_links(
            conversation_id,link_kind,link_ref,created_at
         ) VALUES(?1,?2,?3,?4)",
        rusqlite::params![conversation_id, link_kind, link_ref, now],
    )?;
    Ok(())
}

pub fn links(conn: &Connection, conversation_id: &str) -> AppResult<Vec<LinkRecord>> {
    let mut statement = conn.prepare(
        "SELECT conversation_id,link_kind,link_ref
           FROM conversation_links
          WHERE conversation_id=?1 ORDER BY link_kind,link_ref",
    )?;
    let rows = statement.query_map([conversation_id], |row| {
        Ok(LinkRecord {
            conversation_id: row.get(0)?,
            link_kind: row.get(1)?,
            link_ref: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<Result<Vec<_>, _>>()?)
}
