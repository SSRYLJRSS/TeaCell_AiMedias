//! 凭据服务（指导书 §6.3）：API Key 存系统凭据存储（Windows Credential Manager）。
//!  - service = "bagertea_ai_media_v2"，username = connection_id；
//!  - Secret 内容只保存 API Key；JSON/SQLite 只保存 api_key_ref（= connection_id），不保存明文；
//!  - 读取设置时兼容旧 apiKey；首次成功保存时迁移到 keyring；迁移失败不得清空旧 key（必须显示错误）。

use crate::error::{AppError, AppResult};

/// 与数据库 service 名保持一致：bagertea_ai_media_v2（Cargo package name）
pub const KEYRING_SERVICE: &str = "bagertea_ai_media_v2";

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
    let entry = keyring::Entry::new(KEYRING_SERVICE, connection_id)
        .map_err(|e| AppError::msg(format!("凭据存储初始化失败: {e}")))?;
    entry
        .set_password(api_key)
        .map_err(|e| AppError::msg(format!("API 密钥保存到系统凭据失败: {e}")))
}

/// 读取 API Key。未配置返回 None；读取错误（如凭据损坏）返回错误而非静默吞。
pub fn get_api_key(connection_id: &str) -> AppResult<Option<String>> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, connection_id)
        .map_err(|e| AppError::msg(format!("凭据存储初始化失败: {e}")))?;
    match entry.get_password() {
        Ok(pw) => Ok(Some(pw)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(AppError::msg(format!("读取 API 密钥失败: {e}"))),
    }
}

/// 删除 API Key（连接档案删除时清理凭据）。未配置视为成功。
pub fn delete_api_key(connection_id: &str) -> AppResult<()> {
    let entry = keyring::Entry::new(KEYRING_SERVICE, connection_id)
        .map_err(|e| AppError::msg(format!("凭据存储初始化失败: {e}")))?;
    match entry.delete_credential() {
        Ok(()) => Ok(()),
        Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(AppError::msg(format!("删除 API 密钥失败: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        if !backend_usable() {
            return;
        }
        let id = "test-conn-missing";
        let _ = delete_api_key(id);
        assert_eq!(get_api_key(id).unwrap(), None);
    }

    #[test]
    fn overwrite_changes_value() {
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
}
