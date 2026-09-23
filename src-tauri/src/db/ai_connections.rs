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
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

/// 按 id 读取连接档案。
pub fn get(conn: &Connection, id: &str) -> AppResult<Option<AiConnection>> {
    let mut stmt = conn.prepare(
        "SELECT id, name, deployment, protocol, base_url, model, api_key_ref, enabled, created_at, updated_at
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
        "SELECT id, name, deployment, protocol, base_url, model, api_key_ref, enabled, created_at, updated_at
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
        enabled: row.get::<_, i64>(7)? != 0,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

/// 按用途读取绑定（super_search / tagging）。
pub fn binding_for(conn: &Connection, usage: &str) -> AppResult<Option<AiConnection>> {
    let mut stmt = conn.prepare(
        "SELECT c.id, c.name, c.deployment, c.protocol, c.base_url, c.model, c.api_key_ref,
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

/// 用途绑定解析结果（§10）：不含 plaintext 的 ApiProfile 骨架 + 凭据引用。
/// db 层不访问系统凭据；调用方在**释放 DB 锁后**用 `api_key_ref` 读 keyring 补全 profile。
pub struct UsageProfileRef {
    /// api_key 字段固定为空串；由锁外的 keyring 读取填充。
    pub profile: crate::db::settings::ApiProfile,
    /// 凭据引用（= connection_id）；None 表示无需密钥（常见本地服务）。
    pub api_key_ref: Option<String>,
}

/// 按用途解析出 ApiProfile 骨架（不含明文密钥）+ 凭据引用（§10）。
/// db 层只读数据库、不碰 keyring；明文密钥由 service/command 在锁外组装。
pub fn usage_profile_ref(conn: &Connection, usage: &str) -> AppResult<Option<UsageProfileRef>> {
    let Some(c) = binding_for(conn, usage)? else {
        return Ok(None);
    };
    Ok(Some(UsageProfileRef {
        profile: crate::db::settings::ApiProfile {
            id: c.id.clone(),
            name: c.name.clone(),
            api_mode: protocol_to_api_mode(&c.protocol).to_string(),
            kind: c.deployment.clone(),
            base_url: c.base_url.clone(),
            api_key: String::new(),
            model: c.model.clone(),
        },
        api_key_ref: c.api_key_ref,
    }))
}

/// 把（已在锁外补全明文密钥的）绑定 profile 覆盖到调用方持有的 AiSettings 上。
/// 纯内存操作：不读 DB、不读 keyring，使现有 `cfg.active()` 直接命中绑定档案（§4.4）。
pub fn apply_profile_to_cfg(
    cfg: &mut crate::db::settings::AiSettings,
    profile: crate::db::settings::ApiProfile,
) {
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
    if !matches!(deployment, "cloud" | "local") {
        return Err(AppError::msg("非法部署类型（cloud|local）"));
    }
    if !matches!(protocol, "openai_chat" | "anthropic_messages") {
        return Err(AppError::msg("非法协议（openai_chat|anthropic_messages）"));
    }
    let now = chrono::Utc::now().timestamp_millis();
    conn.execute(
        "INSERT INTO ai_connections
           (id, name, deployment, protocol, base_url, model, api_key_ref, enabled, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?8)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name, deployment = excluded.deployment, protocol = excluded.protocol,
           base_url = excluded.base_url, model = excluded.model,
           api_key_ref = COALESCE(excluded.api_key_ref, ai_connections.api_key_ref),
           updated_at = excluded.updated_at",
        params![id, name, deployment, protocol, base_url, model, api_key_ref, now],
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

    /// P1-01：新安装环境无 legacy profile，只有「超级搜索」用途绑定时，
    /// 解析绑定并注入后 `active()` 必须命中，不会被误判为「未配置」。
    #[test]
    fn usage_binding_populates_active_for_fresh_install() {
        let conn = init_memory().unwrap();
        upsert(
            &conn,
            "conn-new",
            "新中转",
            "cloud",
            "openai_chat",
            "https://new/v1",
            "gpt-4o-mini",
            Some("conn-new"),
        )
        .unwrap();
        bind_usage(&conn, "super_search", "conn-new").unwrap();

        // 模拟全新 settings：无任何 legacy profile。
        let mut ai = crate::db::settings::AiSettings::default();
        assert!(ai.active().is_none(), "全新环境无 legacy 档案");

        // 按 super_search_cmd 的顺序解析并注入。
        let bref = usage_profile_ref(&conn, "super_search").unwrap().unwrap();
        assert_eq!(bref.api_key_ref.as_deref(), Some("conn-new"));
        apply_profile_to_cfg(&mut ai, bref.profile);

        // 注入后 active() 命中绑定档案。
        let active = ai.active().expect("绑定注入后 active 必须命中");
        assert_eq!(active.id, "conn-new");
        assert_eq!(active.base_url, "https://new/v1");
        assert_eq!(active.model, "gpt-4o-mini");
    }

    /// P1-01：绑定优先于 legacy active——存在 legacy 档案但绑定指向另一连接时，
    /// 注入后 active 指向绑定连接。
    #[test]
    fn usage_binding_overrides_legacy_active() {
        let conn = init_memory().unwrap();
        upsert(
            &conn,
            "conn-bound",
            "绑定连接",
            "cloud",
            "openai_chat",
            "https://bound/v1",
            "model-b",
            Some("conn-bound"),
        )
        .unwrap();
        bind_usage(&conn, "super_search", "conn-bound").unwrap();

        // legacy 档案指向另一个连接
        let mut ai = crate::db::settings::AiSettings {
            active_profile: "legacy".into(),
            profiles: vec![crate::db::settings::ApiProfile {
                id: "legacy".into(),
                name: "旧档案".into(),
                api_mode: "openai".into(),
                kind: "cloud".into(),
                base_url: "https://legacy/v1".into(),
                api_key: "sk-legacy".into(),
                model: "model-legacy".into(),
            }],
            ..Default::default()
        };
        assert_eq!(ai.active().unwrap().id, "legacy");

        let bref = usage_profile_ref(&conn, "super_search").unwrap().unwrap();
        apply_profile_to_cfg(&mut ai, bref.profile);
        assert_eq!(ai.active().unwrap().id, "conn-bound", "绑定优先于 legacy");
    }
}
