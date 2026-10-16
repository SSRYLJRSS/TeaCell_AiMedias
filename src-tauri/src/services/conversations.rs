//! Host conversation index and persistent workbench drafts.
//!
//! The Pi transcript stays the single source of conversational context. This
//! service stores only a rebuildable projection: titles, draft payloads,
//! attachment references, and explicit plan/operation/batch handoff links.
//! Reads and writes are scoped to the authenticated principal; deleting a
//! conversation never deletes assets, tags, or required operation receipts.

use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::Value;

use crate::contracts::agent::{AgentErrorCode, ResourceRef};
use crate::contracts::caller::CallerContext;
use crate::db::authorization;
use crate::db::conversations::{self, AttachmentRecord, ConversationRecord, DraftRecord};
use crate::db::libraries;
use crate::error::{AppError, AppResult};
use crate::state::Database;

const TITLE_MAX_BYTES: usize = 256;
const DRAFT_MAX_BYTES: usize = 64 * 1024;
const DRAFT_KINDS: [&str; 5] = ["import", "tagging", "export", "workbench", "assistant"];

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationSummary {
    pub conversation_ref: ResourceRef,
    pub title: String,
    pub status: String,
    pub created_at_unix_seconds: i64,
    pub updated_at_unix_seconds: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub archived_at_unix_seconds: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DraftView {
    pub kind: String,
    pub payload: Value,
    pub revision: i64,
    pub updated_at_unix_seconds: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentView {
    pub attachment_ref: ResourceRef,
    pub asset_id: i64,
    pub display_name: String,
    pub available: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkView {
    pub kind: String,
    pub reference: ResourceRef,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationView {
    pub conversation_ref: ResourceRef,
    pub title: String,
    pub status: String,
    pub created_at_unix_seconds: i64,
    pub updated_at_unix_seconds: i64,
    pub drafts: Vec<DraftView>,
    pub attachments: Vec<AttachmentView>,
    pub links: Vec<LinkView>,
}

pub fn create_conversation(
    db: &Database,
    caller: &CallerContext,
    title: &str,
) -> AppResult<ConversationSummary> {
    let title = validate_title(title)?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let conversation_ref = ResourceRef::generated("conversation");
    let now = now_unix_seconds()?;
    conversations::insert_conversation(
        &conn,
        conversation_ref.as_str(),
        caller.principal_id.as_str(),
        &title,
        now,
    )?;
    Ok(ConversationSummary {
        conversation_ref,
        title,
        status: "active".to_string(),
        created_at_unix_seconds: now,
        updated_at_unix_seconds: now,
        archived_at_unix_seconds: None,
    })
}

pub fn list_conversations(
    db: &Database,
    caller: &CallerContext,
    include_archived: bool,
    query: Option<&str>,
) -> AppResult<Vec<ConversationSummary>> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    conversations::list_conversations(&conn, caller.principal_id.as_str(), include_archived, query)
        .map(|records| records.iter().map(summary_of).collect())
}

pub fn get_conversation(
    db: &Database,
    caller: &CallerContext,
    conversation_ref: &ResourceRef,
) -> AppResult<ConversationView> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let record = load_owned(&conn, caller, conversation_ref)?;
    let drafts = conversations::drafts(&conn, record.conversation_id.as_str())?
        .iter()
        .map(draft_view)
        .collect::<AppResult<Vec<_>>>()?;
    let attachments = conversations::attachments(&conn, record.conversation_id.as_str())?
        .iter()
        .map(attachment_view)
        .collect::<AppResult<Vec<_>>>()?;
    let links = conversations::links(&conn, record.conversation_id.as_str())?
        .iter()
        .map(|link| {
            Ok(LinkView {
                kind: link.link_kind.clone(),
                reference: ResourceRef::from_trusted(link.link_ref.clone())?,
            })
        })
        .collect::<AppResult<Vec<_>>>()?;
    Ok(ConversationView {
        conversation_ref: ResourceRef::from_trusted(record.conversation_id.clone())?,
        title: record.title.clone(),
        status: record.status.clone(),
        created_at_unix_seconds: record.created_at,
        updated_at_unix_seconds: record.updated_at,
        drafts,
        attachments,
        links,
    })
}

pub fn rename_conversation(
    db: &Database,
    caller: &CallerContext,
    conversation_ref: &ResourceRef,
    title: &str,
) -> AppResult<ConversationSummary> {
    let title = validate_title(title)?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    load_owned(&conn, caller, conversation_ref)?;
    if !conversations::rename_conversation(
        &conn,
        conversation_ref.as_str(),
        &title,
        now_unix_seconds()?,
    )? {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    summary_from_record(&load_owned(&conn, caller, conversation_ref)?)
}

pub fn set_archived(
    db: &Database,
    caller: &CallerContext,
    conversation_ref: &ResourceRef,
    archived: bool,
) -> AppResult<ConversationSummary> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    load_owned(&conn, caller, conversation_ref)?;
    if !conversations::set_archived(
        &conn,
        conversation_ref.as_str(),
        archived,
        now_unix_seconds()?,
    )? {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    summary_from_record(&load_owned(&conn, caller, conversation_ref)?)
}

/// Deleting a conversation refuses while a linked operation is unfinished and
/// only soft-deletes the projection. Assets, tags, plans, operations and
/// receipts are left untouched.
pub fn delete_conversation(
    db: &Database,
    caller: &CallerContext,
    conversation_ref: &ResourceRef,
) -> AppResult<()> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    load_owned(&conn, caller, conversation_ref)?;
    if conversations::unfinished_operation_count(&conn, conversation_ref.as_str())? > 0 {
        return Err(AppError::from(AgentErrorCode::ResourceConflict));
    }
    if !conversations::soft_delete_conversation(
        &conn,
        conversation_ref.as_str(),
        now_unix_seconds()?,
    )? {
        return Err(AppError::from(AgentErrorCode::ReferenceExpired));
    }
    Ok(())
}

pub fn save_draft(
    db: &Database,
    caller: &CallerContext,
    conversation_ref: &ResourceRef,
    kind: &str,
    payload: &Value,
) -> AppResult<DraftView> {
    validate_draft_kind(kind)?;
    let encoded = validate_draft_payload(payload)?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    load_owned(&conn, caller, conversation_ref)?;
    let now = now_unix_seconds()?;
    let revision =
        conversations::upsert_draft(&conn, conversation_ref.as_str(), kind, &encoded, now)?;
    Ok(DraftView {
        kind: kind.to_string(),
        payload: payload.clone(),
        revision,
        updated_at_unix_seconds: now,
    })
}

pub fn attach_asset(
    db: &Database,
    caller: &CallerContext,
    conversation_ref: &ResourceRef,
    asset_id: i64,
    display_name: &str,
) -> AppResult<AttachmentView> {
    if asset_id <= 0 {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let display_name = validate_title(display_name)?;
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    load_owned(&conn, caller, conversation_ref)?;
    let attachment_id = ResourceRef::generated("attachment");
    let now = now_unix_seconds()?;
    conversations::insert_attachment(
        &conn,
        attachment_id.as_str(),
        conversation_ref.as_str(),
        asset_id,
        &display_name,
        now,
    )?;
    let record = conversations::attachments(&conn, conversation_ref.as_str())?
        .into_iter()
        .find(|item| item.attachment_id == attachment_id.as_str())
        .ok_or_else(|| AppError::internal("附件登记后无法读取"))?;
    attachment_view(&record)
}

/// Explicit handoff: bind a plan/operation/batch to a conversation. Creating
/// work without a conversation stays valid; only this call links them.
pub fn link(
    db: &Database,
    caller: &CallerContext,
    conversation_ref: &ResourceRef,
    kind: &str,
    reference: &ResourceRef,
) -> AppResult<()> {
    if !matches!(kind, "plan" | "operation" | "batch") {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    load_owned(&conn, caller, conversation_ref)?;
    conversations::link(
        &conn,
        conversation_ref.as_str(),
        kind,
        reference.as_str(),
        now_unix_seconds()?,
    )?;
    Ok(())
}

fn load_owned(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
    conversation_ref: &ResourceRef,
) -> AppResult<ConversationRecord> {
    let record = conversations::conversation(conn, conversation_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if record.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    Ok(record)
}

fn summary_of(record: &ConversationRecord) -> ConversationSummary {
    ConversationSummary {
        conversation_ref: ResourceRef::from_trusted(record.conversation_id.clone())
            .expect("persisted conversation ids are host-generated"),
        title: record.title.clone(),
        status: record.status.clone(),
        created_at_unix_seconds: record.created_at,
        updated_at_unix_seconds: record.updated_at,
        archived_at_unix_seconds: record.archived_at,
    }
}

fn summary_from_record(record: &ConversationRecord) -> AppResult<ConversationSummary> {
    Ok(ConversationSummary {
        conversation_ref: ResourceRef::from_trusted(record.conversation_id.clone())?,
        title: record.title.clone(),
        status: record.status.clone(),
        created_at_unix_seconds: record.created_at,
        updated_at_unix_seconds: record.updated_at,
        archived_at_unix_seconds: record.archived_at,
    })
}

fn draft_view(record: &DraftRecord) -> AppResult<DraftView> {
    let payload: Value = serde_json::from_str(&record.payload)
        .map_err(|_| AppError::internal("会话草稿载荷损坏"))?;
    Ok(DraftView {
        kind: record.draft_kind.clone(),
        payload,
        revision: record.revision,
        updated_at_unix_seconds: record.updated_at,
    })
}

fn attachment_view(record: &AttachmentRecord) -> AppResult<AttachmentView> {
    Ok(AttachmentView {
        attachment_ref: ResourceRef::from_trusted(record.attachment_id.clone())?,
        asset_id: record.asset_id,
        display_name: record.display_name.clone(),
        available: record.available,
    })
}

fn validate_title(title: &str) -> AppResult<String> {
    let title = title.trim();
    if title.is_empty() || title.len() > TITLE_MAX_BYTES || title.contains('\0') {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    Ok(title.to_string())
}

fn validate_draft_kind(kind: &str) -> AppResult<()> {
    if DRAFT_KINDS.contains(&kind) {
        Ok(())
    } else {
        Err(AppError::from(AgentErrorCode::InvalidInput))
    }
}

fn validate_draft_payload(payload: &Value) -> AppResult<String> {
    if !payload.is_object() {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let encoded =
        serde_json::to_string(payload).map_err(|_| AppError::from(AgentErrorCode::InvalidInput))?;
    if encoded.len() > DRAFT_MAX_BYTES {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    Ok(encoded)
}

fn validate_caller(conn: &rusqlite::Connection, caller: &CallerContext) -> AppResult<()> {
    let principal = authorization::principal(conn, caller.principal_id.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::GrantRevoked))?;
    if principal.grant_epoch != caller.grant_epoch.as_str() {
        return Err(AppError::from(AgentErrorCode::GrantRevoked));
    }
    let library =
        libraries::current(conn)?.ok_or_else(|| AppError::from(AgentErrorCode::LibraryOffline))?;
    if library.health != libraries::LibraryHealth::Ready {
        return Err(AppError::from(AgentErrorCode::LibraryOffline));
    }
    if library.library_id != caller.library.library_id.as_str()
        || library.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(())
}

fn now_unix_seconds() -> AppResult<i64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .map_err(|_| AppError::internal("系统时间早于 Unix 纪元"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_memory, plans as plans_db};
    use crate::services::authorization::{self, TrustedAdapterIdentity};
    use crate::services::managed_library;
    use serde_json::json;

    fn ready_db(root: &std::path::Path) -> Database {
        let conn = init_memory().unwrap();
        managed_library::adopt_initial_root(&conn, root).unwrap();
        Database::new(conn)
    }

    fn gui_caller(db: &Database) -> CallerContext {
        let conn = db.lock().unwrap();
        authorization::build_caller_context(
            &conn,
            TrustedAdapterIdentity::Gui {
                session_id: "session_conv_fixture".to_string(),
                instance_id: "instance_conv_fixture".to_string(),
            },
            ResourceRef::generated("request"),
            Vec::new(),
        )
        .unwrap()
    }

    fn error_code(error: AppError) -> String {
        error.code().to_string()
    }

    #[test]
    fn two_conversations_keep_separate_drafts_and_links() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let first = create_conversation(&db, &caller, "第一个会话").unwrap();
        let second = create_conversation(&db, &caller, "第二个会话").unwrap();

        save_draft(
            &db,
            &caller,
            &first.conversation_ref,
            "import",
            &json!({ "fileNames": ["a.jpg"] }),
        )
        .unwrap();
        save_draft(
            &db,
            &caller,
            &second.conversation_ref,
            "import",
            &json!({ "fileNames": ["b.jpg", "c.jpg"] }),
        )
        .unwrap();

        let first_view = get_conversation(&db, &caller, &first.conversation_ref).unwrap();
        let second_view = get_conversation(&db, &caller, &second.conversation_ref).unwrap();
        assert_eq!(first_view.drafts[0].payload["fileNames"], json!(["a.jpg"]));
        assert_eq!(
            second_view.drafts[0].payload["fileNames"],
            json!(["b.jpg", "c.jpg"])
        );
        assert_eq!(first_view.drafts[0].revision, 1);
    }

    #[test]
    fn draft_persists_across_reads_and_bumps_revision() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "草稿").unwrap();
        save_draft(
            &db,
            &caller,
            &conversation.conversation_ref,
            "workbench",
            &json!({ "step": 1 }),
        )
        .unwrap();
        let saved = save_draft(
            &db,
            &caller,
            &conversation.conversation_ref,
            "workbench",
            &json!({ "step": 2 }),
        )
        .unwrap();
        assert_eq!(saved.revision, 2);
        // A fresh read rebuilds the projection from the stable conversation id.
        let reopened = get_conversation(&db, &caller, &conversation.conversation_ref).unwrap();
        assert_eq!(reopened.drafts.len(), 1);
        assert_eq!(reopened.drafts[0].payload["step"], json!(2));
    }

    #[test]
    fn invalid_draft_kind_and_payload_are_rejected() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "校验").unwrap();
        assert_eq!(
            error_code(
                save_draft(
                    &db,
                    &caller,
                    &conversation.conversation_ref,
                    "unknown",
                    &json!({}),
                )
                .unwrap_err()
            ),
            "INVALID_INPUT"
        );
        assert_eq!(
            error_code(
                save_draft(
                    &db,
                    &caller,
                    &conversation.conversation_ref,
                    "import",
                    &json!(["not", "an", "object"]),
                )
                .unwrap_err()
            ),
            "INVALID_INPUT"
        );
    }

    #[test]
    fn another_principal_cannot_read_or_edit_a_conversation() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let owner = gui_caller(&db);
        let conversation = create_conversation(&db, &owner, "私有").unwrap();
        let other = {
            let conn = db.lock().unwrap();
            authorization::build_caller_context(
                &conn,
                TrustedAdapterIdentity::Pi {
                    principal_id: "principal_pi_other".to_string(),
                    session_id: "session_pi_other".to_string(),
                    instance_id: "instance_pi_other".to_string(),
                },
                ResourceRef::generated("request"),
                Vec::new(),
            )
            .unwrap()
        };
        assert_eq!(
            error_code(get_conversation(&db, &other, &conversation.conversation_ref).unwrap_err()),
            "UNAUTHORIZED"
        );
        assert_eq!(
            error_code(
                save_draft(
                    &db,
                    &other,
                    &conversation.conversation_ref,
                    "import",
                    &json!({}),
                )
                .unwrap_err()
            ),
            "UNAUTHORIZED"
        );
    }

    #[test]
    fn archive_rename_and_search_project_from_stable_ids() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "旧标题").unwrap();
        rename_conversation(&db, &caller, &conversation.conversation_ref, "新标题").unwrap();
        assert_eq!(
            list_conversations(&db, &caller, false, Some("新标题"))
                .unwrap()
                .len(),
            1
        );
        set_archived(&db, &caller, &conversation.conversation_ref, true).unwrap();
        assert!(list_conversations(&db, &caller, false, None)
            .unwrap()
            .is_empty());
        assert_eq!(
            list_conversations(&db, &caller, true, None).unwrap().len(),
            1
        );
    }

    #[test]
    fn delete_is_blocked_by_unfinished_operation_and_keeps_rows_otherwise() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "删除").unwrap();
        let operation_ref = ResourceRef::generated("operation");
        {
            let conn = db.lock().unwrap();
            plans_db::insert_operation(
                &conn,
                &plans_db::NewOperation {
                    operation_id: operation_ref.as_str().to_string(),
                    step_id: ResourceRef::generated("step").as_str().to_string(),
                    plan_id: ResourceRef::generated("plan").as_str().to_string(),
                    principal_id: caller.principal_id.as_str().to_string(),
                    capability: "imports.submit".to_string(),
                    idempotency_key: ResourceRef::generated("idempotency").as_str().to_string(),
                    library_id: caller.library.library_id.as_str().to_string(),
                    library_epoch: caller.library.library_epoch.as_str().to_string(),
                    state: "queued".to_string(),
                    phase: "ready".to_string(),
                    revision: 1,
                    total: Some(1),
                    completed: 0,
                    failed: 0,
                    created_at: now_unix_seconds().unwrap(),
                },
            )
            .unwrap();
        }
        link(
            &db,
            &caller,
            &conversation.conversation_ref,
            "operation",
            &operation_ref,
        )
        .unwrap();
        assert_eq!(
            error_code(
                delete_conversation(&db, &caller, &conversation.conversation_ref).unwrap_err()
            ),
            "RESOURCE_CONFLICT"
        );

        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE operations SET state='succeeded',phase='complete' WHERE operation_id=?1",
                [operation_ref.as_str()],
            )
            .unwrap();
        }
        delete_conversation(&db, &caller, &conversation.conversation_ref).unwrap();
        assert!(list_conversations(&db, &caller, true, None)
            .unwrap()
            .is_empty());
        // The operation receipt survives the conversation soft-delete.
        let conn = db.lock().unwrap();
        let operations: i64 = conn
            .query_row(
                "SELECT count(*) FROM operations WHERE operation_id=?1",
                [operation_ref.as_str()],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(operations, 1);
    }

    #[test]
    fn attachment_availability_is_reported_without_regranting() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "附件").unwrap();
        let attachment = attach_asset(
            &db,
            &caller,
            &conversation.conversation_ref,
            9999,
            "缺失素材.jpg",
        )
        .unwrap();
        assert!(!attachment.available, "缺失/未核验文件必须显式不可用");
        let view = get_conversation(&db, &caller, &conversation.conversation_ref).unwrap();
        assert_eq!(view.attachments.len(), 1);
        assert!(!view.attachments[0].available);
    }
}
