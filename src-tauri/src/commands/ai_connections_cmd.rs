//! AI 连接档案命令（指导书 §6.3/§4.4）：连接档案 CRUD（API Key 走系统凭据）+ 用途绑定。
//!  - API Key 不经过普通 settings JSON：保存时写 keyring，成功后才置 api_key_ref；
//!  - 用途绑定（super_search/tagging）独立可变：修改一个不影响另一个。

use tauri::State;

use crate::db::ai_connections;
use crate::db::settings;
use crate::error::{AppError, AppResult};
use crate::services::credentials;
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
    /// 是否已配置 API 密钥（不回显 plaintext；兼容旧 UI，等价 credentialStatus=="configured"）。
    pub has_key: bool,
    /// 三态凭据状态（B3）：configured|missing|unavailable。UI 三态以此为准。
    pub credential_status: String,
    /// 仅 unavailable 时的可执行文案（不含密钥值）。
    pub credential_message: Option<String>,
    pub enabled: bool,
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
            has_key: status.has_key(),
            credential_status: status.tag().to_string(),
            credential_message: status.message().map(str::to_owned),
            enabled: c.enabled,
        }
    }
}

fn lock_db(state: &AppState) -> AppResult<std::sync::MutexGuard<'_, rusqlite::Connection>> {
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
) -> AppResult<AiConnectionView> {
    if id.trim().is_empty() || name.trim().is_empty() {
        return Err(AppError::msg("连接名称与内部标识不能为空"));
    }
    if base_url.trim().is_empty() {
        return Err(AppError::msg("服务地址不能为空"));
    }
    let id_owned = id.trim().to_string();
    // B3：先在 DB 锁**之外**写 keyring 并读校验；新 key 只有确认可读回后才引用。
    // 空/None 保留原 key（不当删除）。
    let mut api_key_ref: Option<String> = None;
    if let Some(k) = &api_key {
        let trimmed = k.trim();
        if !trimmed.is_empty() {
            let id_for_key = id_owned.clone();
            let key_for_write = trimmed.to_string();
            tauri::async_runtime::spawn_blocking(move || {
                credentials::save_api_key(&id_for_key, &key_for_write)?;
                // 写后读校验：确认凭据可读回，避免「保存显示成功但实际未落盘」。
                match credentials::probe_status(&id_for_key) {
                    credentials::CredentialStatus::Configured => Ok(()),
                    credentials::CredentialStatus::Missing => {
                        Err(AppError::msg("凭据写入后无法读回，请检查系统密钥服务"))
                    }
                    credentials::CredentialStatus::Unavailable(m) => Err(AppError::unauthorized(m)),
                }
            })
            .await
            .map_err(|e| AppError::msg(format!("凭据写入任务失败: {e}")))??;
            api_key_ref = Some(id_owned.clone());
        }
    }
    // 短锁写 DB 并读回行
    let row = {
        let conn = lock_db(&state)?;
        ai_connections::upsert(
            &conn,
            &id_owned,
            name.trim(),
            &deployment,
            &protocol,
            base_url.trim(),
            model.trim(),
            api_key_ref.as_deref(),
        )?;
        ai_connections::get(&conn, &id_owned)?.ok_or_else(|| AppError::msg("保存后读取失败"))?
    };
    // 凭据状态探测在锁外
    let status = match &row.api_key_ref {
        Some(ref_id) => {
            let ref_id = ref_id.clone();
            tauri::async_runtime::spawn_blocking(move || credentials::probe_status(&ref_id))
                .await
                .map_err(|e| AppError::msg(format!("凭据状态探测任务失败: {e}")))?
        }
        None => credentials::CredentialStatus::Missing,
    };
    Ok(AiConnectionView::from_row(row, status))
}

/// 删除连接档案（同时删除用途绑定与系统凭据）。
///
/// §10：DB 锁内只做数据库删除；凭据删除在锁外执行。数据库删除成功、凭据删除失败时，
/// 连接已从列表消失（不再可用），残留 keyring 项不含引用、只是惰性垃圾，记录 warning
/// 供后续排查即可，不因凭据删除失败反过来判定整体失败。
#[tauri::command]
pub async fn delete_ai_connection(state: State<'_, AppState>, id: String) -> AppResult<()> {
    {
        let conn = lock_db(&state)?;
        ai_connections::delete(&conn, &id)?;
    }
    // 锁已释放：keyring 删除是阻塞系统调用，放锁外线程。
    let id_for_key = id.clone();
    let cred =
        tauri::async_runtime::spawn_blocking(move || credentials::delete_api_key(&id_for_key))
            .await
            .map_err(|e| AppError::msg(format!("凭据删除任务失败: {e}")))?;
    if let Err(e) = cred {
        tracing::warn!("连接 {id} 已删除，但系统凭据清理失败（残留项不含引用，可忽略）: {e}");
    }
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
    // §10：DB 锁内只读不含 plaintext 的连接 DTO；keyring 访问一律在锁外。
    // 先短锁复制连接行，释放锁，再读 keyring，最后 spawn_blocking 做网络测试。
    let (base_url, protocol, model, api_key_ref, is_local) = {
        let conn = lock_db(&state)?;
        let c = ai_connections::get(&conn, &connection_id)?
            .ok_or_else(|| AppError::msg("连接档案不存在"))?;
        if c.base_url.trim().is_empty() {
            return Err(AppError::msg("该服务未填写地址，请先编辑并保存"));
        }
        (
            c.base_url,
            c.protocol,
            c.model,
            c.api_key_ref,
            c.deployment == "local",
        )
    };
    // 锁已释放：从 keyring 读密钥（阻塞）；未配置时按空串处理（本地服务通常无需密钥）。
    let api_key = match api_key_ref {
        Some(id) => tauri::async_runtime::spawn_blocking(move || credentials::get_api_key(&id))
            .await
            .map_err(|e| AppError::msg(format!("凭据读取任务失败: {e}")))??
            .unwrap_or_default(),
        None => String::new(),
    };
    tauri::async_runtime::spawn_blocking(move || {
        Ok(crate::services::ai_cloud::test_connection(
            &base_url, &api_key, &protocol, &model, is_local,
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
    // §10：DB 锁内只读不含 plaintext 的连接 DTO（含 api_key_ref），keyring 读取移到锁外。
    let (base_url, protocol, deployment, api_key_ref) = {
        let conn = lock_db(&state)?;
        if let Some(cid) = &connection_id {
            let c =
                ai_connections::get(&conn, cid)?.ok_or_else(|| AppError::msg("连接档案不存在"))?;
            (
                base_url.unwrap_or(c.base_url),
                protocol.unwrap_or(c.protocol),
                deployment.unwrap_or(c.deployment),
                c.api_key_ref,
            )
        } else {
            let b = base_url.ok_or_else(|| AppError::msg("请先填写服务地址"))?;
            (
                b,
                protocol.unwrap_or_else(|| "openai_chat".to_string()),
                deployment.unwrap_or_else(|| "cloud".to_string()),
                None,
            )
        }
    };
    // 锁已释放：草稿 key 优先于 keyring 保存值（§13.5）；keyring 读取（阻塞）在锁外完成。
    let key = match &api_key {
        Some(k) if !k.trim().is_empty() => k.trim().to_string(),
        _ => match api_key_ref {
            Some(id) => tauri::async_runtime::spawn_blocking(move || credentials::get_api_key(&id))
                .await
                .map_err(|e| AppError::msg(format!("凭据读取任务失败: {e}")))??
                .unwrap_or_default(),
            None => String::new(),
        },
    };
    tauri::async_runtime::spawn_blocking(move || {
        crate::services::ai_cloud::discover_models(
            &base_url,
            &key,
            &protocol,
            &deployment == "local",
        )
    })
    .await
    .map_err(|e| AppError::msg(format!("模型发现任务失败: {e}")))?
}
