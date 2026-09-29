//! AI 连接档案表（指导书 §6.3）：ai_connections（连接档案）+ ai_usage_bindings（用途绑定）。
//!  - `api_key_ref` 只保存凭据引用（= connection_id），明文 API Key 存系统凭据（credentials.rs）；
//!  - 用途绑定：super_search / tagging 各自指向一个 connection_id，可同可异；
//!  - 协议 protocol ∈ openai_chat | anthropic_messages（apiMode 迁移见 §6.4）。

use rusqlite::{params, Connection, OptionalExtension};

use crate::error::{AppError, AppResult};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AiConnection {
    pub id: String,
    pub name: String,
    pub deployment: String, // cloud | local
    pub protocol: String,   // openai_chat | anthropic_messages
    pub base_url: String,
    pub model: String,
    pub api_key_ref: Option<String>,
    /// 外部 AI 服务限流配置；0 表示不限。
    pub max_concurrency: i64,
    pub requests_per_minute: i64,
    pub requests_per_hour: i64,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 按 id 读取连接档案。
pub fn get(conn: &Connection, id: &str) -> AppResult<Option<AiConnection>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, deployment, protocol, base_url, model, api_key_ref,
                max_concurrency, requests_per_minute, requests_per_hour,
                enabled, created_at, updated_at
         FROM ai_connections WHERE id = ?1",
    )?;
    let mut rows = stmt.query(params![id])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(map_connection(row)?))
}

pub fn list(conn: &Connection) -> AppResult<Vec<AiConnection>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, deployment, protocol, base_url, model, api_key_ref,
                max_concurrency, requests_per_minute, requests_per_hour,
                enabled, created_at, updated_at
         FROM ai_connections ORDER BY created_at",
    )?;
    let rows = stmt.query_map([], map_connection)?;
    let mut out = Vec::new();
    for r in rows {
        out.push(r?);
    }
    Ok(out)
}

fn map_connection(row: &rusqlite::Row) -> rusqlite::Result<AiConnection> {
    Ok(AiConnection {
        id: row.get(0)?,
        name: row.get(1)?,
        deployment: row.get(2)?,
        protocol: row.get(3)?,
        base_url: row.get(4)?,
        model: row.get(5)?,
        api_key_ref: row.get(6)?,
        max_concurrency: row.get(7)?,
        requests_per_minute: row.get(8)?,
        requests_per_hour: row.get(9)?,
        enabled: row.get::<_, i64>(10)? != 0,
        created_at: row.get(11)?,
        updated_at: row.get(12)?,
    })
}

/// 按用途读取绑定（super_search / tagging）。
pub fn binding_for(conn: &Connection, usage: &str) -> AppResult<Option<AiConnection>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.name, c.deployment, c.protocol, c.base_url, c.model, c.api_key_ref,
                c.max_concurrency, c.requests_per_minute, c.requests_per_hour,
                c.enabled, c.created_at, c.updated_at
         FROM ai_usage_bindings b JOIN ai_connections c ON c.id = b.connection_id
         WHERE b.usage = ?1 AND c.enabled = 1",
    )?;
    let mut rows = stmt.query(params![usage])?;
    let Some(row) = rows.next()? else {
        return Ok(None);
    };
    Ok(Some(map_connection(row)?))
}

/// §6.4 反向映射 protocol → 老 apiMode（供现有 ai_cloud 管线消费）。
/// openai_chat → openai，anthropic_messages → anthropic，其余按 openai（含 warning）。
pub fn protocol_to_api_mode(protocol: &str) -> &'static str {
    match protocol {
        "anthropic_messages" => "anthropic",
        _ => "openai",
    }
}

/// 按用途读取 AI HTTP service 所需的纯数据库元数据。
/// keyring IO 必须在调用方释放 Database guard 后通过 `services::credentials` 完成。
pub fn usage_profile_metadata(
    conn: &Connection,
    usage: &str,
) -> AppResult<Option<(crate::db::settings::ApiProfile, Option<String>)>> {
    let Some(c) = binding_for(conn, usage)? else {
        return Ok(None);
    };
    let credential_ref = c.api_key_ref.clone();
    Ok(Some((profile_from_connection(&c), credential_ref)))
}

/// 把连接档案映射到现有 AI HTTP 管线使用的配置，不读取系统凭据。
pub fn profile_from_connection(c: &AiConnection) -> crate::db::settings::ApiProfile {
    crate::db::settings::ApiProfile {
        id: c.id.clone(),
        name: c.name.clone(),
        api_mode: protocol_to_api_mode(&c.protocol).to_string(),
        kind: c.deployment.clone(),
        base_url: c.base_url.clone(),
        api_key: String::new(),
        model: c.model.clone(),
        max_concurrency: c.max_concurrency,
        requests_per_minute: c.requests_per_minute,
        requests_per_hour: c.requests_per_hour,
    }
}

/// Backwards-compatible pure metadata projection; never reads the system keyring.
pub fn usage_profile(
    conn: &Connection,
    usage: &str,
) -> AppResult<Option<crate::db::settings::ApiProfile>> {
    Ok(usage_profile_metadata(conn, usage)?.map(|(profile, _)| profile))
}

/// 把用途绑定的连接纯元数据覆盖到调用方持有的 AiSettings 上（不动库内 settings 本体）；
/// 返回值只表示是否有绑定。密钥需在释放数据库锁后另行解析。
/// 返回是否命中绑定（false = 无绑定，调用方继续用默认 active 档案）。
pub fn apply_usage_binding(
    conn: &Connection,
    usage: &str,
    cfg: &mut crate::db::settings::AiSettings,
) -> AppResult<bool> {
    let Some(profile) = usage_profile(conn, usage)? else {
        return Ok(false);
    };
    apply_profile(cfg, profile);
    Ok(true)
}

/// Inject a resolved profile into an in-memory AI settings snapshot.
/// Credential lookup is deliberately not performed here; callers must resolve it
/// through `services::credentials` after releasing the DB guard.
pub fn apply_profile(
    cfg: &mut crate::db::settings::AiSettings,
    profile: crate::db::settings::ApiProfile,
) {
    // 把绑定连接注入 profiles（替换同 id，或追加），并设为 active
    let id = profile.id.clone();
    if let Some(existing) = cfg.profiles.iter_mut().find(|p| p.id == id) {
        *existing = profile;
    } else {
        cfg.profiles.push(profile);
    }
    cfg.active_profile = id;
}

/// 写入/覆盖连接档案（upsert）。api_key_ref 由调用方决定（成功写凭据后置为 id）。
// 8 参数为档案字段的内聚集合，收进结构体需同步改全部调用点，收益低，集中豁免。
#[allow(clippy::too_many_arguments)]
pub fn upsert(
    conn: &Connection,
    id: &str,
    name: &str,
    deployment: &str,
    protocol: &str,
    base_url: &str,
    model: &str,
    api_key_ref: Option<&str>,
) -> AppResult<()> {
    upsert_with_limits(
        conn,
        id,
        name,
        deployment,
        protocol,
        base_url,
        model,
        api_key_ref,
        0,
        0,
        0,
    )
}

/// 写入连接档案和限额配置。0 表示不限；非法数值在持久化前拒绝。
#[allow(clippy::too_many_arguments)]
pub fn upsert_with_limits(
    conn: &Connection,
    id: &str,
    name: &str,
    deployment: &str,
    protocol: &str,
    base_url: &str,
    model: &str,
    api_key_ref: Option<&str>,
    max_concurrency: i64,
    requests_per_minute: i64,
    requests_per_hour: i64,
) -> AppResult<()> {
    if !matches!(deployment, "cloud" | "local") {
        return Err(AppError::msg("非法部署类型（cloud|local）"));
    }
    if !matches!(protocol, "openai_chat" | "anthropic_messages") {
        return Err(AppError::msg("非法协议（openai_chat|anthropic_messages）"));
    }
    for (label, value, max) in [
        ("最大并发数", max_concurrency, 128),
        ("每分钟请求数", requests_per_minute, 1_000_000),
        ("每小时请求数", requests_per_hour, 10_000_000),
    ] {
        if !(0..=max).contains(&value) {
            return Err(AppError::invalid_arg(format!(
                "{label}必须是 0–{max} 的整数（0 表示不限）"
            )));
        }
    }
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "INSERT INTO ai_connections
           (id, name, deployment, protocol, base_url, model, api_key_ref,
            max_concurrency, requests_per_minute, requests_per_hour,
            enabled, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1, ?11, ?11)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name, deployment = excluded.deployment, protocol = excluded.protocol,
           base_url = excluded.base_url, model = excluded.model,
           api_key_ref = COALESCE(excluded.api_key_ref, ai_connections.api_key_ref),
           max_concurrency = excluded.max_concurrency,
           requests_per_minute = excluded.requests_per_minute,
           requests_per_hour = excluded.requests_per_hour,
           updated_at = excluded.updated_at",
        params![
            id,
            name,
            deployment,
            protocol,
            base_url,
            model,
            api_key_ref,
            max_concurrency,
            requests_per_minute,
            requests_per_hour,
            now
        ],
    )?;
    Ok(())
}

/// 绑定用途 → 连接（幂等 upsert）。
pub fn bind_usage(conn: &Connection, usage: &str, connection_id: &str) -> AppResult<()> {
    if !matches!(usage, "super_search" | "tagging") {
        return Err(AppError::msg("非法用途（super_search|tagging）"));
    }
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "INSERT INTO ai_usage_bindings (usage, connection_id, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT(usage) DO UPDATE SET connection_id = excluded.connection_id, updated_at = excluded.updated_at",
        params![usage, connection_id, now],
    )?;
    Ok(())
}

/// 清除某个用途绑定。
pub fn unbind_usage(conn: &Connection, usage: &str) -> AppResult<()> {
    if !matches!(usage, "super_search" | "tagging") {
        return Err(AppError::msg("非法用途（super_search|tagging）"));
    }
    conn.execute(
        "DELETE FROM ai_usage_bindings WHERE usage = ?1",
        params![usage],
    )?;
    Ok(())
}

/// 读取某个用途当前绑定的连接 id（未绑定返回 None）。
pub fn binding_id(conn: &Connection, usage: &str) -> AppResult<Option<String>> {
    if !matches!(usage, "super_search" | "tagging") {
        return Err(AppError::msg("非法用途（super_search|tagging）"));
    }
    Ok(conn
        .query_row(
            "SELECT connection_id FROM ai_usage_bindings WHERE usage = ?1",
            params![usage],
            |row| row.get(0),
        )
        .optional()?)
}

/// 删除连接档案（同时清除用途绑定）。
pub fn delete(conn: &Connection, id: &str) -> AppResult<()> {
    conn.execute(
        "DELETE FROM ai_usage_bindings WHERE connection_id = ?1",
        params![id],
    )?;
    conn.execute("DELETE FROM ai_connections WHERE id = ?1", params![id])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;

    #[test]
    fn upsert_get_binding_roundtrip() {
        let conn = init_memory().unwrap();
        upsert(
            &conn,
            "c1",
            "通义",
            "cloud",
            "openai_chat",
            "https://a/v1",
            "qwen-max",
            Some("c1"),
        )
        .unwrap();
        let c = get(&conn, "c1").unwrap().unwrap();
        assert_eq!(c.protocol, "openai_chat");
        assert!(c.enabled);
        assert_eq!(c.api_key_ref.as_deref(), Some("c1"));

        bind_usage(&conn, "tagging", "c1").unwrap();
        let b = binding_for(&conn, "tagging").unwrap().unwrap();
        assert_eq!(b.id, "c1");
        assert_eq!(b.base_url, "https://a/v1");
    }

    #[test]
    fn usage_bindings_are_independent() {
        let conn = init_memory().unwrap();
        upsert(&conn, "c1", "A", "cloud", "openai_chat", "u1", "m", None).unwrap();
        upsert(&conn, "c2", "B", "local", "openai_chat", "u2", "m", None).unwrap();
        bind_usage(&conn, "super_search", "c1").unwrap();
        bind_usage(&conn, "tagging", "c2").unwrap();
        assert_eq!(
            binding_for(&conn, "super_search").unwrap().unwrap().id,
            "c1"
        );
        assert_eq!(binding_for(&conn, "tagging").unwrap().unwrap().id, "c2");
        // 修改 super_search 不影响 tagging
        bind_usage(&conn, "super_search", "c2").unwrap();
        assert_eq!(
            binding_for(&conn, "super_search").unwrap().unwrap().id,
            "c2"
        );
        assert_eq!(binding_for(&conn, "tagging").unwrap().unwrap().id, "c2");
    }

    #[test]
    fn delete_clears_bindings() {
        let conn = init_memory().unwrap();
        upsert(&conn, "c1", "A", "cloud", "openai_chat", "u", "m", None).unwrap();
        bind_usage(&conn, "super_search", "c1").unwrap();
        delete(&conn, "c1").unwrap();
        assert!(get(&conn, "c1").unwrap().is_none());
        assert!(binding_for(&conn, "super_search").unwrap().is_none());
    }

    #[test]
    fn rejects_invalid_deployment_and_protocol() {
        let conn = init_memory().unwrap();
        assert!(upsert(&conn, "x", "X", "bad", "openai_chat", "u", "m", None).is_err());
        assert!(upsert(&conn, "x", "X", "cloud", "bad_proto", "u", "m", None).is_err());
    }

    #[test]
    fn rate_limit_fields_roundtrip_and_reject_invalid_values() {
        let conn = init_memory().unwrap();
        upsert_with_limits(
            &conn,
            "limited",
            "限流服务",
            "cloud",
            "openai_chat",
            "https://example.invalid/v1",
            "model",
            None,
            3,
            20,
            400,
        )
        .unwrap();
        let connection = get(&conn, "limited").unwrap().unwrap();
        assert_eq!(connection.max_concurrency, 3);
        assert_eq!(connection.requests_per_minute, 20);
        assert_eq!(connection.requests_per_hour, 400);
        assert!(upsert_with_limits(
            &conn,
            "bad",
            "invalid",
            "cloud",
            "openai_chat",
            "u",
            "m",
            None,
            -1,
            0,
            0
        )
        .is_err());
    }
}
