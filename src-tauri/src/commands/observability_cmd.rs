use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::State;

use crate::error::{AppError, AppResult};
use crate::observability;
use crate::state::AppState;

const MAX_DIAGNOSTIC_LOG_BYTES: u64 = 20 * 1024 * 1024;
const MAX_DIAGNOSTIC_FILE_BYTES: u64 = 4 * 1024 * 1024;

/// 前端日志回传入口。参数会做长度限制和敏感信息脱敏，失败时不向调用方抛错。
#[tauri::command]
pub fn log_frontend(level: String, message: String, context: Option<String>) {
    observability::log_frontend_message(&level, &message, context.as_deref());
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiagnosticsReport {
    pub path: String,
    pub log_files: usize,
    pub truncated_logs: usize,
    pub bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticLogLimits {
    total_bytes: u64,
    per_file_bytes: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct DiagnosticsSummary {
    generated_at: String,
    app_version: &'static str,
    os: &'static str,
    arch: &'static str,
    schema_version: i64,
    asset_count: i64,
    trashed_asset_count: i64,
    tag_count: i64,
    log_level: String,
    included_logs: Vec<String>,
    truncated_logs: Vec<String>,
    included_log_bytes: u64,
    log_limits: DiagnosticLogLimits,
    recent_ai_batches: Vec<AiBatchSummary>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AiBatchSummary {
    id: i64,
    status: String,
    mode: String,
    total: i64,
    processed: i64,
    confirmed: i64,
    created_at: i64,
}

#[derive(Debug, Clone)]
struct LogSource {
    path: PathBuf,
    offset: u64,
    length: u64,
    truncated: bool,
}

/// 导出脱敏诊断包。数据库只用于读取摘要，文件打包在锁外执行。
#[tauri::command]
pub async fn export_diagnostics(
    state: State<'_, AppState>,
    target: String,
) -> AppResult<DiagnosticsReport> {
    let target = target.trim().to_string();
    if target.is_empty() {
        return Err(AppError::msg("诊断包保存路径为空"));
    }
    let data_dir = state.data_dir.clone();
    let db = std::sync::Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || {
        export_diagnostics_blocking(&data_dir, &db, &target)
    })
    .await
    .map_err(|e| AppError::msg(format!("诊断包线程异常: {e}")))?
}

fn export_diagnostics_blocking(
    data_dir: &Path,
    db: &std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
    target: &str,
) -> AppResult<DiagnosticsReport> {
    let target_path = PathBuf::from(target);
    if let Some(parent) = target_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            return Err(AppError::msg(format!(
                "诊断包目录不存在: {}",
                parent.display()
            )));
        }
    }

    let logs_dir = data_dir.join("logs");
    let log_sources = collect_log_sources(
        &logs_dir,
        MAX_DIAGNOSTIC_LOG_BYTES,
        MAX_DIAGNOSTIC_FILE_BYTES,
    )?;
    let summary = {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        diagnostics_summary(&conn, &log_sources)?
    };

    let file = std::fs::File::create(&target_path)
        .map_err(|e| AppError::msg(format!("创建诊断包失败: {e}")))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    let summary_json = serde_json::to_vec_pretty(&summary)?;
    zip.start_file("diagnostics.json", options)
        .map_err(|e| AppError::msg(format!("写入诊断摘要失败: {e}")))?;
    zip.write_all(&summary_json)
        .map_err(|e| AppError::msg(format!("写入诊断摘要失败: {e}")))?;

    let mut included = 0usize;
    let mut truncated_logs = 0usize;
    let mut included_log_bytes = 0u64;
    for source in &log_sources {
        let Some(name) = source.path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        let mut file = match std::fs::File::open(&source.path) {
            Ok(file) => file,
            Err(_) => continue,
        };
        if file.seek(SeekFrom::Start(source.offset)).is_err() {
            continue;
        }
        zip.start_file(format!("logs/{name}"), options)
            .map_err(|e| AppError::msg(format!("写入日志 {name} 失败: {e}")))?;
        let mut limited = file.take(source.length);
        if std::io::copy(&mut limited, &mut zip).is_ok() {
            included += 1;
            included_log_bytes = included_log_bytes.saturating_add(source.length);
            if source.truncated {
                truncated_logs += 1;
            }
        }
    }
    zip.finish()
        .map_err(|e| AppError::msg(format!("完成诊断包失败: {e}")))?;
    let bytes = std::fs::metadata(&target_path)
        .map(|m| m.len())
        .unwrap_or_default();
    tracing::info!(
        operation = "export_diagnostics",
        log_files = included,
        truncated_logs,
        log_bytes = included_log_bytes,
        bytes,
        "诊断包导出完成"
    );
    Ok(DiagnosticsReport {
        path: target_path.to_string_lossy().into_owned(),
        log_files: included,
        truncated_logs,
        bytes,
    })
}

fn collect_log_sources(
    logs_dir: &Path,
    max_total_bytes: u64,
    max_per_file_bytes: u64,
) -> AppResult<Vec<LogSource>> {
    if !logs_dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in std::fs::read_dir(logs_dir)
        .map_err(|e| AppError::msg(format!("读取日志目录失败: {e}")))?
        .flatten()
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if name.starts_with("app.log") || name == "fatal.log" || name == "panic.log" {
            files.push(path);
        }
    }
    files.sort_by_key(|path| std::fs::metadata(path).and_then(|m| m.modified()).ok());
    files.reverse();

    let mut remaining = max_total_bytes;
    let mut sources = Vec::new();
    for path in files {
        if remaining == 0 {
            break;
        }
        let bytes = std::fs::metadata(&path)
            .map_err(|e| AppError::internal(format!("读取日志文件信息失败: {e}")))?
            .len();
        if bytes == 0 {
            sources.push(LogSource {
                path,
                offset: 0,
                length: 0,
                truncated: false,
            });
            continue;
        }
        let length = bytes.min(max_per_file_bytes).min(remaining);
        sources.push(LogSource {
            path,
            offset: bytes.saturating_sub(length),
            length,
            truncated: length < bytes,
        });
        remaining = remaining.saturating_sub(length);
    }
    Ok(sources)
}

fn diagnostics_summary(
    conn: &rusqlite::Connection,
    log_sources: &[LogSource],
) -> AppResult<DiagnosticsSummary> {
    let schema_version: i64 = conn.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let asset_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM assets WHERE deleted_at IS NULL",
        [],
        |row| row.get(0),
    )?;
    let trashed_asset_count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM assets WHERE deleted_at IS NOT NULL",
        [],
        |row| row.get(0),
    )?;
    let tag_count: i64 = conn.query_row("SELECT COUNT(*) FROM tags", [], |row| row.get(0))?;
    let log_level = crate::db::settings::get_settings(conn)
        .map(|s| s.log_level)
        .unwrap_or_else(|_| "info".to_string());
    let mut stmt = conn.prepare(
        "SELECT id, status, mode, total, processed, confirmed, created_at
         FROM ai_batches ORDER BY id DESC LIMIT 20",
    )?;
    let recent_ai_batches = stmt
        .query_map([], |row| {
            Ok(AiBatchSummary {
                id: row.get(0)?,
                status: row.get(1)?,
                mode: row.get(2)?,
                total: row.get(3)?,
                processed: row.get(4)?,
                confirmed: row.get(5)?,
                created_at: row.get(6)?,
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DiagnosticsSummary {
        generated_at: chrono::Utc::now().to_rfc3339(),
        app_version: env!("CARGO_PKG_VERSION"),
        os: std::env::consts::OS,
        arch: std::env::consts::ARCH,
        schema_version,
        asset_count,
        trashed_asset_count,
        tag_count,
        log_level,
        included_logs: log_sources
            .iter()
            .filter_map(|source| {
                source
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect(),
        truncated_logs: log_sources
            .iter()
            .filter(|source| source.truncated)
            .filter_map(|source| {
                source
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
            })
            .collect(),
        included_log_bytes: log_sources.iter().map(|source| source.length).sum(),
        log_limits: DiagnosticLogLimits {
            total_bytes: MAX_DIAGNOSTIC_LOG_BYTES,
            per_file_bytes: MAX_DIAGNOSTIC_FILE_BYTES,
        },
        recent_ai_batches,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::db::settings::{self, ApiProfile, Settings};

    #[test]
    fn export_diagnostics_includes_logs_and_redacted_summary_only() {
        let temp = tempfile::tempdir().unwrap();
        let data_dir = temp.path().join("data");
        let logs_dir = data_dir.join("logs");
        std::fs::create_dir_all(&logs_dir).unwrap();
        std::fs::write(logs_dir.join("app.log.2026-09-16"), "frontend error\n").unwrap();
        std::fs::write(logs_dir.join("fatal.log"), "startup failed\n").unwrap();
        std::fs::write(logs_dir.join("ignored.txt"), "not part of diagnostics\n").unwrap();

        let conn = crate::db::init_memory().unwrap();
        let mut saved = Settings {
            log_level: "debug".into(),
            ..Settings::default()
        };
        saved.ai.profiles.push(ApiProfile {
            id: "mock".into(),
            name: "mock".into(),
            api_mode: "openai".into(),
            kind: "cloud".into(),
            base_url: "https://example.invalid/v1".into(),
            api_key: "sk-diagnostics-must-not-leak".into(),
            model: "mock-model".into(),
            max_concurrency: 0,
            requests_per_minute: 0,
            requests_per_hour: 0,
        });
        settings::save_settings(&conn, &saved).unwrap();
        conn.execute(
            "INSERT INTO ai_batches (status, mode, total, created_at)
             VALUES ('done', 'cloud', 3, 1)",
            [],
        )
        .unwrap();
        let db = Arc::new(Mutex::new(conn));

        let target = temp.path().join("diagnostics.zip");
        let report = export_diagnostics_blocking(
            &data_dir,
            &db,
            target.to_str().expect("temporary path must be UTF-8"),
        )
        .unwrap();
        assert_eq!(report.log_files, 2);
        assert_eq!(report.truncated_logs, 0);
        assert!(report.bytes > 0);

        let mut archive = zip::ZipArchive::new(std::fs::File::open(&target).unwrap()).unwrap();
        assert!(archive.by_name("diagnostics.json").is_ok());
        assert!(archive.by_name("logs/app.log.2026-09-16").is_ok());
        assert!(archive.by_name("logs/fatal.log").is_ok());
        assert!(archive.by_name("logs/ignored.txt").is_err());

        let mut summary = String::new();
        archive
            .by_name("diagnostics.json")
            .unwrap()
            .read_to_string(&mut summary)
            .unwrap();
        assert!(summary.contains("\"logLevel\": \"debug\""));
        assert!(summary.contains("\"status\": \"done\""));
        assert!(summary.contains("\"includedLogBytes\""));
        assert!(summary.contains("\"totalBytes\": 20971520"));
        assert!(!summary.contains("sk-diagnostics-must-not-leak"));
        assert!(!summary.contains("https://example.invalid/v1"));
    }

    #[test]
    fn diagnostic_log_sources_keep_newest_tail_within_budget() {
        let temp = tempfile::tempdir().unwrap();
        let first = temp.path().join("app.log.2026-09-15");
        let second = temp.path().join("app.log.2026-09-16");
        std::fs::write(&first, b"old-log-data").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(&second, b"new-log-data").unwrap();

        let sources = collect_log_sources(temp.path(), 7, 4).unwrap();
        assert_eq!(sources.iter().map(|source| source.length).sum::<u64>(), 7);
        assert!(sources.iter().any(|source| source.truncated));
        assert_eq!(
            sources[0].path.file_name().unwrap().to_string_lossy(),
            "app.log.2026-09-16"
        );
    }
}
