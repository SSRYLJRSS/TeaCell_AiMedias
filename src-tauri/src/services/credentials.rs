//! 凭据服务（指导书 §6.3）：API Key 存系统凭据存储（Windows Credential Manager）。
//!  - service = "bagertea_ai_media_v2"，username = connection_id；
//!  - Secret 内容只保存 API Key；JSON/SQLite 只保存 api_key_ref（= connection_id），不保存明文；
//!  - 读取设置时兼容旧 apiKey；首次成功保存时迁移到 keyring；迁移失败不得清空旧 key（必须显示错误）。

use crate::error::{AppError, AppResult};
use crate::state::Database;
use std::sync::{Mutex, MutexGuard};

/// 与数据库 service 名保持一致：bagertea_ai_media_v2（Cargo package name）
pub const KEYRING_SERVICE: &str = "bagertea_ai_media_v2";

/// Serializes operations that modify or resolve connection credentials.
/// Required lock order: credential-operation mutex, then a short `Database::lock`,
/// then release the DB guard before calling the backend. Never acquire this mutex
/// while holding a database guard, and never hold it across network requests.
static CREDENTIAL_OPERATION_LOCK: Mutex<()> = Mutex::new(());

pub fn lock_operations() -> AppResult<MutexGuard<'static, ()>> {
    CREDENTIAL_OPERATION_LOCK
        .lock()
        .map_err(|_| AppError::internal("凭据操作锁中毒"))
}

/// Injectable system-credential boundary. Production uses the OS keyring; tests
/// use deterministic memory/failure backends without touching user credentials.
pub trait CredentialBackend: Send + Sync {
    fn get(&self, connection_id: &str) -> AppResult<Option<String>>;
    fn save(&self, connection_id: &str, api_key: &str) -> AppResult<()>;
    fn delete(&self, connection_id: &str) -> AppResult<()>;
}

#[derive(Debug, Clone, Copy, Default)]
pub struct SystemCredentialBackend;

impl CredentialBackend for SystemCredentialBackend {
    fn get(&self, connection_id: &str) -> AppResult<Option<String>> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, connection_id)
            .map_err(|e| AppError::msg(format!("凭据存储初始化失败: {e}")))?;
        match entry.get_password() {
            Ok(pw) => Ok(Some(pw)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(AppError::msg(format!("读取 API 密钥失败: {e}"))),
        }
    }

    fn save(&self, connection_id: &str, api_key: &str) -> AppResult<()> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, connection_id)
            .map_err(|e| AppError::msg(format!("凭据存储初始化失败: {e}")))?;
        entry
            .set_password(api_key)
            .map_err(|e| AppError::msg(format!("API 密钥保存到系统凭据失败: {e}")))
    }

    fn delete(&self, connection_id: &str) -> AppResult<()> {
        let entry = keyring::Entry::new(KEYRING_SERVICE, connection_id)
            .map_err(|e| AppError::msg(format!("凭据存储初始化失败: {e}")))?;
        match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(AppError::msg(format!("删除 API 密钥失败: {e}"))),
        }
    }
}

/// 凭据三态（三端复核 B3）：
/// - `Configured`：已存在且非空密钥。
/// - `Missing`：NoEntry，即未配置（正常空态，可编辑保存）。
/// - `Unavailable`：系统密钥服务不可用/锁定/被拒/平台错误。**必须与 Missing 区分**：
///   mock 或原生 Secret Service 缺失产生的错误不能当成「未配置」，否则会误导用户重复保存。
///
/// `message` 给出可执行文案（不含密钥值），供 UI 直接展示。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialStatus {
    Configured,
    Missing,
    Unavailable(String),
}

impl CredentialStatus {
    /// 序列化给前端的稳定标签。
    pub fn tag(&self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Missing => "missing",
            Self::Unavailable(_) => "unavailable",
        }
    }

    /// 可选可执行文案（仅 unavailable 携带）。
    pub fn message(&self) -> Option<&str> {
        match self {
            Self::Unavailable(m) => Some(m.as_str()),
            _ => None,
        }
    }

    /// 兼容旧 UI 的布尔：仅 Configured 为 true。
    pub fn has_key(&self) -> bool {
        matches!(self, Self::Configured)
    }
}

/// 探测某连接的凭据状态，**不返回错误**：把不可用归入 `Unavailable`，让列表中一条坏连接
/// 不阻断其它连接（B3：`?` 让一条失败中断整个列表是缺陷）。
///
/// 调用方应在 DB 锁**之外**、阻塞工作线程中调用（keyring 可能因锁定/DBus 而慢）。
pub fn probe_status(connection_id: &str) -> CredentialStatus {
    let entry = match keyring::Entry::new(KEYRING_SERVICE, connection_id) {
        Ok(e) => e,
        Err(e) => return CredentialStatus::Unavailable(unavailable_message(&e)),
    };
    match entry.get_password() {
        Ok(pw) if !pw.is_empty() => CredentialStatus::Configured,
        Ok(_) => CredentialStatus::Missing,
        Err(keyring::Error::NoEntry) => CredentialStatus::Missing,
        Err(e) => CredentialStatus::Unavailable(unavailable_message(&e)),
    }
}

/// 把 keyring 错误映射为可执行中文文案；不回显系统英文报错，也不区分泄露密钥。
fn unavailable_message(err: &keyring::Error) -> String {
    match err {
        keyring::Error::NoStorageAccess(_) | keyring::Error::PlatformFailure(_) => {
            "系统密钥服务不可用，请启动并解锁桌面密钥环后重试".to_string()
        }
        _ => "读取系统密钥服务失败，请检查桌面密钥环状态后重试".to_string(),
    }
}

/// 保存 API Key。username = connection_id；失败返回可解释错误（不吞）。
pub fn save_api_key(connection_id: &str, api_key: &str) -> AppResult<()> {
    SystemCredentialBackend.save(connection_id, api_key)
}

/// 读取 API Key。未配置返回 None；读取错误（如凭据损坏）返回错误而非静默吞。
pub fn get_api_key(connection_id: &str) -> AppResult<Option<String>> {
    SystemCredentialBackend.get(connection_id)
}

/// 删除 API Key（连接档案删除时清理凭据）。未配置视为成功。
pub fn delete_api_key(connection_id: &str) -> AppResult<()> {
    SystemCredentialBackend.delete(connection_id)
}

/// Read the profile row with a short DB guard, release it, then read its secret.
/// The credential-operation lock is acquired first and is not held by network callers.
pub fn connection_with_credential<B: CredentialBackend>(
    db: &Database,
    connection_id: &str,
    backend: &B,
) -> AppResult<(crate::db::ai_connections::AiConnection, Option<String>)> {
    let _operations = lock_operations()?;
    let row = {
        let conn = db.lock()?;
        crate::db::ai_connections::get(&conn, connection_id)?
            .ok_or_else(|| AppError::not_found("连接档案不存在"))?
    };
    let secret = match row.api_key_ref.as_deref() {
        Some(reference) => backend.get(reference)?,
        None => None,
    };
    Ok((row, secret))
}

/// Resolve only metadata while holding the DB guard; keyring access happens after
/// that guard is gone. `None` means no usage binding exists.
pub fn usage_profile_with_credential<B: CredentialBackend>(
    db: &Database,
    usage: &str,
    backend: &B,
) -> AppResult<Option<crate::db::settings::ApiProfile>> {
    let _operations = lock_operations()?;
    let resolved = {
        let conn = db.lock()?;
        crate::db::ai_connections::usage_profile_metadata(&conn, usage)?
    };
    let Some((mut profile, credential_ref)) = resolved else {
        return Ok(None);
    };
    if let Some(reference) = credential_ref {
        profile.api_key = backend.get(&reference)?.unwrap_or_default();
    }
    Ok(Some(profile))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SuperSearchProfileSource {
    ExplicitBinding,
    AutomaticOnline,
}

#[derive(Debug, Clone)]
pub struct ResolvedSuperSearchProfile {
    pub profile: crate::db::settings::ApiProfile,
    pub connection_id: String,
    pub name: String,
    pub model: String,
    pub deployment: String,
    pub source: SuperSearchProfileSource,
}

/// Resolve the actual super-search connection. An explicit binding wins; otherwise
/// only enabled cloud connections are considered. The legacy active profile and
/// unbound local connections are deliberately outside this policy.
pub fn resolve_super_search_profile_with_credential<B: CredentialBackend>(
    db: &Database,
    backend: &B,
) -> AppResult<ResolvedSuperSearchProfile> {
    let _operations = lock_operations()?;
    let (binding_id, connections) = {
        let conn = db.lock()?;
        (
            crate::db::ai_connections::binding_id(&conn, "super_search")?,
            crate::db::ai_connections::list(&conn)?,
        )
    };

    if let Some(binding_id) = binding_id {
        let connection = connections
            .into_iter()
            .find(|connection| connection.id == binding_id)
            .ok_or_else(|| AppError::not_found("超级搜索绑定的 AI 服务不存在，请重新选择服务"))?;
        if !connection.enabled {
            return Err(AppError::conflict(
                "超级搜索绑定的 AI 服务已停用，请启用该服务或重新选择",
            ));
        }
        if connection.base_url.trim().is_empty() || connection.model.trim().is_empty() {
            return Err(AppError::invalid_arg(
                "超级搜索绑定的 AI 服务缺少服务地址或模型，请先完善连接配置",
            ));
        }
        let mut profile = crate::db::ai_connections::profile_from_connection(&connection);
        if let Some(reference) = connection.api_key_ref.as_deref() {
            profile.api_key = backend
                .get(reference)?
                .filter(|secret| !secret.trim().is_empty())
                .ok_or_else(|| {
                    AppError::not_found("超级搜索绑定的 AI 服务密钥不可用，请重新保存密钥")
                })?;
        }
        return Ok(resolved_super_search_profile(
            connection,
            profile,
            SuperSearchProfileSource::ExplicitBinding,
        ));
    }

    let mut cloud_connections: Vec<_> = connections
        .into_iter()
        .filter(|connection| connection.enabled && connection.deployment == "cloud")
        .collect();
    cloud_connections.sort_by(|left, right| {
        left.created_at
            .cmp(&right.created_at)
            .then_with(|| left.id.cmp(&right.id))
    });
    if cloud_connections.is_empty() {
        return Err(AppError::not_found(
            "尚未配置可用的在线 AI 服务。请先添加在线服务，或在超级搜索设置中手动选择本机服务。",
        ));
    }

    let mut keyless_candidate = None;
    let mut missing_credential = false;
    for connection in cloud_connections {
        if connection.base_url.trim().is_empty() || connection.model.trim().is_empty() {
            continue;
        }
        match connection.api_key_ref.as_deref() {
            Some(reference) => match backend.get(reference)? {
                Some(secret) if !secret.trim().is_empty() => {
                    let mut profile =
                        crate::db::ai_connections::profile_from_connection(&connection);
                    profile.api_key = secret;
                    return Ok(resolved_super_search_profile(
                        connection,
                        profile,
                        SuperSearchProfileSource::AutomaticOnline,
                    ));
                }
                _ => missing_credential = true,
            },
            None => {
                if keyless_candidate.is_none() {
                    keyless_candidate = Some(connection);
                }
            }
        }
    }
    if let Some(connection) = keyless_candidate {
        let profile = crate::db::ai_connections::profile_from_connection(&connection);
        return Ok(resolved_super_search_profile(
            connection,
            profile,
            SuperSearchProfileSource::AutomaticOnline,
        ));
    }
    if missing_credential {
        return Err(AppError::not_found(
            "已配置在线 AI 服务，但密钥不可用。请在 AI 服务管理中重新保存密钥后重试。",
        ));
    }
    Err(AppError::not_found(
        "未找到配置完整的在线 AI 服务，请检查服务地址和模型名称。",
    ))
}

pub fn resolve_super_search_profile_with_system_credential(
    db: &Database,
) -> AppResult<ResolvedSuperSearchProfile> {
    resolve_super_search_profile_with_credential(db, &SystemCredentialBackend)
}

fn resolved_super_search_profile(
    connection: crate::db::ai_connections::AiConnection,
    profile: crate::db::settings::ApiProfile,
    source: SuperSearchProfileSource,
) -> ResolvedSuperSearchProfile {
    ResolvedSuperSearchProfile {
        profile,
        connection_id: connection.id,
        name: connection.name,
        model: connection.model,
        deployment: connection.deployment,
        source,
    }
}

pub fn connection_with_system_credential(
    db: &Database,
    connection_id: &str,
) -> AppResult<(crate::db::ai_connections::AiConnection, Option<String>)> {
    connection_with_credential(db, connection_id, &SystemCredentialBackend)
}

pub fn usage_profile_with_system_credential(
    db: &Database,
    usage: &str,
) -> AppResult<Option<crate::db::settings::ApiProfile>> {
    usage_profile_with_credential(db, usage, &SystemCredentialBackend)
}

/// Replace a secret and commit its DB reference atomically from the user's point
/// of view. Caller must hold `lock_operations()` and the commit closure must not
/// perform credential IO. Any write/readback/DB failure restores the old secret.
pub fn replace_with_commit<B: CredentialBackend, T>(
    backend: &B,
    connection_id: &str,
    api_key: &str,
    commit: impl FnOnce() -> AppResult<T>,
) -> AppResult<T> {
    let previous = backend.get(connection_id)?;
    let operation = (|| {
        backend.save(connection_id, api_key)?;
        match backend.get(connection_id)? {
            Some(saved) if saved == api_key => Ok(()),
            _ => Err(AppError::msg("凭据写入后无法读回，请检查系统密钥服务")),
        }
    })();
    if let Err(error) = operation {
        return Err(compensate_error(
            error,
            restore(backend, connection_id, previous.as_deref()),
            "凭据写入失败",
        ));
    }
    match commit() {
        Ok(value) => Ok(value),
        Err(error) => Err(compensate_error(
            error,
            restore(backend, connection_id, previous.as_deref()),
            "数据库保存失败",
        )),
    }
}

/// Delete many secrets before a DB reset. Every prior secret is captured first;
/// any delete or DB failure restores all attempted entries. Caller must hold the
/// credential-operation lock and the DB commit closure must not touch keyring.
pub fn delete_many_with_commit<B: CredentialBackend, T>(
    backend: &B,
    connection_ids: &[String],
    commit: impl FnOnce() -> AppResult<T>,
) -> AppResult<T> {
    let mut snapshots = Vec::with_capacity(connection_ids.len());
    for id in connection_ids {
        if snapshots
            .iter()
            .any(|(seen, _): &(String, Option<String>)| seen == id)
        {
            continue;
        }
        snapshots.push((id.clone(), backend.get(id)?));
    }

    let mut attempted = Vec::new();
    for (id, _) in &snapshots {
        attempted.push(id.clone());
        if let Err(error) = backend.delete(id) {
            return Err(compensate_many_error(
                error,
                backend,
                &snapshots,
                &attempted,
                "凭据删除失败",
            ));
        }
    }
    match commit() {
        Ok(value) => Ok(value),
        Err(error) => Err(compensate_many_error(
            error,
            backend,
            &snapshots,
            &attempted,
            "数据库重置失败",
        )),
    }
}

/// Delete one secret and its DB row with compensation if the DB mutation fails.
pub fn delete_with_commit<B: CredentialBackend, T>(
    backend: &B,
    credential_ref: &str,
    commit: impl FnOnce() -> AppResult<T>,
) -> AppResult<T> {
    let previous = backend.get(credential_ref)?;
    if let Err(error) = backend.delete(credential_ref) {
        return Err(compensate_error(
            error,
            restore(backend, credential_ref, previous.as_deref()),
            "凭据删除失败",
        ));
    }
    match commit() {
        Ok(value) => Ok(value),
        Err(error) => Err(compensate_error(
            error,
            restore(backend, credential_ref, previous.as_deref()),
            "数据库删除失败",
        )),
    }
}

fn restore<B: CredentialBackend>(
    backend: &B,
    connection_id: &str,
    previous: Option<&str>,
) -> AppResult<()> {
    match previous {
        Some(secret) => backend.save(connection_id, secret),
        None => backend.delete(connection_id),
    }
}

fn compensate_error(
    operation_error: AppError,
    compensation: AppResult<()>,
    operation: &str,
) -> AppError {
    match compensation {
        Ok(()) => operation_error,
        Err(rollback_error) => AppError::internal(format!(
            "{operation}且凭据补偿失败（原错误：{operation_error}；补偿错误：{rollback_error}）"
        )),
    }
}

fn compensate_many_error<B: CredentialBackend>(
    operation_error: AppError,
    backend: &B,
    snapshots: &[(String, Option<String>)],
    attempted: &[String],
    operation: &str,
) -> AppError {
    let mut failures = Vec::new();
    for id in attempted.iter().rev() {
        let previous = snapshots
            .iter()
            .find(|(snapshot_id, _)| snapshot_id == id)
            .and_then(|(_, secret)| secret.as_deref());
        if let Err(error) = restore(backend, id, previous) {
            failures.push(error.to_string());
        }
    }
    if failures.is_empty() {
        operation_error
    } else {
        AppError::internal(format!(
            "{operation}且凭据补偿失败（原错误：{operation_error}；补偿错误数：{}）",
            failures.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{ai_connections, init_memory};
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::mpsc::{self, Receiver, Sender};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Default)]
    struct MemoryBackend {
        entries: Mutex<HashMap<String, String>>,
        fail_read: AtomicBool,
        fail_save: AtomicBool,
        fail_delete: AtomicBool,
        read_entered: Mutex<Option<Sender<()>>>,
        read_resume: Mutex<Option<Receiver<()>>>,
    }

    impl CredentialBackend for MemoryBackend {
        fn get(&self, id: &str) -> AppResult<Option<String>> {
            if self.fail_read.load(Ordering::SeqCst) {
                return Err(AppError::msg("mock credential read failed"));
            }
            if let Some(sender) = self.read_entered.lock().unwrap().take() {
                let _ = sender.send(());
                if let Some(receiver) = self.read_resume.lock().unwrap().take() {
                    receiver
                        .recv_timeout(Duration::from_secs(3))
                        .map_err(|_| AppError::timeout("mock credential read timeout"))?;
                }
            }
            Ok(self.entries.lock().unwrap().get(id).cloned())
        }

        fn save(&self, id: &str, secret: &str) -> AppResult<()> {
            if self.fail_save.load(Ordering::SeqCst) {
                return Err(AppError::msg("mock credential save failed"));
            }
            self.entries
                .lock()
                .unwrap()
                .insert(id.to_string(), secret.to_string());
            Ok(())
        }

        fn delete(&self, id: &str) -> AppResult<()> {
            if self.fail_delete.load(Ordering::SeqCst) {
                return Err(AppError::msg("mock credential delete failed"));
            }
            self.entries.lock().unwrap().remove(id);
            Ok(())
        }
    }

    fn bound_test_db() -> Arc<Database> {
        let conn = init_memory().unwrap();
        ai_connections::upsert(
            &conn,
            "test-bound",
            "Test",
            "cloud",
            "openai_chat",
            "https://example.invalid/v1",
            "test-model",
            Some("test-bound"),
        )
        .unwrap();
        ai_connections::bind_usage(&conn, "tagging", "test-bound").unwrap();
        Arc::new(Database::new(conn))
    }

    fn add_test_connection(
        db: &Arc<Database>,
        id: &str,
        deployment: &str,
        api_key_ref: Option<&str>,
    ) {
        let conn = db.lock().unwrap();
        ai_connections::upsert(
            &conn,
            id,
            id,
            deployment,
            "openai_chat",
            "https://example.invalid/v1",
            "test-model",
            api_key_ref,
        )
        .unwrap();
    }

    #[test]
    fn unbound_super_search_never_falls_back_to_a_local_connection() {
        let db = Arc::new(Database::new(init_memory().unwrap()));
        add_test_connection(&db, "a-local", "local", None);
        add_test_connection(&db, "z-cloud", "cloud", Some("z-cloud"));
        let backend = MemoryBackend::default();
        backend.save("z-cloud", "sk-cloud").unwrap();

        let resolved = resolve_super_search_profile_with_credential(&db, &backend).unwrap();
        assert_eq!(resolved.connection_id, "z-cloud");
        assert_eq!(resolved.profile.api_key, "sk-cloud");
        assert_eq!(resolved.source, SuperSearchProfileSource::AutomaticOnline);
    }

    #[test]
    fn unbound_super_search_with_only_local_service_requires_online_setup() {
        let db = Arc::new(Database::new(init_memory().unwrap()));
        add_test_connection(&db, "local-only", "local", None);
        let error = resolve_super_search_profile_with_credential(&db, &MemoryBackend::default())
            .expect_err("未绑定时不能自动使用本地服务");
        assert_eq!(error.code(), "NOT_FOUND");
        assert!(error.to_string().contains("在线 AI 服务"));
    }

    #[test]
    fn explicit_local_binding_is_respected_and_disabled_binding_fails_closed() {
        let db = Arc::new(Database::new(init_memory().unwrap()));
        add_test_connection(&db, "cloud", "cloud", None);
        add_test_connection(&db, "local", "local", None);
        {
            let conn = db.lock().unwrap();
            ai_connections::bind_usage(&conn, "super_search", "local").unwrap();
        }
        let backend = MemoryBackend::default();
        let resolved = resolve_super_search_profile_with_credential(&db, &backend).unwrap();
        assert_eq!(resolved.connection_id, "local");
        assert_eq!(resolved.source, SuperSearchProfileSource::ExplicitBinding);

        {
            let conn = db.lock().unwrap();
            conn.execute("UPDATE ai_connections SET enabled=0 WHERE id='local'", [])
                .unwrap();
        }
        let error = resolve_super_search_profile_with_credential(&db, &backend)
            .expect_err("显式绑定失效时不能切换到其他服务");
        assert_eq!(error.code(), "CONFLICT");
    }

    #[test]
    fn explicit_binding_with_missing_key_does_not_fall_back_to_cloud() {
        let db = Arc::new(Database::new(init_memory().unwrap()));
        add_test_connection(&db, "bound", "cloud", Some("bound"));
        add_test_connection(&db, "other", "cloud", Some("other"));
        {
            let conn = db.lock().unwrap();
            ai_connections::bind_usage(&conn, "super_search", "bound").unwrap();
        }
        let backend = MemoryBackend::default();
        backend.save("other", "sk-other").unwrap();

        let error = resolve_super_search_profile_with_credential(&db, &backend)
            .expect_err("密钥缺失时不能改用另一条连接");
        assert_eq!(error.code(), "NOT_FOUND");
        assert!(error.to_string().contains("密钥不可用"));
    }

    #[test]
    fn automatic_online_resolution_skips_connections_with_missing_key_refs() {
        let db = Arc::new(Database::new(init_memory().unwrap()));
        add_test_connection(&db, "a-missing", "cloud", Some("a-missing"));
        add_test_connection(&db, "z-configured", "cloud", Some("z-configured"));
        let backend = MemoryBackend::default();
        backend.save("z-configured", "sk-configured").unwrap();

        let resolved = resolve_super_search_profile_with_credential(&db, &backend).unwrap();
        assert_eq!(resolved.connection_id, "z-configured");
        assert_eq!(resolved.profile.api_key, "sk-configured");
    }

    // These tests use the real OS credential store rather than an in-memory mock. Serialize them
    // because the backend's shared probe entry is process-external state, not an isolated fixture.
    static KEYRING_TEST_LOCK: Mutex<()> = Mutex::new(());

    /// 凭据测试策略：优先真实后端（本机 Windows Credential Manager 可用时做完整 roundtrip）；
    /// 后端不可用（CI/无凭据服务）时验证错误被正确上抛、不静默吞。
    /// keyring 的 mock 库把密码存在 entry 实例内、跨 Entry 不持久，不适合测 wrapper 形态，
    /// 因此这里用真实后端 + 可用性探测，行为与本机一致。
    fn backend_usable() -> bool {
        let probe = "test-conn-probe";
        match save_api_key(probe, "probe") {
            Ok(()) => {
                let _ = delete_api_key(probe);
                true
            }
            Err(_) => false,
        }
    }

    #[test]
    fn save_get_delete_roundtrip() {
        let _serial = KEYRING_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !backend_usable() {
            // 无凭据服务的环境：此路径无法验证；确保不误报
            return;
        }
        let id = "test-conn-roundtrip";
        let _ = delete_api_key(id);
        save_api_key(id, "sk-secret-123").unwrap();
        assert_eq!(get_api_key(id).unwrap().as_deref(), Some("sk-secret-123"));
        delete_api_key(id).unwrap();
        assert_eq!(get_api_key(id).unwrap(), None);
    }

    #[test]
    fn get_missing_returns_none() {
        let _serial = KEYRING_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !backend_usable() {
            return;
        }
        let id = "test-conn-missing";
        let _ = delete_api_key(id);
        assert_eq!(get_api_key(id).unwrap(), None);
    }

    #[test]
    fn overwrite_changes_value() {
        let _serial = KEYRING_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !backend_usable() {
            return;
        }
        let id = "test-conn-overwrite";
        let _ = delete_api_key(id);
        save_api_key(id, "old").unwrap();
        save_api_key(id, "new").unwrap();
        assert_eq!(get_api_key(id).unwrap().as_deref(), Some("new"));
        delete_api_key(id).unwrap();
    }

    #[test]
    fn credential_status_semantics() {
        // 三态标签稳定
        assert_eq!(CredentialStatus::Configured.tag(), "configured");
        assert_eq!(CredentialStatus::Missing.tag(), "missing");
        assert_eq!(
            CredentialStatus::Unavailable("x".into()).tag(),
            "unavailable"
        );
        // has_key 仅 Configured 为 true（missing/unavailable 都不算已配置）
        assert!(CredentialStatus::Configured.has_key());
        assert!(!CredentialStatus::Missing.has_key());
        assert!(!CredentialStatus::Unavailable("x".into()).has_key());
        // message 仅 unavailable 携带（不泄露密钥）
        assert!(CredentialStatus::Configured.message().is_none());
        assert!(CredentialStatus::Missing.message().is_none());
        assert_eq!(
            CredentialStatus::Unavailable("服务不可用".into()).message(),
            Some("服务不可用")
        );
    }

    #[test]
    fn probe_status_missing_vs_configured_on_real_backend() {
        let _serial = KEYRING_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if !backend_usable() {
            // 无凭据服务：probe 应报告 Unavailable（区别于 Missing），不 panic
            let st = probe_status("test-conn-probe-status-unavail");
            assert!(matches!(st, CredentialStatus::Unavailable(_)));
            return;
        }
        let id = "test-conn-probe-status";
        let _ = delete_api_key(id);
        // 未配置 → Missing
        assert_eq!(probe_status(id), CredentialStatus::Missing);
        // 配置后 → Configured
        save_api_key(id, "sk-xyz").unwrap();
        assert_eq!(probe_status(id), CredentialStatus::Configured);
        delete_api_key(id).unwrap();
        assert_eq!(probe_status(id), CredentialStatus::Missing);
    }

    /// 错误必须上抛为 AppError（不静默吞）：无凭据服务环境 save 返回 Err 属预期，但不 panic。
    #[test]
    fn backend_unavailable_errors_are_surfaced_not_silent() {
        let _serial = KEYRING_TEST_LOCK
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        if backend_usable() {
            // 本机后端可用：空 user 等路径不得 panic（参数错误由上层校验，此处只验证不 panic）
            let _ = get_api_key("");
            let _ = save_api_key("", "x");
        } else {
            // 后端不可用：save 必须 Err 而非 Ok
            let r = save_api_key("probe-err", "x");
            assert!(r.is_err(), "凭据后端不可用时保存应返回错误（不静默吞）");
        }
    }

    #[test]
    fn missing_credential_is_an_empty_key_but_read_failure_propagates() {
        let db = bound_test_db();
        let backend = MemoryBackend::default();
        let (row, missing) = connection_with_credential(&db, "test-bound", &backend).unwrap();
        assert_eq!(row.id, "test-bound");
        assert_eq!(missing, None);

        backend.fail_read.store(true, Ordering::SeqCst);
        assert!(connection_with_credential(&db, "test-bound", &backend).is_err());
    }

    #[test]
    fn delayed_credential_read_does_not_hold_database_lock() {
        let db = bound_test_db();
        let backend = Arc::new(MemoryBackend::default());
        backend
            .entries
            .lock()
            .unwrap()
            .insert("test-bound".into(), "secret".into());
        let (entered_tx, entered_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        *backend.read_entered.lock().unwrap() = Some(entered_tx);
        *backend.read_resume.lock().unwrap() = Some(resume_rx);

        let db_for_reader = Arc::clone(&db);
        let backend_for_reader = Arc::clone(&backend);
        let reader = std::thread::spawn(move || {
            connection_with_credential(&db_for_reader, "test-bound", backend_for_reader.as_ref())
        });
        entered_rx
            .recv_timeout(Duration::from_secs(3))
            .expect("backend should reach delayed keyring read");

        // This would block until the delayed backend read completes if the helper
        // accidentally kept Database::lock alive across the credential call.
        let conn = db
            .lock()
            .expect("ordinary DB access must remain available during keyring IO");
        let exists: i64 = conn
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM ai_connections WHERE id='test-bound')",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(exists, 1);
        drop(conn);
        resume_tx.send(()).unwrap();
        let (_, secret) = reader.join().unwrap().unwrap();
        assert_eq!(secret.as_deref(), Some("secret"));
    }

    #[test]
    fn failed_delete_keeps_db_commit_from_running_and_preserves_secret() {
        let backend = MemoryBackend::default();
        backend.save("c1", "old").unwrap();
        backend.fail_delete.store(true, Ordering::SeqCst);
        let db = bound_test_db();
        let _operations = lock_operations().unwrap();
        let result = delete_with_commit(&backend, "c1", || {
            let conn = db.lock()?;
            ai_connections::delete(&conn, "test-bound")
        });
        assert!(result.is_err());
        assert_eq!(backend.get("c1").unwrap().as_deref(), Some("old"));
        let conn = db.lock().unwrap();
        assert!(ai_connections::get(&conn, "test-bound").unwrap().is_some());
        assert_eq!(
            ai_connections::binding_id(&conn, "tagging")
                .unwrap()
                .as_deref(),
            Some("test-bound")
        );
    }

    #[test]
    fn database_save_failure_compensates_to_previous_secret() {
        let backend = MemoryBackend::default();
        backend.save("c1", "old").unwrap();
        let _operations = lock_operations().unwrap();
        let result: AppResult<()> = replace_with_commit(&backend, "c1", "new", || {
            Err(AppError::msg("injected DB failure"))
        });
        assert!(result.is_err());
        assert_eq!(backend.get("c1").unwrap().as_deref(), Some("old"));
    }

    #[test]
    fn credential_write_failure_preserves_previous_secret() {
        let backend = MemoryBackend::default();
        backend.save("c1", "old").unwrap();
        backend.fail_save.store(true, Ordering::SeqCst);
        let _operations = lock_operations().unwrap();
        let result: AppResult<()> = replace_with_commit(&backend, "c1", "new", || Ok(()));
        assert!(result.is_err());
        assert_eq!(backend.get("c1").unwrap().as_deref(), Some("old"));
    }

    #[test]
    fn deleting_a_missing_secret_is_idempotent() {
        let backend = MemoryBackend::default();
        let _operations = lock_operations().unwrap();
        let result = delete_with_commit(&backend, "missing", || Ok(42));
        assert_eq!(result.unwrap(), 42);
    }

    #[test]
    fn reset_db_failure_restores_every_deleted_credential() {
        let backend = MemoryBackend::default();
        backend.save("c1", "one").unwrap();
        backend.save("c2", "two").unwrap();
        let _operations = lock_operations().unwrap();
        let result: AppResult<()> =
            delete_many_with_commit(&backend, &["c1".into(), "c2".into()], || {
                Err(AppError::msg("injected reset failure"))
            });
        assert!(result.is_err());
        assert_eq!(backend.get("c1").unwrap().as_deref(), Some("one"));
        assert_eq!(backend.get("c2").unwrap().as_deref(), Some("two"));
    }

    #[test]
    fn serialized_updates_keep_the_final_db_reference_and_secret_consistent() {
        let backend = Arc::new(MemoryBackend::default());
        backend.save("c1", "old").unwrap();
        let db = bound_test_db();
        let (first_committing_tx, first_committing_rx) = mpsc::channel();
        let (finish_first_tx, finish_first_rx) = mpsc::channel();
        let first_backend = Arc::clone(&backend);
        let first_db = Arc::clone(&db);
        let first = std::thread::spawn(move || {
            let _operations = lock_operations().unwrap();
            replace_with_commit(first_backend.as_ref(), "c1", "first", || {
                first_committing_tx.send(()).unwrap();
                finish_first_rx.recv().unwrap();
                let conn = first_db.lock()?;
                ai_connections::upsert(
                    &conn,
                    "test-bound",
                    "Test",
                    "cloud",
                    "openai_chat",
                    "https://example.invalid/v1",
                    "test-model",
                    Some("c1"),
                )
            })
            .unwrap();
        });
        first_committing_rx.recv().unwrap();

        let second_backend = Arc::clone(&backend);
        let second_db = Arc::clone(&db);
        let second = std::thread::spawn(move || {
            let _operations = lock_operations().unwrap();
            replace_with_commit(second_backend.as_ref(), "c1", "second", || {
                let conn = second_db.lock()?;
                ai_connections::upsert(
                    &conn,
                    "test-bound",
                    "Test",
                    "cloud",
                    "openai_chat",
                    "https://example.invalid/v1",
                    "test-model",
                    Some("c1"),
                )
            })
            .unwrap();
        });
        finish_first_tx.send(()).unwrap();
        first.join().unwrap();
        second.join().unwrap();
        assert_eq!(backend.get("c1").unwrap().as_deref(), Some("second"));
        let conn = db.lock().unwrap();
        assert_eq!(
            ai_connections::get(&conn, "test-bound")
                .unwrap()
                .unwrap()
                .api_key_ref
                .as_deref(),
            Some("c1")
        );
    }
}
