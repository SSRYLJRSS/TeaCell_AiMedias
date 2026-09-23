//! AI 连接档案命令（指导书 §6.3/§4.4）：连接档案 CRUD（API Key 走系统凭据）+ 用途绑定。
//!  - API Key 不经过普通 settings JSON：保存时写 keyring，成功后才置 api_key_ref；
//!  - 用途绑定（super_search/tagging）独立可变：修改一个不影响另一个。

use tauri::State;

use crate::db::ai_connections;
use crate::db::settings;
use crate::error::{AppError, AppResult};
use crate::services::credentials;
use crate::services::ollama_runtime;
use crate::state::AppState;

#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AiConnectionView {
    pub id: String,
    pub name: String,
    pub deployment: String,
    pub protocol: String,
    pub base_url: String,
    pub model: String,
    pub max_concurrency: i64,
    pub requests_per_minute: i64,
    pub requests_per_hour: i64,
    /// 是否已配置 API 密钥（不回显 plaintext；只暴露已配置/未配置）
    pub has_key: bool,
    pub enabled: bool,
}

fn lock_db(state: &AppState) -> AppResult<std::sync::MutexGuard<'_, rusqlite::Connection>> {
    state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))
}

/// 列出全部连接档案（含 key 配置状态）。
#[tauri::command]
pub fn list_ai_connections(state: State<AppState>) -> AppResult<Vec<AiConnectionView>> {
    let conn = lock_db(&state)?;
    let rows = ai_connections::list(&conn)?;
    let mut out = Vec::new();
    for c in rows {
        let has_key = match &c.api_key_ref {
            Some(id) => credentials::get_api_key(id)?
                .map(|k| !k.is_empty())
                .unwrap_or(false),
            None => false,
        };
        out.push(AiConnectionView {
            id: c.id,
            name: c.name,
            deployment: c.deployment,
            protocol: c.protocol,
            base_url: c.base_url,
            model: c.model,
            max_concurrency: c.max_concurrency,
            requests_per_minute: c.requests_per_minute,
            requests_per_hour: c.requests_per_hour,
            has_key,
            enabled: c.enabled,
        });
    }
    Ok(out)
}

/// 保存连接档案。api_key 为 Some(非空) 时写入系统凭据（keyring）并置 api_key_ref；
/// 为空/None 时保留原有密钥配置（不覆盖）。
// 参数即 IPC 契约（前端 invoke 按字段名传参），收进结构体会破坏前端调用，故平铺并豁免。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub fn save_ai_connection(
    state: State<AppState>,
    id: String,
    name: String,
    deployment: String,
    protocol: String,
    base_url: String,
    model: String,
    api_key: Option<String>,
    max_concurrency: Option<i64>,
    requests_per_minute: Option<i64>,
    requests_per_hour: Option<i64>,
) -> AppResult<AiConnectionView> {
    if id.trim().is_empty() || name.trim().is_empty() {
        return Err(AppError::msg("连接名称与内部标识不能为空"));
    }
    if base_url.trim().is_empty() {
        return Err(AppError::msg("服务地址不能为空"));
    }
    let conn = lock_db(&state)?;
    // 新 key 只在 keyring 写成功后才引用
    let mut api_key_ref: Option<String> = None;
    if let Some(k) = &api_key {
        if !k.trim().is_empty() {
            credentials::save_api_key(&id, k.trim())?;
            api_key_ref = Some(id.clone());
        }
    }
    let existing = ai_connections::get(&conn, id.trim())?;
    let limits = (
        max_concurrency
            .unwrap_or_else(|| existing.as_ref().map(|c| c.max_concurrency).unwrap_or(0)),
        requests_per_minute.unwrap_or_else(|| {
            existing
                .as_ref()
                .map(|c| c.requests_per_minute)
                .unwrap_or(0)
        }),
        requests_per_hour
            .unwrap_or_else(|| existing.as_ref().map(|c| c.requests_per_hour).unwrap_or(0)),
    );
    ai_connections::upsert_with_limits(
        &conn,
        id.trim(),
        name.trim(),
        &deployment,
        &protocol,
        base_url.trim(),
        model.trim(),
        api_key_ref.as_deref(),
        limits.0,
        limits.1,
        limits.2,
    )?;
    let c =
        ai_connections::get(&conn, id.trim())?.ok_or_else(|| AppError::msg("保存后读取失败"))?;
    let has_key = match &c.api_key_ref {
        Some(id) => credentials::get_api_key(id)?
            .map(|k| !k.is_empty())
            .unwrap_or(false),
        None => false,
    };
    Ok(AiConnectionView {
        id: c.id,
        name: c.name,
        deployment: c.deployment,
        protocol: c.protocol,
        base_url: c.base_url,
        model: c.model,
        max_concurrency: c.max_concurrency,
        requests_per_minute: c.requests_per_minute,
        requests_per_hour: c.requests_per_hour,
        has_key,
        enabled: c.enabled,
    })
}

/// 删除连接档案（同时删除用途绑定与系统凭据）。
#[tauri::command]
pub fn delete_ai_connection(state: State<AppState>, id: String) -> AppResult<()> {
    let conn = lock_db(&state)?;
    ai_connections::delete(&conn, &id)?;
    let _ = credentials::delete_api_key(&id);
    Ok(())
}

/// 绑定用途 → 连接（connection_id 为空 = 解绑，回退默认档案）。
#[tauri::command]
pub fn set_ai_usage_binding(
    state: State<AppState>,
    usage: String,
    connection_id: Option<String>,
) -> AppResult<()> {
    if !matches!(usage.as_str(), "super_search" | "tagging") {
        return Err(AppError::msg("非法用途（super_search|tagging）"));
    }
    let conn = lock_db(&state)?;
    let Some(cid) = connection_id.filter(|s| !s.is_empty()) else {
        ai_connections::unbind_usage(&conn, &usage)?;
        return Ok(());
    };
    // 校验连接存在
    if ai_connections::get(&conn, &cid)?.is_none() {
        return Err(AppError::msg("连接档案不存在"));
    }
    ai_connections::bind_usage(&conn, &usage, &cid)
}

/// 读取两个用途的当前绑定（connection_id，无绑定为 null）。
#[tauri::command]
pub fn get_ai_usage_bindings(
    state: State<AppState>,
) -> AppResult<std::collections::HashMap<String, Option<String>>> {
    let conn = lock_db(&state)?;
    let mut out = std::collections::HashMap::new();
    for usage in ["super_search", "tagging"] {
        let id = ai_connections::binding_id(&conn, usage)?;
        out.insert(usage.to_string(), id);
    }
    Ok(out)
}

/// §6.3：AI 打标/搜索命令的旧版回退——读取默认 active 档案 id（未做连接绑定迁移前的兼容路径）。
/// 仅供 UI 展示「传统档案」状态；新代码一律走 ai_connections 绑定。
#[tauri::command]
pub fn get_legacy_active_profile(state: State<AppState>) -> AppResult<Option<String>> {
    let conn = lock_db(&state)?;
    let s = settings::get_settings(&conn)?;
    Ok(s.ai.active_profile_opt())
}

/// FB3-08（§10.2）：连接测试。密钥只在 Rust 侧从 keyring 读取（不经前端回显明文），
/// 按协议分支测试：OpenAI 兼容 / 本地 → GET /models（Bearer/无鉴权）；
/// Anthropic Messages → POST /messages（x-api-key，max_tokens=1 最小请求）。
#[tauri::command]
pub async fn test_ai_connection(
    state: State<'_, AppState>,
    connection_id: String,
) -> AppResult<crate::services::ai_cloud::AiConnectionTestResult> {
    // DB 与 keyring 都是阻塞调用：先在当前任务提取需要的数据（短锁），网络测试放 spawn_blocking。
    // State 不能 move 进 'static 闭包，所以这里克隆 Arc 后释放。
    let (base_url, protocol, model, api_key, managed_ollama) = {
        let conn = lock_db(&state)?;
        let c = ai_connections::get(&conn, &connection_id)?
            .ok_or_else(|| AppError::msg("连接档案不存在"))?;
        if c.base_url.trim().is_empty() {
            return Err(AppError::msg("该服务未填写地址，请先编辑并保存"));
        }
        // 从 keyring 读取密钥；未配置时按空串处理（本地服务通常无需密钥）
        let api_key = match &c.api_key_ref {
            Some(id) => credentials::get_api_key(id)?.unwrap_or_default(),
            None => String::new(),
        };
        let managed_ollama = cfg!(target_os = "windows")
            && c.deployment == "local"
            && c.protocol == "openai_chat"
            && ollama_runtime::is_managed_ollama_base(&c.base_url);
        (c.base_url, c.protocol, c.model, api_key, managed_ollama)
    };
    tauri::async_runtime::spawn_blocking(move || {
        Ok(crate::services::ai_cloud::test_connection(
            &base_url,
            &api_key,
            &protocol,
            &model,
            managed_ollama,
        ))
    })
    .await
    .map_err(|e| AppError::msg(format!("连接测试任务失败: {e}")))?
}

/// FB5-04（§3.6）：连接感知模型发现（可手填 combobox 的「读取模型列表」）。
/// - connection_id 提供时：密钥从 keyring 读取；显式 api_key（编辑中的草稿）优先于 keyring；
///   地址/协议/部署未显式提供时用档案保存值（草稿值可覆盖）。
/// - 无 connection_id：legacy 显式字段（AiTaggingPage 的 settings profile，apiKey 在 JSON 中，
///   经「临时 apiKey」路径传入）。
/// 网络请求 spawn_blocking；错误信息由 discover_models 分类（不含 key，URL 去 query）。
#[tauri::command]
pub async fn discover_ai_models(
    state: State<'_, AppState>,
    connection_id: Option<String>,
    deployment: Option<String>,
    protocol: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
) -> AppResult<Vec<String>> {
    let (base_url, protocol, deployment, key) = {
        let conn = lock_db(&state)?;
        if let Some(cid) = &connection_id {
            let c =
                ai_connections::get(&conn, cid)?.ok_or_else(|| AppError::msg("连接档案不存在"))?;
            // keyring 读取（阻塞）在短锁内完成；未配置时按空串处理（本地服务通常无需密钥）
            let saved_key = match &c.api_key_ref {
                Some(id) => credentials::get_api_key(id)?.unwrap_or_default(),
                None => String::new(),
            };
            // 草稿 key 优先于 saved key（§13.5）
            let key = match &api_key {
                Some(k) if !k.trim().is_empty() => k.trim().to_string(),
                _ => saved_key,
            };
            (
                base_url.unwrap_or(c.base_url),
                protocol.unwrap_or(c.protocol),
                deployment.unwrap_or(c.deployment),
                key,
            )
        } else {
            let b = base_url.ok_or_else(|| AppError::msg("请先填写服务地址"))?;
            (
                b,
                protocol.unwrap_or_else(|| "openai_chat".to_string()),
                deployment.unwrap_or_else(|| "cloud".to_string()),
                api_key.unwrap_or_default(),
            )
        }
    };
    let managed_ollama = cfg!(target_os = "windows")
        && deployment == "local"
        && protocol == "openai_chat"
        && ollama_runtime::is_managed_ollama_base(&base_url);
    tauri::async_runtime::spawn_blocking(move || {
        crate::services::ai_cloud::discover_models(&base_url, &key, &protocol, managed_ollama)
    })
    .await
    .map_err(|e| AppError::msg(format!("模型发现任务失败: {e}")))?
}
