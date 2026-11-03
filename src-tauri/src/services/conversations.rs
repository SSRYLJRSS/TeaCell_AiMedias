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
/// 单个会话一次还原的工作卡片上限。计划按 created_at 倒序取最近的一批，避免历史很长
/// 的会话把整库计划都渲染出来。
const WORK_CARD_LIMIT: i64 = 50;

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

/// Explicit handoff: bind a plan/operation/batch/selection to a conversation.
/// Creating work without a conversation stays valid; only this call links them.
///
/// A3-08: `selection` links carry the immutable snapshot the user handed over.
/// Linking never rebinds or re-prepares it — the same reference keeps pointing at
/// the same ids until it expires.
pub fn link(
    db: &Database,
    caller: &CallerContext,
    conversation_ref: &ResourceRef,
    kind: &str,
    reference: &ResourceRef,
) -> AppResult<()> {
    if !matches!(kind, "plan" | "operation" | "batch" | "selection") {
        return Err(AppError::from(AgentErrorCode::InvalidInput));
    }
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    load_owned(&conn, caller, conversation_ref)?;
    if kind == "plan" {
        // 计划归属由可信后端裁决，前端不得直接指定任意 plan 挂到会话上。
        ensure_plan_linkable(&conn, caller, conversation_ref, reference)?;
    }
    if kind == "operation" {
        ensure_operation_linkable(&conn, caller, conversation_ref, reference)?;
    }
    conversations::link(
        &conn,
        conversation_ref.as_str(),
        kind,
        reference.as_str(),
        now_unix_seconds()?,
    )?;
    Ok(())
}

/// 计划归属裁决（P0-4 修复，A3-04 扩展）。
///
/// 前端只应在「本轮会话自己产生的计划」上请求链接，但归属不能靠时间差猜测；
/// 这里回读计划本身做硬校验：
/// - 来源必须可信：可信本地 GUI 会话准备的计划（origin_kind == "gui"/旧行 None）
///   要求 principal 与调用者一致；助手适配器准备的计划（origin_kind == "pi"）则要求
///   计划行上持久化的 `session_id` 恰好是本次要链接的会话 —— 该字段由
///   `plans::prepare_plan` 从 Pi 调用方上下文写入（Pi 的 session 就是会话引用），不是
///   前端传参，因此它是可信的归属依据；其它客户端（MCP 等）准备的计划一律拒绝。
/// - 计划必须仍处于 pending，避免把已审批/过期/被消费的计划挂过来。
/// - 计划必须属于当前调用者的库世代，杜绝跨库关联。
fn ensure_plan_linkable(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
    conversation_ref: &ResourceRef,
    plan_ref: &ResourceRef,
) -> AppResult<()> {
    // 独立入库工作台的「交给助手」交的是**尚未提交**的准备计划（`importplan_*`），
    // 它落在 `import_plans` 而不是审批用的 `plans`，因此必须单独裁决（M-08 真机走查暴露）。
    if plan_ref.as_str().starts_with(IMPORT_PLAN_REF_PREFIX) {
        return ensure_import_plan_linkable(conn, caller, plan_ref);
    }
    let record = crate::db::plans::plan(conn, plan_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    match record.origin_kind.as_deref() {
        Some("gui") | None => {
            if record.principal_id != caller.principal_id.as_str() {
                return Err(AppError::from(AgentErrorCode::Unauthorized));
            }
        }
        Some("pi") => {
            // 助手计划的归属由后端上下文裁决：只有「创建它的那个会话」能认领，
            // 且主体必须是内置助手，不能是别的 MCP 客户端冒用 pi 来源。
            if record.principal_id != crate::services::agent_runtime::PI_HOST_PRINCIPAL {
                return Err(AppError::from(AgentErrorCode::Unauthorized));
            }
            if record.session_id.as_deref() != Some(conversation_ref.as_str()) {
                return Err(AppError::from(AgentErrorCode::Unauthorized));
            }
        }
        Some(_) => return Err(AppError::from(AgentErrorCode::Unauthorized)),
    }
    if record.review_status != "pending" {
        return Err(AppError::from(AgentErrorCode::PlanChanged));
    }
    if record.library_id != caller.library.library_id.as_str()
        || record.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    Ok(())
}

/// 入库准备计划的引用前缀，由 `services/import_prepare.rs` 的
/// `ResourceRef::generated("importplan")` 生成；两种计划不能混为一谈。
const IMPORT_PLAN_REF_PREFIX: &str = "importplan_";

/// 未提交的入库准备计划也允许挂到会话上（`src/components/assistant/AssistantWorkbench.tsx`
/// 与入库页共用同一份草稿，所以「同一业务任务」不是靠这里读出来的，链接只记录归属）。
/// 归属规则与审批计划同源：主体必须一致、库世代必须一致、计划必须尚未过期。
fn ensure_import_plan_linkable(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
    plan_ref: &ResourceRef,
) -> AppResult<()> {
    let record = crate::db::import_plans::plan(conn, plan_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if record.principal_id != caller.principal_id.as_str() {
        return Err(AppError::from(AgentErrorCode::Unauthorized));
    }
    if record.library_id != caller.library.library_id.as_str()
        || record.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    if record.expires_at <= now_unix_seconds()? {
        return Err(AppError::from(AgentErrorCode::PlanExpired));
    }
    Ok(())
}

/// 任务归属裁决。操作行本身不存会话，只能经它所属的计划还原，因此任务可挂到会话
/// 的前提是「该计划已经可以挂到这个会话」，规则与 `ensure_plan_linkable` 完全一致；
/// 否则前端可以把任意 operation 引用塞进任意会话。
fn ensure_operation_linkable(
    conn: &rusqlite::Connection,
    caller: &CallerContext,
    conversation_ref: &ResourceRef,
    operation_ref: &ResourceRef,
) -> AppResult<()> {
    let record = crate::db::plans::operation(conn, operation_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    if record.library_id != caller.library.library_id.as_str()
        || record.library_epoch != caller.library.library_epoch.as_str()
    {
        return Err(AppError::from(AgentErrorCode::LibraryChanged));
    }
    let plan_ref = ResourceRef::from_trusted(record.plan_id.clone())?;
    // 已审批/已被消费的计划同样允许挂任务：任务卡要能显示批准后的执行进度。
    let plan = crate::db::plans::plan(conn, plan_ref.as_str())?
        .ok_or_else(|| AppError::from(AgentErrorCode::ReferenceExpired))?;
    match plan.origin_kind.as_deref() {
        Some("gui") | None => {
            if plan.principal_id != caller.principal_id.as_str() {
                return Err(AppError::from(AgentErrorCode::Unauthorized));
            }
        }
        Some("pi") => {
            if plan.principal_id != crate::services::agent_runtime::PI_HOST_PRINCIPAL
                || plan.session_id.as_deref() != Some(conversation_ref.as_str())
            {
                return Err(AppError::from(AgentErrorCode::Unauthorized));
            }
        }
        Some(_) => return Err(AppError::from(AgentErrorCode::Unauthorized)),
    }
    Ok(())
}

/// 一个会话当前关联的工作卡片：计划与任务都由后端按可信上下文裁决后才返回。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConversationWorkView {
    pub plans: Vec<crate::contracts::agent::PlanPreview>,
    pub operations: Vec<crate::contracts::agent::OperationSnapshot>,
}

/// A3-04：按会话还原计划与任务卡片。
///
/// 归属有两条可信来源，二者取并集：
/// 1. `conversation_links` —— 用户/前端显式交接的记录（GUI 自己的计划只能靠它）。
/// 2. `plans.session_id == 会话引用` —— 内置助手在该会话里产生的计划（Pi 的 session
///    就是会话引用，写于 `plans::prepare_plan`，不是前端传参）。
///
/// 两条都只信任后端持久化的字段，前端无从指定「我要看哪个计划」以外的范围。
pub fn conversation_work(
    db: &Database,
    caller: &CallerContext,
    conversation_ref: &ResourceRef,
) -> AppResult<ConversationWorkView> {
    let conn = db.lock()?;
    validate_caller(&conn, caller)?;
    let record = load_owned(&conn, caller, conversation_ref)?;
    let conversation_id = record.conversation_id.as_str();

    let mut plan_records = conversations::links(&conn, conversation_id)?
        .into_iter()
        .filter(|link| link.link_kind == "plan")
        .map(|link| crate::db::plans::plan(&conn, link.link_ref.as_str()))
        .collect::<AppResult<Vec<_>>>()?
        .into_iter()
        .flatten()
        .collect::<Vec<_>>();
    let mut seen = plan_records
        .iter()
        .map(|record| record.plan_id.clone())
        .collect::<std::collections::HashSet<_>>();
    for session_plan in crate::db::plans::plans_for_session(
        &conn,
        caller.library.library_id.as_str(),
        caller.library.library_epoch.as_str(),
        conversation_id,
        WORK_CARD_LIMIT,
    )? {
        if seen.insert(session_plan.plan_id.clone()) {
            plan_records.push(session_plan);
        }
    }

    let mut plans = Vec::with_capacity(plan_records.len());
    let mut operations = Vec::new();
    let mut seen_operations = std::collections::HashSet::new();
    for plan_record in &plan_records {
        // 计划行必须仍属于当前库世代；换根后旧计划不呈现为可操作卡片。
        if plan_record.library_id != caller.library.library_id.as_str()
            || plan_record.library_epoch != caller.library.library_epoch.as_str()
        {
            continue;
        }
        plans.push(crate::services::plans::plan_preview(&conn, plan_record)?);
        for operation in crate::db::plans::operations_for_plan(&conn, plan_record.plan_id.as_str())?
        {
            if seen_operations.insert(operation.operation_id.clone()) {
                operations.push(crate::services::plans::operation_snapshot(
                    &conn, caller, &operation,
                )?);
            }
        }
    }
    // 显式挂过但计划已被清理的任务仍要能看见，否则用户会丢失任务卡。
    for link in conversations::links(&conn, conversation_id)? {
        if link.link_kind != "operation" {
            continue;
        }
        if !seen_operations.insert(link.link_ref.clone()) {
            continue;
        }
        if let Some(operation) = crate::db::plans::operation(&conn, link.link_ref.as_str())? {
            if operation.library_id == caller.library.library_id.as_str()
                && operation.library_epoch == caller.library.library_epoch.as_str()
            {
                operations.push(crate::services::plans::operation_snapshot(
                    &conn, caller, &operation,
                )?);
            }
        }
    }
    Ok(ConversationWorkView { plans, operations })
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

        // R24：附件按会话隔离 —— 两个会话各自登记不同素材。
        let first_attachment = attach_asset(
            &db,
            &caller,
            &first.conversation_ref,
            101,
            "第一会话素材.jpg",
        )
        .unwrap();
        let second_attachment = attach_asset(
            &db,
            &caller,
            &second.conversation_ref,
            202,
            "第二会话素材.jpg",
        )
        .unwrap();

        // R24：待执行清单按会话隔离 —— 计划与在途 operation 只挂到产生它们的那条会话。
        let plan_a = seed_pending_plan(&db, &caller, caller.principal_id.as_str(), Some("gui"));
        let operation_a = seed_running_operation(&db, &caller, &plan_a);
        link(&db, &caller, &first.conversation_ref, "plan", &plan_a).unwrap();
        link(
            &db,
            &caller,
            &first.conversation_ref,
            "operation",
            &operation_a,
        )
        .unwrap();
        let plan_b = seed_pending_plan(&db, &caller, caller.principal_id.as_str(), Some("gui"));
        link(&db, &caller, &second.conversation_ref, "plan", &plan_b).unwrap();

        // 会话 A 继续编辑草稿（revision 递增到 2），随后切到 B 再切回 A。
        save_draft(
            &db,
            &caller,
            &first.conversation_ref,
            "import",
            &json!({ "fileNames": ["a.jpg", "d.jpg"] }),
        )
        .unwrap();

        let second_view = get_conversation(&db, &caller, &second.conversation_ref).unwrap();
        let second_work = conversation_work(&db, &caller, &second.conversation_ref).unwrap();
        // 切回旧会话 A：重新读回，确认旧会话状态没被 B 顶掉。
        let first_view = get_conversation(&db, &caller, &first.conversation_ref).unwrap();
        let first_work = conversation_work(&db, &caller, &first.conversation_ref).unwrap();

        assert_eq!(
            first_view.drafts[0].payload["fileNames"],
            json!(["a.jpg", "d.jpg"])
        );
        assert_eq!(
            second_view.drafts[0].payload["fileNames"],
            json!(["b.jpg", "c.jpg"])
        );
        assert_eq!(second_view.drafts[0].revision, 1);
        // 切回 A 后草稿仍是 A 自己最新一版，revision 只随 A 自己的编辑递增。
        assert_eq!(first_view.drafts[0].revision, 2);

        // 附件：各自只看到自己登记的素材，引用、素材 id、名称都不串。
        assert_eq!(first_view.attachments.len(), 1);
        assert_eq!(
            first_view.attachments[0].attachment_ref.as_str(),
            first_attachment.attachment_ref.as_str()
        );
        assert_eq!(first_view.attachments[0].asset_id, 101);
        assert_eq!(first_view.attachments[0].display_name, "第一会话素材.jpg");
        // 素材未落库时如实标记不可用，不伪造「可用」。
        assert!(!first_view.attachments[0].available);
        assert_eq!(second_view.attachments.len(), 1);
        assert_eq!(
            second_view.attachments[0].attachment_ref.as_str(),
            second_attachment.attachment_ref.as_str()
        );
        assert_eq!(second_view.attachments[0].asset_id, 202);
        assert_eq!(second_view.attachments[0].display_name, "第二会话素材.jpg");
        assert!(!second_view.attachments[0].available);

        // 链接：A 只有自己的计划 + 在途任务，B 只有自己的计划。
        assert_eq!(
            first_view
                .links
                .iter()
                .map(|link| (link.kind.as_str(), link.reference.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("operation", operation_a.as_str()),
                ("plan", plan_a.as_str()),
            ]
        );
        assert_eq!(
            second_view
                .links
                .iter()
                .map(|link| (link.kind.as_str(), link.reference.as_str()))
                .collect::<Vec<_>>(),
            vec![("plan", plan_b.as_str())]
        );

        // 工作卡片：A 有计划 + 在途任务；B 不继承 A 的待执行清单。
        assert_eq!(first_work.plans.len(), 1);
        assert_eq!(first_work.plans[0].plan_id.as_str(), plan_a.as_str());
        assert_eq!(first_work.operations.len(), 1);
        assert_eq!(
            first_work.operations[0].operation_id.as_str(),
            operation_a.as_str()
        );
        assert_eq!(
            first_work.operations[0].state,
            crate::contracts::agent::OperationState::Running
        );
        assert_eq!(second_work.plans.len(), 1);
        assert_eq!(second_work.plans[0].plan_id.as_str(), plan_b.as_str());
        assert!(second_work.operations.is_empty());

        // R24：工作台状态各自保留 —— 只给 B 存一份工作台草稿，A 不应看到，B 自己要看得到。
        save_draft(
            &db,
            &caller,
            &second.conversation_ref,
            "workbench",
            &json!({ "activeTab": "import", "selectedAssetIds": [202], "step": 2 }),
        )
        .unwrap();
        let first_after_workbench =
            get_conversation(&db, &caller, &first.conversation_ref).unwrap();
        let second_after_workbench =
            get_conversation(&db, &caller, &second.conversation_ref).unwrap();
        assert_eq!(
            first_after_workbench
                .drafts
                .iter()
                .filter(|draft| draft.kind == "workbench")
                .count(),
            0,
            "会话 A 不应看到会话 B 的工作台状态"
        );
        let workbench_b = second_after_workbench
            .drafts
            .iter()
            .find(|draft| draft.kind == "workbench")
            .unwrap_or_else(|| panic!("会话 B 应读回自己的工作台草稿"));
        assert_eq!(workbench_b.payload["activeTab"], json!("import"));
        assert_eq!(workbench_b.payload["selectedAssetIds"], json!([202]));
        assert_eq!(workbench_b.revision, 1);
        // A 既有草稿没被 B 的工作台写入影响：还是 A 自己的那一版，revision 仍为 2。
        assert_eq!(
            first_after_workbench.drafts[0].payload["fileNames"],
            json!(["a.jpg", "d.jpg"])
        );
        assert_eq!(first_after_workbench.drafts[0].revision, 2);
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
        // 任务归属只能经它所属的计划还原，所以这里必须先有一个真实的计划行：
        // 孤儿 operation（plan_id 指向不存在的计划）按设计不可挂到会话。
        let plan_ref = seed_pending_plan(&db, &caller, caller.principal_id.as_str(), Some("gui"));
        let operation_ref = ResourceRef::generated("operation");
        {
            let conn = db.lock().unwrap();
            plans_db::insert_operation(
                &conn,
                &plans_db::NewOperation {
                    operation_id: operation_ref.as_str().to_string(),
                    step_id: ResourceRef::generated("step").as_str().to_string(),
                    plan_id: plan_ref.as_str().to_string(),
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

    /// Build one pending plan belonging to `principal_id` / the caller's library.
    fn seed_pending_plan(
        db: &Database,
        caller: &CallerContext,
        principal_id: &str,
        origin_kind: Option<&str>,
    ) -> ResourceRef {
        let conn = db.lock().unwrap();
        let plan_id = ResourceRef::generated("plan").as_str().to_string();
        let review_ref = ResourceRef::generated("review").as_str().to_string();
        let now = now_unix_seconds().unwrap();
        plans_db::insert_plan(
            &conn,
            &plans_db::NewPlan {
                plan_id: plan_id.clone(),
                principal_id: principal_id.to_string(),
                session_id: None,
                instance_id: "instance_conv_fixture".to_string(),
                capability: "imports.submit".to_string(),
                review_kind: "ordinary".to_string(),
                review_ref,
                plan_hash: "hash".to_string(),
                scope_ref: ResourceRef::generated("scope").as_str().to_string(),
                source_ref: None,
                selection_ref: None,
                target_ref: None,
                service_ref: None,
                service_revision: None,
                model: None,
                item_count: 1,
                summary: "计划".to_string(),
                details_json: None,
                max_requests: 0,
                max_tokens: 0,
                max_seconds: 0,
                max_items: 0,
                currency: None,
                amount_microunits: None,
                library_id: caller.library.library_id.as_str().to_string(),
                library_epoch: caller.library.library_epoch.as_str().to_string(),
                grant_epoch: String::new(),
                created_at: now,
                expires_at: now + 600,
                origin_kind: origin_kind.map(|value| value.to_string()),
            },
        )
        .unwrap();
        ResourceRef::from_trusted(plan_id).unwrap()
    }

    /// Build one in-flight (`running`) operation row under `plan_ref`, owned by the
    /// caller's principal and the caller's current library generation.
    fn seed_running_operation(
        db: &Database,
        caller: &CallerContext,
        plan_ref: &ResourceRef,
    ) -> ResourceRef {
        let conn = db.lock().unwrap();
        let operation_id = ResourceRef::generated("operation").as_str().to_string();
        plans_db::insert_operation(
            &conn,
            &plans_db::NewOperation {
                operation_id: operation_id.clone(),
                step_id: ResourceRef::generated("step").as_str().to_string(),
                plan_id: plan_ref.as_str().to_string(),
                principal_id: caller.principal_id.as_str().to_string(),
                capability: "imports.submit".to_string(),
                idempotency_key: ResourceRef::generated("idempotency").as_str().to_string(),
                library_id: caller.library.library_id.as_str().to_string(),
                library_epoch: caller.library.library_epoch.as_str().to_string(),
                state: "running".to_string(),
                phase: "executing".to_string(),
                revision: 1,
                total: Some(1),
                completed: 0,
                failed: 0,
                created_at: now_unix_seconds().unwrap(),
            },
        )
        .unwrap();
        ResourceRef::from_trusted(operation_id).unwrap()
    }

    /// P0-4 回归：只有本来源（gui）产生的计划可以被会话认领；
    /// 其它来源（如 mcp 客户端）准备的计划即便名为 pending，也不得挂到本会话上。
    #[test]
    fn plan_link_rejects_plans_from_other_origin() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "归属").unwrap();

        // 本来源（gui）计划：允许认领并落库。
        let own_plan = seed_pending_plan(&db, &caller, caller.principal_id.as_str(), Some("gui"));
        link(
            &db,
            &caller,
            &conversation.conversation_ref,
            "plan",
            &own_plan,
        )
        .unwrap();
        let view = get_conversation(&db, &caller, &conversation.conversation_ref).unwrap();
        assert!(view
            .links
            .iter()
            .any(|item| item.kind == "plan" && item.reference == own_plan));

        // 外部（mcp）来源计划：必须被拒绝，且不落库。
        let foreign_plan =
            seed_pending_plan(&db, &caller, caller.principal_id.as_str(), Some("mcp"));
        assert_eq!(
            error_code(
                link(
                    &db,
                    &caller,
                    &conversation.conversation_ref,
                    "plan",
                    &foreign_plan,
                )
                .unwrap_err()
            ),
            "UNAUTHORIZED"
        );
        let view = get_conversation(&db, &caller, &conversation.conversation_ref).unwrap();
        assert!(
            !view.links.iter().any(|item| item.reference == foreign_plan),
            "外部计划不得被挂到本会话"
        );
    }

    /// P0-4 回归：别的 principal 的计划不得被当前会话认领。
    #[test]
    fn plan_link_rejects_another_principals_plan() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "归属2").unwrap();
        // 真实登记第二个 principal（外键要求），再为它放一张 pending 计划。
        {
            let conn = db.lock().unwrap();
            crate::db::authorization::ensure_principal(
                &conn,
                "principal_pi_other",
                now_unix_seconds().unwrap(),
            )
            .unwrap();
        }
        let foreign = seed_pending_plan(&db, &caller, "principal_pi_other", Some("gui"));
        assert_eq!(
            error_code(
                link(
                    &db,
                    &caller,
                    &conversation.conversation_ref,
                    "plan",
                    &foreign,
                )
                .unwrap_err()
            ),
            "UNAUTHORIZED"
        );
    }

    /// P0-4 回归：非 pending 的计划（已审批/过期/消费）不得被认领。
    #[test]
    fn plan_link_rejects_non_pending_plan() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "归属3").unwrap();
        let plan = seed_pending_plan(&db, &caller, caller.principal_id.as_str(), Some("gui"));
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "UPDATE plans SET review_status='consumed' WHERE plan_id=?1",
                [plan.as_str()],
            )
            .unwrap();
        }
        assert_eq!(
            error_code(
                link(&db, &caller, &conversation.conversation_ref, "plan", &plan).unwrap_err()
            ),
            "PLAN_CHANGED"
        );
    }

    /// 放一张属于内置助手、且计划行上持久化了 `session_id` 的计划。
    ///
    /// 真实链路里 `session_id` 由 `plans::prepare_plan` 从 Pi 调用方上下文写入
    /// （Pi 的 session 就是会话引用），不是前端传参；这里直接落库来构造同一形态。
    fn seed_agent_plan(
        db: &Database,
        caller: &CallerContext,
        session_id: Option<&str>,
    ) -> ResourceRef {
        {
            let conn = db.lock().unwrap();
            crate::db::authorization::ensure_principal(
                &conn,
                crate::services::agent_runtime::PI_HOST_PRINCIPAL,
                now_unix_seconds().unwrap(),
            )
            .unwrap();
        }
        let conn = db.lock().unwrap();
        let plan_id = ResourceRef::generated("plan").as_str().to_string();
        let now = now_unix_seconds().unwrap();
        plans_db::insert_plan(
            &conn,
            &plans_db::NewPlan {
                plan_id: plan_id.clone(),
                principal_id: crate::services::agent_runtime::PI_HOST_PRINCIPAL.to_string(),
                session_id: session_id.map(|value| value.to_string()),
                instance_id: "instance_agent_fixture".to_string(),
                capability: "tagging.submit".to_string(),
                review_kind: "ordinary".to_string(),
                review_ref: ResourceRef::generated("review").as_str().to_string(),
                plan_hash: "hash".to_string(),
                scope_ref: ResourceRef::generated("scope").as_str().to_string(),
                source_ref: None,
                selection_ref: None,
                target_ref: None,
                service_ref: None,
                service_revision: None,
                model: None,
                item_count: 2,
                summary: "助手准备的打标计划".to_string(),
                details_json: None,
                max_requests: 0,
                max_tokens: 0,
                max_seconds: 0,
                max_items: 2,
                currency: None,
                amount_microunits: None,
                library_id: caller.library.library_id.as_str().to_string(),
                library_epoch: caller.library.library_epoch.as_str().to_string(),
                grant_epoch: String::new(),
                created_at: now,
                expires_at: now + 600,
                origin_kind: Some("pi".to_string()),
            },
        )
        .unwrap();
        ResourceRef::from_trusted(plan_id).unwrap()
    }

    /// A3-04 核心回归：助手在本会话里产生的计划，不靠任何前端链接也要能被还原出来。
    ///
    /// 这正是审计里「全库待审计划差集被误当会话归属」修复后的正面用例：归属完全由
    /// 后端持久化的 `plans.session_id` 裁决，前端无权指定。
    #[test]
    fn agent_plans_are_attributed_by_persisted_session_not_by_frontend() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "助手归属").unwrap();
        let conversation_ref = conversation.conversation_ref.as_str();

        let mine = seed_agent_plan(&db, &caller, Some(conversation_ref));
        // 同一个助手主体，但属于另一个会话：不得出现在本会话里。
        let other = seed_agent_plan(&db, &caller, Some("conversation_cccccccccccccccc"));
        // 没有会话的计划（比如模型在没有会话的场景下准备的）也不得凭空归到本会话。
        let orphan = seed_agent_plan(&db, &caller, None);

        let work = conversation_work(&db, &caller, &conversation.conversation_ref).unwrap();
        let ids = work
            .plans
            .iter()
            .map(|plan| plan.plan_id.as_str().to_string())
            .collect::<Vec<_>>();
        assert!(
            ids.contains(&mine.as_str().to_string()),
            "本会话的助手计划必须可见"
        );
        assert!(
            !ids.contains(&other.as_str().to_string()),
            "别的会话的助手计划不得被本会话认领"
        );
        assert!(
            !ids.contains(&orphan.as_str().to_string()),
            "没有会话归属的计划不得被凭空认领"
        );
        assert_eq!(ids.len(), 1);
    }

    /// A3-04：助手计划同样不能靠前端 `link` 强行跨会话认领，只能由后端裁决。
    #[test]
    fn agent_plan_cannot_be_linked_into_a_foreign_conversation() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "别人的会话").unwrap();
        let foreign = seed_agent_plan(&db, &caller, Some("conversation_dddddddddddddddd"));
        assert_eq!(
            error_code(
                link(
                    &db,
                    &caller,
                    &conversation.conversation_ref,
                    "plan",
                    &foreign,
                )
                .unwrap_err()
            ),
            "UNAUTHORIZED"
        );

        // 属于本会话的助手计划可以显式认领（幂等，不改变归属）。
        let mine = seed_agent_plan(&db, &caller, Some(conversation.conversation_ref.as_str()));
        link(&db, &caller, &conversation.conversation_ref, "plan", &mine).unwrap();
    }

    /// A3-04：任务卡经计划归属还原；孤儿任务（计划不存在的操作）不得被挂到会话上。
    #[test]
    fn operations_are_reachable_only_through_their_plan() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "任务归属").unwrap();
        let plan = seed_agent_plan(&db, &caller, Some(conversation.conversation_ref.as_str()));
        let operation_ref = ResourceRef::generated("operation");
        {
            let conn = db.lock().unwrap();
            plans_db::insert_operation(
                &conn,
                &plans_db::NewOperation {
                    operation_id: operation_ref.as_str().to_string(),
                    step_id: ResourceRef::generated("step").as_str().to_string(),
                    plan_id: plan.as_str().to_string(),
                    principal_id: crate::services::agent_runtime::PI_HOST_PRINCIPAL.to_string(),
                    capability: "tagging.submit".to_string(),
                    idempotency_key: ResourceRef::generated("idempotency").as_str().to_string(),
                    library_id: caller.library.library_id.as_str().to_string(),
                    library_epoch: caller.library.library_epoch.as_str().to_string(),
                    state: "queued".to_string(),
                    phase: "ready".to_string(),
                    revision: 1,
                    total: Some(2),
                    completed: 0,
                    failed: 0,
                    created_at: now_unix_seconds().unwrap(),
                },
            )
            .unwrap();
        }
        let work = conversation_work(&db, &caller, &conversation.conversation_ref).unwrap();
        assert_eq!(work.operations.len(), 1);
        assert_eq!(
            work.operations[0].operation_id.as_str(),
            operation_ref.as_str()
        );
    }

    /// 在 `import_plans` 里种一条未提交的准备计划。
    fn seed_import_plan(
        db: &Database,
        principal_id: &str,
        library_id: &str,
        library_epoch: &str,
        expires_at: i64,
    ) -> ResourceRef {
        let plan_id = ResourceRef::generated("importplan").as_str().to_string();
        let conn = db.lock().unwrap();
        let now = now_unix_seconds().unwrap();
        conn.execute(
            "INSERT INTO import_plans(plan_id,principal_id,source_ref,target_collection,
                rename_pattern,library_id,library_epoch,item_count,ready_count,
                limited_count,unsupported_count,total_size,created_at,expires_at)
             VALUES(?1,?2,NULL,NULL,NULL,?3,?4,2,2,0,0,2048,?5,?6)",
            rusqlite::params![
                plan_id,
                principal_id,
                library_id,
                library_epoch,
                now,
                expires_at
            ],
        )
        .unwrap();
        ResourceRef::from_trusted(plan_id).unwrap()
    }

    /// M-08 真机走查暴露：独立入库工作台的「交给助手（待确认）」交的是**未提交**的
    /// `importplan_*`，过去一律被 `db::plans::plan()` 判成 REFERENCE_EXPIRED。
    #[test]
    fn unsubmitted_import_plan_can_be_handed_to_a_conversation() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "手动入库工作台").unwrap();
        let now = now_unix_seconds().unwrap();
        let plan_ref = seed_import_plan(
            &db,
            caller.principal_id.as_str(),
            caller.library.library_id.as_str(),
            caller.library.library_epoch.as_str(),
            now + 1_800,
        );

        link(
            &db,
            &caller,
            &conversation.conversation_ref,
            "plan",
            &plan_ref,
        )
        .unwrap();

        let view = get_conversation(&db, &caller, &conversation.conversation_ref).unwrap();
        assert!(view
            .links
            .iter()
            .any(|item| item.kind == "plan" && item.reference.as_str() == plan_ref.as_str()));
    }

    /// 归属与过期照旧硬校验：别人的计划不能挂过来，过期计划必须明确失败。
    #[test]
    fn import_plan_handoff_rejects_foreign_principal_and_expiry() {
        let managed = tempfile::tempdir().unwrap();
        let db = ready_db(managed.path());
        let caller = gui_caller(&db);
        let conversation = create_conversation(&db, &caller, "手动入库工作台").unwrap();
        let now = now_unix_seconds().unwrap();

        // 外键要求主体必须先登记，模拟「别人准备好的入库计划」。
        {
            let conn = db.lock().unwrap();
            conn.execute(
                "INSERT INTO authorization_principals(principal_id,grant_epoch,created_at,updated_at)
                 VALUES('principal_someone_else','epoch_foreign',?1,?1)",
                rusqlite::params![now],
            )
            .unwrap();
        }
        let foreign = seed_import_plan(
            &db,
            "principal_someone_else",
            caller.library.library_id.as_str(),
            caller.library.library_epoch.as_str(),
            now + 1_800,
        );
        let error = link(
            &db,
            &caller,
            &conversation.conversation_ref,
            "plan",
            &foreign,
        )
        .unwrap_err();
        assert_eq!(error_code(error), "UNAUTHORIZED");

        let expired = seed_import_plan(
            &db,
            caller.principal_id.as_str(),
            caller.library.library_id.as_str(),
            caller.library.library_epoch.as_str(),
            now - 1,
        );
        let error = link(
            &db,
            &caller,
            &conversation.conversation_ref,
            "plan",
            &expired,
        )
        .unwrap_err();
        assert_eq!(error_code(error), "PLAN_EXPIRED");
    }
}
