//! 「设置 → AI 服务 → Agent 代理」的读写入口。
//!
//! Agent 外发没有独立的「允许联网」开关：绑定服务、服务启用且凭据可用即可直接
//! 对话（判定见 `GatewayBroker::preflight`）。这里只负责唯一的网络配置项——
//! 显式 http(s) 代理。命令只做参数校验与编排：URL 规则在
//! `services::agent_runtime::gateway_http`，持久化在 `db::settings`。

use tauri::State;

use crate::db::settings;
use crate::error::{AppError, AppResult};
use crate::services::agent_runtime::gateway_http::validate_agent_proxy_url;
use crate::services::agent_runtime::AgentModelProxyView;
use crate::state::AppState;

fn stored_proxy(state: &AppState) -> AppResult<String> {
    let conn = state.db.lock()?;
    Ok(settings::get_settings(&conn)?.agent_model_proxy)
}

#[tauri::command]
pub fn get_agent_model_proxy(state: State<AppState>) -> AppResult<AgentModelProxyView> {
    Ok(AgentModelProxyView {
        proxy: stored_proxy(&state)?,
    })
}

/// 设置 Agent 外发使用的显式代理。空字符串表示直连。
///
/// 先应用再落库：运行时拒绝（client 构建失败）时不写库，界面保持旧值。
#[tauri::command]
pub fn set_agent_model_proxy(
    state: State<AppState>,
    proxy: Option<String>,
) -> AppResult<AgentModelProxyView> {
    let raw = proxy.unwrap_or_default();
    let trimmed = raw.trim();
    let proxy = if trimmed.is_empty() {
        String::new()
    } else {
        validate_agent_proxy_url(trimmed).map_err(|_| {
            AppError::invalid_arg(
                "代理地址无效：只支持 http:// 或 https://主机:端口，且不能包含账号密码、路径或查询参数。",
            )
        })?
    };

    // 先记下旧值：落库失败时要把运行时退回去，否则「界面显示新代理、重启后却变旧值」
    // 这种静默反转比直接报错更难排查。
    let previous = stored_proxy(&state)?;

    state.agent_runtime.apply_model_proxy(&proxy)?;

    let persisted = {
        let conn = state.db.lock()?;
        let mut next = settings::get_settings(&conn)?;
        next.agent_model_proxy = proxy.clone();
        settings::save_settings(&conn, &next)
    };
    if let Err(error) = persisted {
        // 回滚尽力而为：即使回滚也失败，也要把原始错误交给用户，不要用回滚错误覆盖它。
        let _ = state.agent_runtime.apply_model_proxy(&previous);
        return Err(error);
    }

    get_agent_model_proxy(state)
}
