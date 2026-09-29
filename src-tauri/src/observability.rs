//! 统一可观测性边界：日志初始化、panic 兜底、前端日志回传和运行时级别控制。
//!
//! 业务模块只负责使用 `tracing` 记录结构化事实；这里集中处理输出策略和安全边界，
//! 避免每个 command 各自拼接文件、过滤器或脱敏逻辑。

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::reload;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::{fmt, EnvFilter, Registry};

use crate::error::{AppError, AppResult};

pub type LogReloadHandle = reload::Handle<EnvFilter, Registry>;

static LOG_RELOAD_HANDLE: OnceLock<LogReloadHandle> = OnceLock::new();
static PANIC_HOOK_INSTALLED: OnceLock<()> = OnceLock::new();
static PANIC_FALLBACK_DIR: OnceLock<PathBuf> = OnceLock::new();

const DEFAULT_FILTER: &str = "info";
const MAX_FRONTEND_MESSAGE_CHARS: usize = 4_000;
const MAX_FRONTEND_CONTEXT_CHARS: usize = 4_000;
const MAX_LOG_TOTAL_BYTES: u64 = 50 * 1024 * 1024;
const MAX_LOG_AGE_DAYS: i64 = 30;

/// 日志生命周期守卫。`worker` 必须在应用存活期间保留，确保非阻塞 writer 不被提前回收。
#[derive(Debug)]
pub struct LoggingGuard {
    _worker: Option<WorkerGuard>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LogRetentionReport {
    pub removed_files: usize,
    pub removed_bytes: u64,
    pub kept_files: usize,
}

/// 初始化日志并把 reload handle 存入进程级槽位。
///
/// 文件 writer 创建失败时仍保留 stdout 日志，且安装 panic hook。已有全局 subscriber
/// 时（测试或嵌入式宿主）不重复初始化，也不覆盖宿主日志。
pub fn init_logging(data_dir: &Path) -> LoggingGuard {
    let logs_dir = data_dir.join("logs");
    let _ = PANIC_FALLBACK_DIR.set(logs_dir.clone());
    let env_filter = default_env_filter();
    let (filter_layer, reload_handle) = reload::Layer::new(env_filter);

    let file_appender = tracing_appender::rolling::Builder::new()
        .max_log_files(30)
        .filename_prefix("app.log")
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .build(&logs_dir);

    match file_appender {
        Ok(appender) => {
            let (file_writer, guard) = tracing_appender::non_blocking(appender);
            let result = tracing_subscriber::registry()
                .with(filter_layer)
                .with(fmt::layer().with_ansi(false))
                .with(fmt::layer().with_writer(file_writer).with_ansi(false))
                .try_init();
            if result.is_ok() {
                if LOG_RELOAD_HANDLE.set(reload_handle).is_err() {
                    // 另一个测试/宿主抢先完成初始化时，保留已有 subscriber，不制造双日志管道。
                }
            } else {
                eprintln!("tracing subscriber 已初始化，文件日志未接入");
            }
            install_panic_hook();
            LoggingGuard {
                _worker: Some(guard),
            }
        }
        Err(e) => {
            let result = tracing_subscriber::registry()
                .with(filter_layer)
                .with(fmt::layer().with_ansi(false))
                .try_init();
            if result.is_ok() && LOG_RELOAD_HANDLE.set(reload_handle).is_err() {
                // 同进程已有其它 subscriber 时，保留宿主配置。
            }
            eprintln!("日志文件初始化失败（降级为 stdout）: {e}");
            install_panic_hook();
            LoggingGuard { _worker: None }
        }
    }
}

fn default_env_filter() -> EnvFilter {
    EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER))
}

/// 规范并校验用户可设置的日志级别。
pub fn normalize_log_level(level: &str) -> AppResult<&'static str> {
    match level.trim().to_ascii_lowercase().as_str() {
        "info" => Ok("info"),
        "debug" => Ok("debug"),
        "trace" => Ok("trace"),
        _ => Err(AppError::msg("日志级别只支持 info、debug 或 trace")),
    }
}

/// 运行时切换全局过滤级别。未初始化日志时返回明确错误，不伪造成功。
pub fn set_log_level(level: &str) -> AppResult<&'static str> {
    let normalized = normalize_log_level(level)?;
    let handle = LOG_RELOAD_HANDLE
        .get()
        .ok_or_else(|| AppError::msg("日志系统尚未初始化"))?;
    handle
        .reload(EnvFilter::new(normalized))
        .map_err(|e| AppError::msg(format!("日志级别切换失败: {e}")))?;
    Ok(normalized)
}

/// 启动后清理过期的日滚动日志，并把现存日志总量压到上限以内。
/// 始终保留最新一个文件，避免删除当前正在写入的 `app.log`。
pub fn prune_log_files(logs_dir: &Path) -> AppResult<LogRetentionReport> {
    prune_log_files_with_limits(logs_dir, MAX_LOG_TOTAL_BYTES, MAX_LOG_AGE_DAYS)
}

fn prune_log_files_with_limits(
    logs_dir: &Path,
    max_total_bytes: u64,
    max_age_days: i64,
) -> AppResult<LogRetentionReport> {
    if !logs_dir.exists() {
        return Ok(LogRetentionReport::default());
    }

    let mut files: Vec<(PathBuf, u64, Option<std::time::SystemTime>)> = Vec::new();
    for entry in std::fs::read_dir(logs_dir)
        .map_err(|e| AppError::internal(format!("读取日志目录失败: {e}")))?
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !name.starts_with("app.log") {
            continue;
        }
        let metadata = match std::fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(_) => continue,
        };
        files.push((path, metadata.len(), metadata.modified().ok()));
    }
    files.sort_by_key(|(_, _, modified)| *modified);

    let total_before: u64 = files.iter().map(|(_, bytes, _)| bytes).sum();
    let mut total = total_before;
    let newest = files.last().map(|(path, _, _)| path.clone());
    let cutoff = chrono::Utc::now() - chrono::Duration::days(max_age_days);
    let mut removed_files = 0usize;
    let mut removed_bytes = 0u64;

    for (path, bytes, modified) in &files {
        if newest.as_ref() == Some(path) {
            continue;
        }
        let expired = modified
            .map(chrono::DateTime::<chrono::Utc>::from)
            .is_some_and(|time| time < cutoff);
        if !expired && total <= max_total_bytes {
            break;
        }
        if std::fs::remove_file(path).is_ok() {
            removed_files += 1;
            removed_bytes = removed_bytes.saturating_add(*bytes);
            total = total.saturating_sub(*bytes);
        }
    }

    Ok(LogRetentionReport {
        removed_files,
        removed_bytes,
        kept_files: files.len().saturating_sub(removed_files),
    })
}

/// 给启动致命错误提供一个不依赖异步日志队列的同步落盘证据。
pub fn write_sync_diagnostic(logs_dir: &Path, message: &str) {
    let _ = std::fs::create_dir_all(logs_dir);
    let path = logs_dir.join("fatal.log");
    let line = format!(
        "{} {}\n",
        chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        message.replace('\n', "\\n")
    );
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        use std::io::Write;
        let _ = file.write_all(line.as_bytes());
        let _ = file.flush();
    }
}

/// 安装全局 panic hook。hook 先写 tracing，再同步写一份 panic 文件；最后调用旧 hook
/// 保留开发期 stderr/backtrace 行为。
pub fn install_panic_hook() {
    if PANIC_HOOK_INSTALLED.set(()).is_err() {
        return;
    }
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let thread = std::thread::current();
        let thread_name = thread.name().unwrap_or("<unnamed>");
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        let payload = panic_payload(info.payload());
        let summary = format!(
            "panic thread={thread_name} location={location} message={}",
            redact_secrets(&payload)
        );
        tracing::error!(
            source = "panic",
            thread = thread_name,
            location = %location,
            "{}",
            summary
        );
        if let Some(dir) = PANIC_FALLBACK_DIR.get() {
            write_sync_diagnostic(dir, &summary);
        }
        previous(info);
    }));
}

fn panic_payload(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "<non-string panic payload>".to_string()
    }
}

/// 前端日志入口。命令本身不返回错误，避免日志通道反过来制造业务失败。
pub fn log_frontend_message(level: &str, message: &str, context: Option<&str>) {
    let level = match level.trim().to_ascii_lowercase().as_str() {
        "error" => LogLevel::Error,
        "warn" | "warning" => LogLevel::Warn,
        "debug" => LogLevel::Debug,
        "trace" => LogLevel::Trace,
        _ => LogLevel::Info,
    };
    let message = redact_secrets(message);
    let message = truncate_chars(&message, MAX_FRONTEND_MESSAGE_CHARS);
    let context = context
        .map(redact_secrets)
        .map(|s| truncate_chars(&s, MAX_FRONTEND_CONTEXT_CHARS))
        .unwrap_or_default();
    match level {
        LogLevel::Error => tracing::error!(source = "frontend", context = %context, "{}", message),
        LogLevel::Warn => tracing::warn!(source = "frontend", context = %context, "{}", message),
        LogLevel::Debug => tracing::debug!(source = "frontend", context = %context, "{}", message),
        LogLevel::Trace => tracing::trace!(source = "frontend", context = %context, "{}", message),
        LogLevel::Info => tracing::info!(source = "frontend", context = %context, "{}", message),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

/// 最低限度的日志脱敏。这里只处理高置信度的凭据形态，业务日志仍禁止直接记录完整请求体。
pub fn redact_secrets(input: &str) -> String {
    // 先处理带 scheme 的 Authorization，避免 key=value 规则只截掉 "Bearer"/"Basic"
    // 而把后续令牌留在日志里。
    let mut out = redact_scheme_secret(input, "bearer ");
    out = redact_scheme_secret(&out, "basic ");
    for marker in [
        "authorization:",
        "\"authorization\":",
        "'authorization':",
        "authorization=",
        "api_key:",
        "api_key=",
        "\"api_key\":",
        "'api_key':",
        "api-key:",
        "api-key=",
        "\"api-key\":",
        "'api-key':",
        "apikey:",
        "apikey=",
        "\"apikey\":",
        "'apikey':",
        "token:",
        "token=",
        "\"token\":",
        "'token':",
        "access_token:",
        "access_token=",
        "\"access_token\":",
        "'access_token':",
        "refresh_token:",
        "refresh_token=",
        "\"refresh_token\":",
        "'refresh_token':",
        "password:",
        "password=",
        "\"password\":",
        "'password':",
    ] {
        out = redact_after_marker(&out, marker);
    }
    out
}

fn redact_after_marker(input: &str, marker: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find(marker) {
        let start = cursor + relative;
        let value_start = start + marker.len();
        out.push_str(&input[cursor..value_start]);
        let mut value_begin = value_start;
        while input[value_begin..]
            .chars()
            .next()
            .is_some_and(char::is_whitespace)
        {
            value_begin += input[value_begin..].chars().next().unwrap().len_utf8();
        }
        out.push_str(&input[value_start..value_begin]);
        let quote = input[value_begin..]
            .chars()
            .next()
            .filter(|c| *c == '"' || *c == '\'');
        let content_start = quote
            .map(|c| value_begin + c.len_utf8())
            .unwrap_or(value_begin);
        if let Some(quote) = quote {
            out.push(quote);
        }
        let end = if let Some(quote) = quote {
            input[content_start..]
                .find(quote)
                .map(|i| content_start + i + quote.len_utf8())
                .unwrap_or(input.len())
        } else {
            input[content_start..]
                .find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '&' | '}' | ']'))
                .map(|i| content_start + i)
                .unwrap_or(input.len())
        };
        out.push_str("<redacted>");
        if quote.is_some() && end < input.len() {
            out.push_str(&input[end - 1..end]);
        }
        cursor = end;
    }
    out.push_str(&input[cursor..]);
    out
}

fn redact_scheme_secret(input: &str, scheme: &str) -> String {
    let lower = input.to_ascii_lowercase();
    let mut out = String::with_capacity(input.len());
    let mut cursor = 0;
    while let Some(relative) = lower[cursor..].find(scheme) {
        let start = cursor + relative;
        let value_start = start + scheme.len();
        out.push_str(&input[cursor..value_start]);
        let end = input[value_start..]
            .find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | '&' | '}' | ']'))
            .map(|i| value_start + i)
            .unwrap_or(input.len());
        out.push_str("<redacted>");
        cursor = end;
    }
    out.push_str(&input[cursor..]);
    out
}

fn truncate_chars(input: &str, max: usize) -> String {
    let mut chars = input.chars();
    let mut out: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        out.push('…');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_log_level_accepts_supported_values() {
        assert_eq!(normalize_log_level("INFO").unwrap(), "info");
        assert_eq!(normalize_log_level(" debug ").unwrap(), "debug");
        assert_eq!(normalize_log_level("trace").unwrap(), "trace");
        assert!(normalize_log_level("warn").is_err());
    }

    #[test]
    fn redact_secrets_masks_common_key_value_shapes() {
        let input = "api_key=sk-secret token: abc123 Authorization: Bearer xyz";
        let redacted = redact_secrets(input);
        assert!(!redacted.contains("sk-secret"));
        assert!(!redacted.contains("abc123"));
        assert!(!redacted.contains("xyz"));
        assert!(redacted.contains("<redacted>"));
    }

    #[test]
    fn redact_secrets_masks_json_keys_and_basic_auth() {
        let input = r#"{"api_key": "sk-json-secret", "Authorization": "Basic dXNlcjpwYXNz", "token": "abc123"}"#;
        let redacted = redact_secrets(input);
        assert!(!redacted.contains("sk-json-secret"));
        assert!(!redacted.contains("dXNlcjpwYXNz"));
        assert!(!redacted.contains("abc123"));
        assert!(redacted.contains(r#""api_key": "<redacted>""#));
    }

    #[test]
    fn truncate_chars_preserves_utf8_boundary() {
        assert_eq!(truncate_chars("茶馆素材", 2), "茶馆…");
        assert_eq!(truncate_chars("茶包", 2), "茶包");
    }

    #[test]
    fn log_pruning_removes_oldest_first_and_keeps_active_file() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("app.log.2026-08-01");
        let second = dir.path().join("app.log.2026-08-02");
        let active = dir.path().join("app.log.2026-09-16");
        std::fs::write(&first, vec![b'a'; 8]).unwrap();
        std::fs::write(&second, vec![b'b'; 8]).unwrap();
        std::fs::write(&active, vec![b'c'; 8]).unwrap();

        let report = prune_log_files_with_limits(dir.path(), 8, 30).unwrap();
        assert_eq!(report.removed_files, 2);
        assert_eq!(report.removed_bytes, 16);
        assert!(!first.exists());
        assert!(!second.exists());
        assert!(active.exists());
        assert_eq!(report.kept_files, 1);
    }
}
