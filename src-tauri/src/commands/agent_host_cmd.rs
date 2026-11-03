//! GUI-only ingress for the first Pi Host lifecycle slice.
//! Conversation ownership is resolved through the existing host conversation index;
//! callers never provide filesystem paths or provider credentials.

use tauri::State;

use crate::contracts::agent::ResourceRef;
use crate::contracts::caller::CallerContext;
use crate::error::AppResult;
use crate::services::agent_runtime::permission::AgentPermission;
use crate::services::agent_runtime::{AgentConversationStatus, AgentHostStatus, AgentRunDispatch};
use crate::services::authorization::{self, TrustedAdapterIdentity};
use crate::services::conversations;
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

fn require_owned_conversation(state: &AppState, conversation_ref: &ResourceRef) -> AppResult<()> {
    let caller = gui_caller(state)?;
    conversations::get_conversation(&state.db, &caller, conversation_ref).map(|_| ())
}

/// Start the bundled host. `recover_stranded_host` is only set after the GUI
/// explicitly presents a recovery choice; it never replays a prompt.
#[tauri::command]
pub fn agent_host_start(
    state: State<AppState>,
    recover_stranded_host: Option<bool>,
) -> AppResult<AgentHostStatus> {
    state
        .agent_runtime
        .start(recover_stranded_host.unwrap_or(false))
}

#[tauri::command]
pub fn agent_host_open_conversation(
    state: State<AppState>,
    conversation_ref: ResourceRef,
    mode: String,
    permission: String,
) -> AppResult<AgentConversationStatus> {
    require_owned_conversation(&state, &conversation_ref)?;
    // A3-10：档位由用户在界面上显式选择。未知取值一律拒绝，不回落默认档——
    // 回落会把「前端写错」静默变成「用了一个用户没选的档」。
    let permission = AgentPermission::parse(&permission)
        .ok_or_else(|| crate::error::AppError::invalid_arg("会话权限档位无效"))?;
    state
        .agent_runtime
        .open_conversation(conversation_ref.as_str(), &mode, permission)
}

#[tauri::command]
pub fn agent_host_send(
    state: State<AppState>,
    conversation_ref: ResourceRef,
    text: String,
    thinking_level: Option<String>,
) -> AppResult<AgentRunDispatch> {
    require_owned_conversation(&state, &conversation_ref)?;
    state
        .agent_runtime
        .send(conversation_ref.as_str(), &text, thinking_level.as_deref())
}

#[tauri::command]
pub fn agent_host_cancel_run(
    state: State<AppState>,
    conversation_ref: ResourceRef,
    run_id: ResourceRef,
) -> AppResult<()> {
    require_owned_conversation(&state, &conversation_ref)?;
    state
        .agent_runtime
        .cancel_run(conversation_ref.as_str(), run_id.as_str())
}

#[tauri::command]
pub fn agent_host_close_conversation(
    state: State<AppState>,
    conversation_ref: ResourceRef,
) -> AppResult<()> {
    require_owned_conversation(&state, &conversation_ref)?;
    state
        .agent_runtime
        .close_conversation(conversation_ref.as_str())
}

/// 只读回读一段会话的历史投影（来自 Pi JSONL，唯一事实源）。
/// 用于应用重启后恢复消息列表；不写库、不重放、不产生业务副作用。
#[tauri::command]
pub fn agent_host_conversation_history(
    state: State<AppState>,
    conversation_ref: ResourceRef,
) -> AppResult<serde_json::Value> {
    require_owned_conversation(&state, &conversation_ref)?;
    state
        .agent_runtime
        .conversation_history(conversation_ref.as_str())
}
