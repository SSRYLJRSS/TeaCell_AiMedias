//! 统一错误模型：所有 command 返回 Result<T, AppError>，序列化为
//! `{ code, message, cause? }`。`code` 供前端做机器判别，`cause` 保留 thiserror
//! 暴露的底层错误链，不把 rusqlite/IO 的具体原因丢在序列化边界。

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
    LibraryRootRequired,
    InvalidLibraryRoot,
    Unauthorized,
    Unsupported,
    Internal,
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
            Self::LibraryRootRequired => "LIBRARY_ROOT_REQUIRED",
            Self::InvalidLibraryRoot => "INVALID_LIBRARY_ROOT",
            Self::Unauthorized => "UNAUTHORIZED",
            Self::Unsupported => "UNSUPPORTED",
            Self::Internal => "INTERNAL",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
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

    pub fn unauthorized(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::Unauthorized, s)
    }

    pub fn unsupported(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::Unsupported, s)
    }

    pub fn internal(s: impl Into<String>) -> Self {
        Self::coded(AppErrorCode::Internal, s)
    }

    /// 机器可判错误码。日志与序列化共用同一定义，避免两套映射漂移。
    pub fn code(&self) -> &'static str {
        match self {
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

#[cfg(test)]
mod tests {
    use super::*;

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
