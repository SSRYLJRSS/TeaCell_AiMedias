//! 统一错误模型：所有 command 返回 Result<T, AppError>，序列化为
//! `{ code, message, cause? }`。`code` 供前端做机器判别，`cause` 保留 thiserror
//! 暴露的底层错误链，不把 rusqlite/IO 的具体原因丢在序列化边界。

use crate::contracts::agent::AgentErrorCode;
use serde::Serialize;
use std::error::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppErrorCode {
    InvalidArg,
    NotFound,
    Conflict,
    Cancelled,
    Timeout,
    FileLocked,
    AiRateLimited,
    AiFormatUnsupported,
    AiOutputTruncated,
    BudgetExceeded,
    LibraryRootRequired,
    InvalidLibraryRoot,
    Unauthorized,
    Unsupported,
    Internal,
    /// Agent 网络出口未就绪（未绑定可用服务/未通过外发前置检查）。历史码名沿用
    /// A3-02；已无用户可见的「联网开关」（2026-11-01 裁决）。
    AgentModelNetworkDisabled,
    /// 用户还没有为 Agent 绑定 AI 服务，或绑定的服务已删除/停用。
    AgentServiceUnbound,
    /// 绑定的云服务还没有保存密钥（本机服务不需要密钥）。
    AgentCredentialMissing,
    /// Agent Host 上次是被强行中断的，需要用户明确选择恢复。
    AgentHostInterrupted,
    /// 另一个启动流程正在进行中，稍后重试即可（不是错误操作）。
    AgentHostStarting,
    /// Agent Host 不可用（平台不支持、资源缺失、启动失败已耗尽重试）。
    AgentHostUnavailable,
    /// Host 重启后旧会话需要重新打开。
    AgentConversationNotOpen,
    /// Host 上已不存在该会话（会话文件被清理/总仓重建）。
    AgentSessionNotFound,
}

impl AppErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArg => "INVALID_ARG",
            Self::NotFound => "NOT_FOUND",
            Self::Conflict => "CONFLICT",
            Self::Cancelled => "CANCELLED",
            Self::Timeout => "TIMEOUT",
            Self::FileLocked => "FILE_LOCKED",
            Self::AiRateLimited => "AI_RATE_LIMITED",
            Self::AiFormatUnsupported => "AI_FORMAT_UNSUPPORTED",
            Self::AiOutputTruncated => "AI_OUTPUT_TRUNCATED",
            Self::BudgetExceeded => "BUDGET_EXCEEDED",
            Self::LibraryRootRequired => "LIBRARY_ROOT_REQUIRED",
            Self::InvalidLibraryRoot => "INVALID_LIBRARY_ROOT",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Unsupported => "UNSUPPORTED",
            Self::Internal => "INTERNAL",
            Self::AgentModelNetworkDisabled => "AGENT_MODEL_NETWORK_DISABLED",
            Self::AgentServiceUnbound => "AGENT_SERVICE_UNBOUND",
            Self::AgentCredentialMissing => "AGENT_CREDENTIAL_MISSING",
            Self::AgentHostInterrupted => "AGENT_HOST_INTERRUPTED",
            Self::AgentHostStarting => "AGENT_HOST_STARTING",
            Self::AgentHostUnavailable => "AGENT_HOST_UNAVAILABLE",
            Self::AgentConversationNotOpen => "AGENT_CONVERSATION_NOT_OPEN",
            Self::AgentSessionNotFound => "AGENT_SESSION_NOT_FOUND",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{}", .0.public_message())]
    Agent(AgentErrorCode),
    #[error("数据库错误: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("序列化错误: {0}")]
    Json(#[from] serde_json::Error),
    #[error("图像错误: {0}")]
    Image(#[from] image::ImageError),
    #[error("{1}")]
    Coded(AppErrorCode, String),
    #[error("{0}")]
    Msg(String),
}

impl AppError {
    /// External Agent boundaries expose classified fixed messages, never raw causes.
    /// Operation recovery must classify uncertain effects explicitly before this conversion.
    pub fn into_agent_error(self) -> Self {
        let code = match self {
            Self::Agent(code) => code,
            Self::Coded(code, _) => match code {
                AppErrorCode::InvalidArg => AgentErrorCode::InvalidInput,
                AppErrorCode::Unauthorized => AgentErrorCode::Unauthorized,
                AppErrorCode::Conflict | AppErrorCode::FileLocked => {
                    AgentErrorCode::ResourceConflict
                }
                AppErrorCode::Cancelled => AgentErrorCode::Cancelled,
                AppErrorCode::Timeout | AppErrorCode::AiRateLimited => {
                    AgentErrorCode::ServiceUnavailable
                }
                AppErrorCode::BudgetExceeded => AgentErrorCode::BudgetExceeded,
                AppErrorCode::LibraryRootRequired | AppErrorCode::InvalidLibraryRoot => {
                    AgentErrorCode::LibraryOffline
                }
                AppErrorCode::Unsupported => AgentErrorCode::UnsupportedCapability,
                // Agent 自身的门禁错误：对外只表达“服务暂时不可用/需要重新选择”，
                // 不把内部原因（开关、绑定、Host 生命周期）泄漏给 Agent 协议。
                AppErrorCode::AgentHostStarting
                | AppErrorCode::AgentHostUnavailable
                | AppErrorCode::AgentModelNetworkDisabled
                | AppErrorCode::AgentServiceUnbound
                | AppErrorCode::AgentCredentialMissing
                | AppErrorCode::AgentHostInterrupted => AgentErrorCode::ServiceUnavailable,
                // NotFound can describe more than an expired reference; do not guess.
                AppErrorCode::NotFound
                | AppErrorCode::Internal
                | AppErrorCode::AiFormatUnsupported
                | AppErrorCode::AiOutputTruncated
                | AppErrorCode::AgentConversationNotOpen
                | AppErrorCode::AgentSessionNotFound => AgentErrorCode::Internal,
            },
            Self::Db(_) | Self::Io(_) | Self::Json(_) | Self::Image(_) | Self::Msg(_) => {
                AgentErrorCode::Internal
            }
        };
        Self::Agent(code)
    }

    pub fn msg(s: impl Into<String>) -> Self {
        Self::Msg(s.into())
    }

    pub fn coded(code: AppErrorCode, s: impl Into<String>) -> Self {
        Self::Coded(code, s.into())
    }

    pub fn invalid_arg(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::InvalidArg, s)
    }

    pub fn not_found(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::NotFound, s)
    }

    pub fn conflict(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::Conflict, s)
    }

    pub fn cancelled(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::Cancelled, s)
    }

    pub fn timeout(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::Timeout, s)
    }

    pub fn file_locked(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::FileLocked, s)
    }

    pub fn ai_rate_limited(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::AiRateLimited, s)
    }

    pub fn ai_format_unsupported(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::AiFormatUnsupported, s)
    }

    pub fn ai_output_truncated(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::AiOutputTruncated, s)
    }

    pub fn budget_exceeded(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::BudgetExceeded, s)
    }

    pub fn unauthorized(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::Unauthorized, s)
    }

    pub fn unsupported(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::Unsupported, s)
    }

    pub fn internal(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::Internal, s)
    }

    /// Agent 网络出口未就绪（未绑定/未启用可用服务）。历史码名沿用 A3-02；
    /// 已无用户可见的「联网开关」（2026-11-01 裁决）。
    pub fn agent_model_network_disabled(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::AgentModelNetworkDisabled, s)
    }

    /// 没有为 Agent 绑定可用的 AI 服务（未绑定 / 已删除 / 已停用）。
    pub fn agent_service_unbound(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::AgentServiceUnbound, s)
    }

    /// 云服务缺少本机密钥（本机服务无需密钥）。
    pub fn agent_credential_missing(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::AgentCredentialMissing, s)
    }

    /// Host 上次被强行中断，需要用户明确选择恢复。
    pub fn agent_host_interrupted(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::AgentHostInterrupted, s)
    }

    /// 另一个启动流程正在进行；这是可重试的瞬时状态。
    pub fn agent_host_starting(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::AgentHostStarting, s)
    }

    /// Host 在本机不可用（平台不支持、资源缺失或启动失败）。
    pub fn agent_host_unavailable(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::AgentHostUnavailable, s)
    }

    /// 旧会话没有在当前的 Host 实例里打开过，需要重新打开后再发送。
    pub fn agent_conversation_not_open(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::AgentConversationNotOpen, s)
    }

    /// Host 上已不存在该会话，应新建会话而不是重试旧会话。
    pub fn agent_session_not_found(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::AgentSessionNotFound, s)
    }

    /// 机器可判错误码。日志与序列化共用同一定义，避免两套映射漂移。
    pub fn code(&self) -> &'static str {
        match self {
            Self::Agent(code) => code.as_str(),
            Self::Db(_) => "DB",
            Self::Io(_) => "IO",
            Self::Json(_) => "JSON",
            Self::Image(_) => "IMAGE",
            Self::Coded(code, _) => code.as_str(),
            Self::Msg(_) => "ERROR",
        }
    }
}

impl Serialize for AppError {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct ErrBody<'a> {
            code: &'a str,
            message: String,
            cause: Option<String>,
        }
        ErrBody {
            code: self.code(),
            message: self.to_string(),
            cause: self.source().map(ToString::to_string),
        }
        .serialize(s)
    }
}

pub type AppResult<T> = Result<T, AppError>;

impl From<AgentErrorCode> for AppError {
    fn from(code: AgentErrorCode) -> Self {
        Self::Agent(code)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_codes_use_the_shared_error_envelope_without_sensitive_causes() {
        let catalog: serde_json::Value =
            serde_json::from_str(include_str!("../../docs/contracts/agent-v1.json")).unwrap();
        for raw_code in catalog["enums"]["AgentErrorCode"].as_array().unwrap() {
            let code: AgentErrorCode = serde_json::from_value(raw_code.clone()).unwrap();
            let error = AppError::from(code);
            let body = serde_json::to_value(&error).unwrap();
            assert_eq!(body["code"], *raw_code);
            assert_eq!(body["message"], catalog["errorMessages"][code.as_str()]);
            assert!(body["cause"].is_null());
            assert!(error.source().is_none());
        }
    }

    #[test]
    fn external_agent_conversion_drops_raw_paths_keys_and_unclassified_messages() {
        let secret = "C:/private/photos/key-sk-secret";
        for error in [
            AppError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                secret,
            )),
            AppError::unauthorized(secret),
            AppError::msg(secret),
        ] {
            let public = error.into_agent_error();
            let body = serde_json::to_string(&public).unwrap();
            assert!(!body.contains(secret));
            assert!(public.source().is_none());
        }
        assert_eq!(
            AppError::from(AgentErrorCode::OutcomeUnknown)
                .into_agent_error()
                .code(),
            "OUTCOME_UNKNOWN"
        );
        assert_eq!(
            AppError::unauthorized(secret).into_agent_error().code(),
            "UNAUTHORIZED"
        );
    }

    #[test]
    fn serializes_code_and_source_cause() {
        let error = AppError::Io(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "file is locked",
        ));
        let body = serde_json::to_value(error).unwrap();

        assert_eq!(body["code"], "IO");
        assert_eq!(body["cause"], "file is locked");
    }

    #[test]
    fn serializes_business_error_code() {
        let error = AppError::coded(AppErrorCode::InvalidArg, "scope 不合法");
        let body = serde_json::to_value(error).unwrap();

        assert_eq!(body["code"], "INVALID_ARG");
        assert_eq!(body["message"], "scope 不合法");
        assert!(body["cause"].is_null());
    }
}
