//! GUI entry points for the host conversation index and persistent drafts.
//!
//! These commands only project host metadata; the Pi transcript stays the
//! single source of conversational context.

use serde_json::Value;
use tauri::State;

use crate::contracts::agent::ResourceRef;
use crate::contracts::caller::CallerContext;
use crate::error::AppResult;
use crate::services::authorization::{self, TrustedAdapterIdentity};
use crate::services::conversations::{
    self, AttachmentView, ConversationSummary, ConversationView, ConversationWorkView, DraftView,
};
use crate::state::AppState;

fn gui_caller(state: &AppState) -> AppResult<CallerContext> {
    let conn = state.db.lock()?;
    authorization::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Gui {
            session_id: state.authorization_session_id.clone(),
            instance_id: state.authorization_instance_id.clone(),
        },
        ResourceRef::generated("request"),
        Vec::new(),
    )
}

#[tauri::command]
pub fn create_conversation(
    state: State<AppState>,
    title: String,
) -> AppResult<ConversationSummary> {
    let caller = gui_caller(&state)?;
    conversations::create_conversation(&state.db, &caller, &title)
}

#[tauri::command]
pub fn list_conversations(
    state: State<AppState>,
    include_archived: Option<bool>,
    query: Option<String>,
) -> AppResult<Vec<ConversationSummary>> {
    let caller = gui_caller(&state)?;
    conversations::list_conversations(
        &state.db,
        &caller,
        include_archived.unwrap_or(false),
        query.as_deref(),
    )
}

#[tauri::command]
pub fn get_conversation(
    state: State<AppState>,
    conversation_ref: ResourceRef,
) -> AppResult<ConversationView> {
    let caller = gui_caller(&state)?;
    conversations::get_conversation(&state.db, &caller, &conversation_ref)
}

#[tauri::command]
pub fn rename_conversation(
    state: State<AppState>,
    conversation_ref: ResourceRef,
    title: String,
) -> AppResult<ConversationSummary> {
    let caller = gui_caller(&state)?;
    conversations::rename_conversation(&state.db, &caller, &conversation_ref, &title)
}

#[tauri::command]
pub fn set_conversation_archived(
    state: State<AppState>,
    conversation_ref: ResourceRef,
    archived: bool,
) -> AppResult<ConversationSummary> {
    let caller = gui_caller(&state)?;
    conversations::set_archived(&state.db, &caller, &conversation_ref, archived)
}

#[tauri::command]
pub fn delete_conversation(state: State<AppState>, conversation_ref: ResourceRef) -> AppResult<()> {
    let caller = gui_caller(&state)?;
    conversations::delete_conversation(&state.db, &caller, &conversation_ref)
}

#[tauri::command]
pub fn save_conversation_draft(
    state: State<AppState>,
    conversation_ref: ResourceRef,
    kind: String,
    payload: Value,
) -> AppResult<DraftView> {
    let caller = gui_caller(&state)?;
    conversations::save_draft(&state.db, &caller, &conversation_ref, &kind, &payload)
}

#[tauri::command]
pub fn attach_conversation_asset(
    state: State<AppState>,
    conversation_ref: ResourceRef,
    asset_id: i64,
    display_name: String,
) -> AppResult<AttachmentView> {
    let caller = gui_caller(&state)?;
    conversations::attach_asset(
        &state.db,
        &caller,
        &conversation_ref,
        asset_id,
        &display_name,
    )
}

#[tauri::command]
pub fn link_conversation(
    state: State<AppState>,
    conversation_ref: ResourceRef,
    kind: String,
    reference: ResourceRef,
) -> AppResult<()> {
    let caller = gui_caller(&state)?;
    conversations::link(&state.db, &caller, &conversation_ref, &kind, &reference)
}

/// A3-04：按会话还原计划与任务卡片。归属由后端从 `conversation_links` 与
/// `plans.session_id` 裁决，前端只提供会话引用，无法指定要认领哪些计划。
#[tauri::command]
pub fn get_conversation_work(
    state: State<AppState>,
    conversation_ref: ResourceRef,
) -> AppResult<ConversationWorkView> {
    let caller = gui_caller(&state)?;
    conversations::conversation_work(&state.db, &caller, &conversation_ref)
}
