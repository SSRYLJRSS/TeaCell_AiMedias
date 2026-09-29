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
    /// 是否已配置 API 密钥（不回显 plaintext；兼容旧 UI，等价 credentialStatus=="configured"）。
    pub has_key: bool,
    /// 三态凭据状态（B3）：configured|missing|unavailable。UI 三态以此为准。
    pub credential_status: String,
    /// 仅 unavailable 时的可执行文案（不含密钥值）。
    pub credential_message: Option<String>,
    pub enabled: bool,
}

/// 超级搜索当前真正会使用的连接状态；不包含服务地址或凭据。
#[derive(Debug, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SuperSearchServiceResolutionView {
    pub ready: bool,
    pub source: String,
    pub connection_id: Option<String>,
    pub name: Option<String>,
    pub model: Option<String>,
    pub deployment: Option<String>,
    pub message: Option<String>,
}

impl AiConnectionView {
    fn from_row(c: ai_connections::AiConnection, status: credentials::CredentialStatus) -> Self {
        Self {
            id: c.id,
            name: c.name,
            deployment: c.deployment,
            protocol: c.protocol,
            base_url: c.base_url,
            model: c.model,
            max_concurrency: c.max_concurrency,
            requests_per_minute: c.requests_per_minute,
            requests_per_hour: c.requests_per_hour,
            has_key: status.has_key(),
            credential_status: status.tag().to_string(),
            credential_message: status.message().map(str::to_owned),
            enabled: c.enabled,
        }
    }
}

fn lock_db(state: &AppState) -> AppResult<crate::state::DbConnectionGuard<'_>> {
    state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))
}

/// 列出全部连接档案（含 key 三态状态）。
///
/// B3：先短锁读取行、立即释放锁，再在阻塞工作线程逐个探测 keyring；
/// 一条不可读只标该条 unavailable，其它连接仍可编辑使用（不再让 `?` 中断整个列表）。
#[tauri::command]
pub async fn list_ai_connections(state: State<'_, AppState>) -> AppResult<Vec<AiConnectionView>> {
    // 短锁取行后释放
    let rows = {
        let conn = lock_db(&state)?;
        ai_connections::list(&conn)?
    };
    // keyring 访问在阻塞线程（锁外）
    tauri::async_runtime::spawn_blocking(move || {
        rows.into_iter()
            .map(|c| {
                let status = match &c.api_key_ref {
                    Some(id) => credentials::probe_status(id),
                    None => credentials::CredentialStatus::Missing,
                };
                AiConnectionView::from_row(c, status)
            })
            .collect::<Vec<_>>()
    })
    .await
    .map_err(|e| AppError::msg(format!("凭据状态探测任务失败: {e}")))
}

/// 保存连接档案。api_key 为 Some(非空) 时写入系统凭据（keyring）并置 api_key_ref；
/// 为空/None 时保留原有密钥配置（不覆盖）。
// 参数即 IPC 契约（前端 invoke 按字段名传参），收进结构体会破坏前端调用，故平铺并豁免。
#[tauri::command]
#[allow(clippy::too_many_arguments)]
pub async fn save_ai_connection(
    state: State<'_, AppState>,
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
    let id_owned = id.trim().to_string();
    let db = std::sync::Arc::clone(&state.db);
    let name = name.trim().to_string();
    let base_url = base_url.trim().to_string();
    let model = model.trim().to_string();
    let key_to_write = api_key
        .filter(|key| !key.trim().is_empty())
        .map(|key| key.trim().to_string());
    tauri::async_runtime::spawn_blocking(move || {
        // Lock order: credential operation -> short DB reads/writes -> keyring IO
        // only while no DB guard is held. The keyring write is compensated if DB
        // upsert fails, so an old working key is never silently replaced.
        let _operations = credentials::lock_operations()?;
        let existing = {
            let conn = db.lock()?;
            ai_connections::get(&conn, &id_owned)?
        };
        let max_concurrency = max_concurrency
            .or_else(|| {
                existing
                    .as_ref()
                    .map(|connection| connection.max_concurrency)
            })
            .unwrap_or(0);
        let requests_per_minute = requests_per_minute
            .or_else(|| {
                existing
                    .as_ref()
                    .map(|connection| connection.requests_per_minute)
            })
            .unwrap_or(0);
        let requests_per_hour = requests_per_hour
            .or_else(|| {
                existing
                    .as_ref()
                    .map(|connection| connection.requests_per_hour)
            })
            .unwrap_or(0);
        let write_row = || -> AppResult<ai_connections::AiConnection> {
            let mut conn = db.lock()?;
            let tx = conn.transaction()?;
            ai_connections::upsert_with_limits(
                &tx,
                &id_owned,
                &name,
                &deployment,
                &protocol,
                &base_url,
                &model,
                key_to_write.as_ref().map(|_| id_owned.as_str()),
                max_concurrency,
                requests_per_minute,
                requests_per_hour,
            )?;
            let row = ai_connections::get(&tx, &id_owned)?
                .ok_or_else(|| AppError::msg("保存后读取失败"))?;
            tx.commit()?;
            Ok(row)
        };
        let row = match &key_to_write {
            Some(key) => credentials::replace_with_commit(
                &credentials::SystemCredentialBackend,
                &id_owned,
                key,
                write_row,
            )?,
            None => write_row()?,
        };
        let status = match &row.api_key_ref {
            Some(_) if key_to_write.is_some() => credentials::CredentialStatus::Configured,
            Some(reference) => credentials::probe_status(reference),
            None => credentials::CredentialStatus::Missing,
        };
        Ok(AiConnectionView::from_row(row, status))
    })
    .await
    .map_err(|e| AppError::msg(format!("连接保存任务失败: {e}")))?
}

/// 删除连接档案（同时删除用途绑定与系统凭据）。
#[tauri::command]
pub async fn delete_ai_connection(state: State<'_, AppState>, id: String) -> AppResult<()> {
    let db = std::sync::Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || {
        let _operations = credentials::lock_operations()?;
        let row = {
            let conn = db.lock()?;
            ai_connections::get(&conn, &id)?.ok_or_else(|| AppError::not_found("连接档案不存在"))?
        };
        let delete_row = || -> AppResult<()> {
            let mut conn = db.lock()?;
            let tx = conn.transaction()?;
            ai_connections::delete(&tx, &id)?;
            tx.commit()?;
            Ok(())
        };
        match row.api_key_ref.as_deref() {
            Some(reference) => credentials::delete_with_commit(
                &credentials::SystemCredentialBackend,
                reference,
                delete_row,
            ),
            None => delete_row(),
        }
    })
    .await
    .map_err(|e| AppError::msg(format!("连接删除任务失败: {e}")))?
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
    // 禁用连接不能成为新的功能绑定；当前已有的失效绑定仍可通过解绑恢复。
    let connection =
        ai_connections::get(&conn, &cid)?.ok_or_else(|| AppError::not_found("连接档案不存在"))?;
    if !connection.enabled {
        return Err(AppError::conflict("该 AI 服务已停用，请先启用后再选择"));
    }
    ai_connections::bind_usage(&conn, &usage, &cid)
}

/// 返回超级搜索实际解析出的服务。未绑定时只按在线连接自动选择，不能读取旧版 activeProfile。
#[tauri::command]
pub async fn get_super_search_service_resolution(
    state: State<'_, AppState>,
) -> AppResult<SuperSearchServiceResolutionView> {
    let db = std::sync::Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || {
        let binding_id = {
            let conn = db.lock()?;
            ai_connections::binding_id(&conn, "super_search")?
        };
        match credentials::resolve_super_search_profile_with_system_credential(&db) {
            Ok(resolved) => Ok(SuperSearchServiceResolutionView {
                ready: true,
                source: match resolved.source {
                    credentials::SuperSearchProfileSource::ExplicitBinding => "explicitBinding",
                    credentials::SuperSearchProfileSource::AutomaticOnline => "automaticOnline",
                }
                .to_string(),
                connection_id: Some(resolved.connection_id),
                name: Some(resolved.name),
                model: Some(resolved.model),
                deployment: Some(resolved.deployment),
                message: None,
            }),
            Err(error) => Ok(SuperSearchServiceResolutionView {
                ready: false,
                source: if binding_id.is_some() {
                    "explicitBinding"
                } else {
                    "automaticOnline"
                }
                .to_string(),
                connection_id: binding_id,
                name: None,
                model: None,
                deployment: None,
                message: Some(error.to_string()),
            }),
        }
    })
    .await
    .map_err(|e| AppError::msg(format!("超级搜索服务状态读取任务失败: {e}")))?
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
    let db = std::sync::Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || {
        // helper 获取元数据后会先释放 DB guard，再读取系统 keyring；此后才发起网络探测。
        let (c, key) = credentials::connection_with_system_credential(&db, &connection_id)?;
        if c.base_url.trim().is_empty() {
            return Err(AppError::msg("该服务未填写地址，请先编辑并保存"));
        }
        let managed_ollama = cfg!(target_os = "windows")
            && c.deployment == "local"
            && c.protocol == "openai_chat"
            && ollama_runtime::is_managed_ollama_base(&c.base_url);
        Ok(crate::services::ai_cloud::test_connection(
            &c.base_url,
            key.as_deref().unwrap_or_default(),
            &c.protocol,
            &c.model,
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
    let db = std::sync::Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || {
        let (base_url, protocol, deployment, key) = if let Some(cid) = connection_id {
            // DB guard is released by the credential helper before keyring access.
            let (connection, saved_key) =
                credentials::connection_with_system_credential(&db, &cid)?;
            let key = match &api_key {
                Some(k) if !k.trim().is_empty() => k.trim().to_string(),
                _ => saved_key.unwrap_or_default(),
            };
            (
                base_url.unwrap_or(connection.base_url),
                protocol.unwrap_or(connection.protocol),
                deployment.unwrap_or(connection.deployment),
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
        };
        let managed_ollama = cfg!(target_os = "windows")
            && deployment == "local"
            && protocol == "openai_chat"
            && ollama_runtime::is_managed_ollama_base(&base_url);
        crate::services::ai_cloud::discover_models(&base_url, &key, &protocol, managed_ollama)
    })
    .await
    .map_err(|e| AppError::msg(format!("模型发现任务失败: {e}")))?
}
