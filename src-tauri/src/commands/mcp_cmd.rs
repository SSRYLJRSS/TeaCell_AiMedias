//! A5-01：本机外部接入（MCP）的配对与开关命令。
//!
//! 全部命令只由可信设置界面调用。外部客户端自己不能创建、改名、扩权或撤销
//! 连接——它的身份来自这里的配对记录，`clientInfo` 只是标签。
//!
//! 行为事实：[`docs/contracts/mcp-bridge-v1.md`]。

use serde_json::Value;
use tauri::State;

use crate::db::mcp_pairings;
use crate::db::settings;
use crate::error::{AppError, AppErrorCode, AppResult};
use crate::services::mcp_bridge::{self, BridgeRuntimeStatus, MAX_SESSIONS};
use crate::state::AppState;

fn now_unix_seconds() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 状态投影。`running` 与 `enabled` 分开汇报，界面才能如实区分
/// 「已启用但没在监听」——用猜测的文案会让用户以为已经能用。
fn build_status(state: &AppState, conn: &rusqlite::Connection) -> AppResult<Value> {
    let settings = settings::get_settings(conn)?;
    let runtime: BridgeRuntimeStatus = state.mcp_bridge.status();
    let connections: Vec<Value> = mcp_pairings::list(conn)?
        .iter()
        .map(mcp_bridge::pairing_view)
        .collect();
    Ok(serde_json::json!({
        "enabled": settings.mcp_bridge_enabled,
        "running": runtime.running,
        "platformSupported": mcp_bridge::platform_supported(),
        "instance": runtime.instance,
        "pipeName": runtime.pipe_name,
        "endpointPath": runtime.endpoint_path,
        "sessionCount": runtime.session_count,
        "maxSessions": MAX_SESSIONS,
        "connections": connections,
        "publishable": mcp_bridge::capability_views(),
        "lastError": runtime.last_error,
    }))
}

fn normalize_label(label: &str) -> AppResult<String> {
    let trimmed = label.trim();
    if trimmed.is_empty() {
        return Err(AppError::invalid_arg("连接名称不能为空"));
    }
    if trimmed.chars().count() > 64 {
        return Err(AppError::invalid_arg("连接名称最多 64 个字符"));
    }
    Ok(trimmed.to_string())
}

#[tauri::command]
pub fn mcp_bridge_status(state: State<AppState>) -> AppResult<Value> {
    let conn = state.db.lock()?;
    build_status(&state, &conn)
}

/// 开关本机接入。开启时立刻建端点文件并监听；关闭时删端点文件并断开所有会话。
#[tauri::command]
pub fn set_mcp_bridge_enabled(state: State<AppState>, enabled: bool) -> AppResult<Value> {
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "切换本机接入")?;
    if enabled && !mcp_bridge::platform_supported() {
        return Err(AppError::coded(
            AppErrorCode::Unsupported,
            "本机接入目前只在 Windows x64 提供",
        ));
    }

    // 先写设置再起监听：反过来的话监听起来了但设置是关的，重启后就没了。
    {
        let conn = state.db.lock()?;
        let mut settings = settings::get_settings(&conn)?;
        if settings.mcp_bridge_enabled != enabled {
            settings.mcp_bridge_enabled = enabled;
            settings::save_settings(&conn, &settings)?;
        }
    }

    if enabled {
        // 起不来就把设置回滚成关闭并如实报错，不能留下「显示已启用但其实没监听」。
        if let Err(error) = state.mcp_bridge.start(
            std::sync::Arc::clone(&state.db),
            &state.data_dir,
            std::sync::Arc::clone(&state.library_maintenance),
        ) {
            let conn = state.db.lock()?;
            let mut settings = settings::get_settings(&conn)?;
            settings.mcp_bridge_enabled = false;
            settings::save_settings(&conn, &settings)?;
            return Err(error);
        }
    } else {
        state.mcp_bridge.stop();
    }

    let conn = state.db.lock()?;
    build_status(&state, &conn)
}

/// 新建连接。默认只勾查询类是界面的事；后端只校验「不得超出可发布集合」。
#[tauri::command]
pub fn create_mcp_pairing(
    state: State<AppState>,
    label: String,
    capabilities: Vec<String>,
) -> AppResult<Value> {
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "创建本机接入连接")?;
    let label = normalize_label(&label)?;
    let capabilities = mcp_bridge::normalize_capability_request(&capabilities)?;
    let conn = state.db.lock()?;
    let record = mcp_bridge::create_pairing(&conn, &label, &capabilities)?;
    Ok(mcp_bridge::pairing_view(&record))
}

/// 整份替换名称与能力范围（不是增量补丁）。
#[tauri::command]
pub fn update_mcp_pairing(
    state: State<AppState>,
    connection_id: String,
    label: String,
    capabilities: Vec<String>,
) -> AppResult<Value> {
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "修改本机接入连接")?;
    let label = normalize_label(&label)?;
    let capabilities = mcp_bridge::normalize_capability_request(&capabilities)?;
    let now = now_unix_seconds();
    let conn = state.db.lock()?;
    let record = mcp_pairings::get(&conn, &connection_id)?
        .ok_or_else(|| AppError::not_found("这个连接不存在"))?;
    if record.is_revoked() {
        // 撤销是终态：想继续用就新建一条，避免「撤销了又能改回来」。
        return Err(AppError::conflict("这个连接已被撤销，不能再修改"));
    }
    if record.label != label {
        mcp_pairings::rename(&conn, &connection_id, &label, now)?;
    }
    if record.capabilities != capabilities {
        mcp_pairings::set_capabilities(&conn, &connection_id, &capabilities, now)?;
    }
    let updated = mcp_pairings::get(&conn, &connection_id)?
        .ok_or_else(|| AppError::not_found("这个连接不存在"))?;
    Ok(mcp_bridge::pairing_view(&updated))
}

/// 撤销连接。幂等：对已撤销的连接再调一次返回同一条记录，不报错。
///
/// 撤销**立即生效**：已建立的会话在下一帧 `call` 就会被拒（契约 §7），
/// 已入队的写任务不因此回滚或重放。
#[tauri::command]
pub fn revoke_mcp_pairing(state: State<AppState>, connection_id: String) -> AppResult<Value> {
    crate::commands::library_maintenance_cmd::ensure_writable(&state, "撤销本机接入连接")?;
    let conn = state.db.lock()?;
    mcp_pairings::get(&conn, &connection_id)?
        .ok_or_else(|| AppError::not_found("这个连接不存在"))?;
    mcp_pairings::revoke(&conn, &connection_id, now_unix_seconds())?;
    let record = mcp_pairings::get(&conn, &connection_id)?
        .ok_or_else(|| AppError::not_found("这个连接不存在"))?;
    Ok(mcp_bridge::pairing_view(&record))
}

/// 应用启动时按设置自动起监听。设置关着就什么都不做（契约 §8 默认关闭）。
///
/// 监听本身不依赖总仓健康：握手第 ⑥ 步才检查总仓，所以总仓还没就绪时也应该
/// 先把管道起起来，否则用户只能看到「已启用但没有在监听」这一态而没有出路。
pub fn start_from_settings(state: &AppState) {
    let enabled = match state.db.lock() {
        Ok(conn) => settings::get_settings(&conn)
            .map(|settings| settings.mcp_bridge_enabled)
            .unwrap_or(false),
        Err(error) => {
            tracing::warn!("启动时读取本机接入设置失败: {error}");
            return;
        }
    };
    if !enabled {
        return;
    }
    if !mcp_bridge::platform_supported() {
        tracing::warn!("本机接入已在设置中启用，但当前平台不支持命名管道");
        return;
    }
    if let Err(error) = state.mcp_bridge.start(
        std::sync::Arc::clone(&state.db),
        &state.data_dir,
        std::sync::Arc::clone(&state.library_maintenance),
    ) {
        tracing::warn!("启动本机接入监听失败: {error}");
    }
}
