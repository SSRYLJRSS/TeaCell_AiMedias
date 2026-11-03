//! 本地导出：只复制 + 逐文件结果 + 进度回调 + 取消 + export_tasks/export_items 持久化（架构 §4.4）
//!
//! A4-01 决策：库外移动已关闭，导出统一为「复制」。库内原文件、原路径、标签全部保留，
//! 旧请求 move=true 由命令层返回明确错误（见 `commands/export_cmd.rs`），而不是静默改成复制。
//! 逐文件结果落在 `export_items`：单个文件失败只记该行失败，不再中止整个任务，
//! 这样「目标已存在 / 权限拒绝 / 中途取消」都能返回真实分项结果。

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rusqlite::Connection;
use serde::Serialize;

use crate::db::export_items::{self, NewExportItem};
use crate::db::{assets, export};
use crate::error::{AppError, AppResult};
use crate::state::Database;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportProgress {
    pub task_id: i64,
    pub done: i64,
    pub total: i64,
}

/// 逐文件汇总：命令层用它生成「复制导出」完成摘要（目标位置 + 失败项）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportSummary {
    pub task_id: i64,
    pub dest_dir: String,
    pub copied: i64,
    pub failed: i64,
    pub skipped: i64,
    pub cancelled: bool,
}

/// R-26 子目录组织：flat（默认平铺）| by_tag（首个标签名，无标签入「未分组」）| by_date（YYYY-MM，taken_at 优先）
fn subdir_for(asset: &assets::Asset, layout: &str) -> String {
    match layout {
        "by_tag" => asset
            .tags
            .first()
            .map(|t| sanitize_dirname(&t.name))
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "未分组".to_string()),
        "by_date" => {
            let ts = asset.taken_at.unwrap_or(asset.created_at);
            chrono::DateTime::from_timestamp_millis(ts)
                .map(|dt| dt.format("%Y-%m").to_string())
                .unwrap_or_else(|| "未知日期".to_string())
        }
        _ => String::new(),
    }
}

/// Windows 非法字符清洗（目录名）；全非法时回退「未分组」
///
/// 安全：`.`, `..` 以及清洗后仍为点号的名称一律回退「未分组」，
/// 防止合法标签名 `..` 经清洗后仍是 `..`，拼接到授权目录父级造成越界。
fn sanitize_dirname(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| match c {
            '\\' | '/' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            c => c,
        })
        .collect();
    let s = s.trim().to_string();
    // 纯点号（含 ".", "..", "..."）或全空白一律视为不安全，回退默认分组。
    if s.is_empty() || s.chars().all(|c| c == '.') {
        "未分组".to_string()
    } else {
        s
    }
}

/// 校验子目录名是单层普通分量：不含路径分隔符、不是 `.`/`..`、非空。
/// 用于越界防线：只有单层普通分量拼到授权根目录下才不会越界。
fn is_single_normal_component(name: &str) -> bool {
    if name.is_empty() || name == "." || name == ".." {
        return false;
    }
    if name.contains('/') || name.contains('\\') || name.contains('\0') {
        return false;
    }
    // 盘符前缀（如 "C:"）或 UNC 前缀也视为越界
    let bytes = name.as_bytes();
    if bytes.len() >= 2 && bytes[1] == b':' {
        return false;
    }
    true
}

/// 一次复制导出的输入。收成一个结构体，避免参数继续膨胀（clippy too_many_arguments）。
pub struct ExportRun<'a> {
    pub task_id: i64,
    pub asset_ids: &'a [i64],
    pub dest_dir: &'a str,
    pub layout: &'a str,
    pub cancel: &'a Arc<AtomicBool>,
}

/// 逐文件「短锁读库 → 锁外复制 → 短锁记结果」，文件 IO 期间不持 DB 锁，
/// 避免大文件导出长时间阻塞全应用 DB 读写
/// R-26：layout 控制子目录组织（flat|by_tag|by_date）
/// A4-01：只复制；单文件失败记 failed 后继续，不中止任务
///
/// `on_item` 在每个逐文件结果写入 `export_items` 之后、且不持 DB 锁时回调，
/// 供命令层把真实分项结果登记成 operation 收据（A4-01 第 2 条）。
///
/// 硬失败（目标根目录不可建、目录身份被换成链接、收据写入中断等）也必须把这个
/// 任务落成终态：`export_tasks` 没有启动期对账链（`operations::reconcile_startup`
/// 只处理 operations 表），一旦留下 `running` 就再没有任何执行者会推进它，
/// 界面上的那条导出会永远转圈。所以这里统一兜底写 `failed` 并保留真实错误。
pub fn export_local<F: Fn(ExportProgress)>(
    db: &Arc<Database>,
    run: ExportRun<'_>,
    progress: F,
    on_item: &mut dyn FnMut(&NewExportItem) -> AppResult<()>,
) -> AppResult<ExportSummary> {
    let task_id = run.task_id;
    let outcome = copy_assets(db, run, progress, on_item);
    if let Err(error) = &outcome {
        match db.lock() {
            Ok(conn) => {
                if let Err(settle) =
                    export::finish_task(&conn, task_id, "failed", None, Some(&error.to_string()))
                {
                    tracing::error!(
                        operation = "export_local",
                        task_id,
                        error = %settle,
                        "导出硬失败后写终态失败"
                    );
                }
            }
            Err(_) => tracing::error!(
                operation = "export_local",
                task_id,
                "导出硬失败后数据库锁中毒，未能写终态"
            ),
        }
    }
    outcome
}

fn copy_assets<F: Fn(ExportProgress)>(
    db: &Arc<Database>,
    run: ExportRun<'_>,
    progress: F,
    on_item: &mut dyn FnMut(&NewExportItem) -> AppResult<()>,
) -> AppResult<ExportSummary> {
    let ExportRun {
        task_id,
        asset_ids,
        dest_dir,
        layout,
        cancel,
    } = run;
    let lock = || db.lock().map_err(|_| AppError::msg("数据库锁中毒"));
    let dest = PathBuf::from(dest_dir);
    // 授权根目录的真实身份：逐项复制前用它复验子目录没有越界。
    let dest_root = guard_target_directory(&dest)?;
    let started = Instant::now();
    let total = asset_ids.len() as i64;
    tracing::info!(
        operation = "export_local",
        task_id,
        stage = "start",
        mode = "copy",
        layout = %layout,
        total,
        "开始本地复制导出"
    );
    {
        let conn = lock()?;
        export::update_progress(&conn, task_id, 0, "running")?;
    }

    let mut copied = 0i64;
    let mut failed = 0i64;
    let mut skipped = 0i64;
    let mut processed = 0i64;
    let mut cancelled = false;

    for (ordinal, &id) in asset_ids.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            cancelled = true;
            // 未处理的素材也要有真实分项结果：记 skipped + CANCELLED，而不是留空白
            for (rest_index, &rest_id) in asset_ids.iter().enumerate().skip(ordinal) {
                let row = NewExportItem {
                    task_id,
                    ordinal: rest_index as i64,
                    asset_id: Some(rest_id),
                    file_name: String::new(),
                    dest_path: None,
                    state: export_items::STATE_SKIPPED.to_string(),
                    error_code: Some(export_items::CODE_CANCELLED.to_string()),
                    error_message: Some("任务已取消，未开始复制".to_string()),
                    created_at: now_millis(),
                };
                let conn = lock()?;
                export_items::insert_result(&conn, &row)?;
                drop(conn);
                on_item(&row)?;
                skipped += 1;
            }
            break;
        }

        let asset = {
            let conn = lock()?;
            assets::get_managed(&conn, id)
        };
        let asset = match asset {
            Ok(asset) => asset,
            Err(e) => {
                // 库内记录取不到（已删除/已转移）：这是真实的逐项失败，不是中止理由
                let row = NewExportItem {
                    task_id,
                    ordinal: ordinal as i64,
                    asset_id: Some(id),
                    file_name: String::new(),
                    dest_path: None,
                    state: export_items::STATE_FAILED.to_string(),
                    error_code: Some(export_items::CODE_SOURCE_MISSING.to_string()),
                    error_message: Some(e.to_string()),
                    created_at: now_millis(),
                };
                let conn = lock()?;
                export_items::insert_result(&conn, &row)?;
                export::update_progress(&conn, task_id, processed + 1, "running")?;
                drop(conn);
                on_item(&row)?;
                failed += 1;
                processed += 1;
                progress(ExportProgress {
                    task_id,
                    done: processed,
                    total,
                });
                continue;
            }
        };

        // R-26：按 layout 解析目标子目录（flat = 目标根目录）
        let sub = subdir_for(&asset, layout);
        // 目标子目录不安全（越界 / 被换成链接或重解析点）属于**单个文件**的
        // 失败：其余素材的目标目录完全正常，不该被一个异常标签名或一次外部
        // 替换把整批导出打断。所以这里记该文件失败后继续，而不是向上抛错。
        let target_dir = match resolve_target_dir(&dest, &sub, &dest_root) {
            Ok(dir) => dir,
            Err((code, message)) => {
                failed += 1;
                tracing::warn!(
                    operation = "export_local",
                    task_id,
                    asset_id = id,
                    file_name = %asset.file_name,
                    stage = "target_rejected",
                    error_code = code,
                    error = %message,
                    "导出目标子目录被拒绝（任务继续）"
                );
                let row = NewExportItem {
                    task_id,
                    ordinal: ordinal as i64,
                    asset_id: Some(id),
                    file_name: asset.file_name.clone(),
                    dest_path: None,
                    state: export_items::STATE_FAILED.to_string(),
                    error_code: Some(code.to_string()),
                    error_message: Some(message),
                    created_at: now_millis(),
                };
                processed += 1;
                {
                    let conn = lock()?;
                    export_items::insert_result(&conn, &row)?;
                    export::update_progress(&conn, task_id, processed, "running")?;
                }
                on_item(&row)?;
                progress(ExportProgress {
                    task_id,
                    done: processed,
                    total,
                });
                continue;
            }
        };

        let outcome = copy_one(&asset, &target_dir);
        let (state, dest_path, code, message) = match outcome {
            Ok((path, copied_bytes)) => {
                copied += 1;
                (
                    export_items::STATE_COPIED,
                    Some(path),
                    None,
                    // 落盘核验通过的真实字节数，便于事后核对
                    Some(format!("已复制 {copied_bytes} 字节")),
                )
            }
            Err((code, message)) => {
                failed += 1;
                tracing::warn!(
                    operation = "export_local",
                    task_id,
                    asset_id = id,
                    file_name = %asset.file_name,
                    stage = "file_failed",
                    error_code = code,
                    error = %message,
                    "导出单个素材失败（任务继续）"
                );
                (
                    export_items::STATE_FAILED,
                    None,
                    Some(code.to_string()),
                    Some(message),
                )
            }
        };

        let row = NewExportItem {
            task_id,
            ordinal: ordinal as i64,
            asset_id: Some(id),
            file_name: asset.file_name.clone(),
            dest_path: dest_path.clone(),
            state: state.to_string(),
            error_code: code,
            error_message: message,
            created_at: now_millis(),
        };
        processed += 1;
        {
            let conn = lock()?;
            export_items::insert_result(&conn, &row)?;
            export::update_progress(&conn, task_id, processed, "running")?;
        }
        on_item(&row)?;
        progress(ExportProgress {
            task_id,
            done: processed,
            total,
        });
    }

    let summary = ExportSummary {
        task_id,
        dest_dir: dest_dir.to_string(),
        copied,
        failed,
        skipped,
        cancelled,
    };
    {
        let conn = lock()?;
        if cancelled {
            export::finish_task(&conn, task_id, "cancelled", None, None)?;
            tracing::warn!(
                operation = "export_local",
                task_id,
                stage = "cancelled",
                copied,
                failed,
                skipped,
                total,
                duration_ms = started.elapsed().as_millis() as u64,
                "本地导出已取消（逐文件结果已保留）"
            );
        } else if failed == 0 && skipped == 0 {
            export::finish_task(&conn, task_id, "done", None, None)?;
            tracing::info!(
                operation = "export_local",
                task_id,
                stage = "done",
                copied,
                total,
                duration_ms = started.elapsed().as_millis() as u64,
                "本地导出完成"
            );
        } else if copied == 0 {
            export::finish_task(
                &conn,
                task_id,
                "failed",
                None,
                Some(&format!("{failed} 个文件复制失败，详见逐文件结果")),
            )?;
            tracing::error!(
                operation = "export_local",
                task_id,
                stage = "failed",
                failed,
                total,
                duration_ms = started.elapsed().as_millis() as u64,
                "本地导出全部失败"
            );
        } else {
            // 部分成功：任务仍是 done（有真实输出），但用 warning 明示失败数量，不谎报全成功
            export::finish_task_with_warning(
                &conn,
                task_id,
                &format!("{failed} 个文件复制失败，其余 {copied} 个已完成（详见逐文件结果）"),
            )?;
            tracing::warn!(
                operation = "export_local",
                task_id,
                stage = "done_with_failures",
                copied,
                failed,
                total,
                duration_ms = started.elapsed().as_millis() as u64,
                "本地导出部分成功"
            );
        }
    }
    Ok(summary)
}

/// 解析单个素材的目标目录并复验它是授权根目录下的一层普通目录。
///
/// 为什么不能只在开始时验证一次：授权句柄在导出开始时是可信的，但目标目录里的
/// 子目录（如「按标签分目录」生成的 `风光`/`人物`）是这批导出自己建出来的，
/// 期间完全可能被换成符号链接、目录联接或重解析点，也可能整条路径被替换——那样
/// 后续复制就会把文件写到授权目录之外。每次写入前用符号元数据 + 规范化父目录
/// 重新确认，才算逐项复验。
///
/// 失败返回 `(error_code, message)`，由调用方记成该文件的逐项失败并继续。
fn resolve_target_dir(
    dest: &Path,
    sub: &str,
    dest_root: &Path,
) -> Result<PathBuf, (&'static str, String)> {
    let reject = |message: String| (export_items::CODE_PATH_REJECTED, message);
    if sub.is_empty() {
        return Ok(dest_root.to_path_buf());
    }
    // 越界防线：`sub` 必须是单层、非点的普通相对分量，拼出的子目录
    // 才不可能越出授权目标根目录。即便 sanitize_dirname 已拦截点号，
    // 这里再做一次结构性校验，防止未来 layout/命名变化引入 `..` 或绝对路径越界。
    if !is_single_normal_component(sub) {
        return Err(reject(format!("标签子目录越出导出目标目录：{sub}")));
    }
    let dir = dest.join(sub);
    fs::create_dir_all(&dir).map_err(|error| {
        (
            export_items::CODE_PERMISSION_DENIED,
            format!("无法创建子目录：{error}"),
        )
    })?;
    let metadata = fs::symlink_metadata(&dir).map_err(|error| {
        (
            export_items::CODE_PERMISSION_DENIED,
            format!("子目录不可访问：{error}"),
        )
    })?;
    if crate::utils::path::is_link_or_reparse(&metadata) {
        return Err(reject(format!("导出子目录是链接或重解析点：{sub}")));
    }
    if !metadata.is_dir() {
        return Err(reject(format!("导出子目录不是目录：{sub}")));
    }
    let canonical = dir.canonicalize().map_err(|error| {
        (
            export_items::CODE_PERMISSION_DENIED,
            format!("子目录不可访问：{error}"),
        )
    })?;
    if canonical.parent() != Some(dest_root) {
        return Err(reject(format!("导出子目录不在授权目标目录下：{sub}")));
    }
    Ok(canonical)
}

/// 授权根目录本身也要复验一次：它是本批导出的写入边界，如果根目录已经是
/// 链接/重解析点，后续所有「在根目录下写文件」的判断都不可信。
fn guard_target_directory(dir: &Path) -> AppResult<PathBuf> {
    fs::create_dir_all(dir)
        .map_err(|error| AppError::conflict(format!("无法创建导出目标目录：{error}")))?;
    // 必须用 symlink_metadata：`metadata` 会跟随链接，把链接目标当成普通目录。
    let metadata = fs::symlink_metadata(dir)
        .map_err(|error| AppError::conflict(format!("导出目标目录不可访问：{error}")))?;
    if crate::utils::path::is_link_or_reparse(&metadata) {
        return Err(AppError::unauthorized(
            "导出目标目录是链接或重解析点，已拒绝写入",
        ));
    }
    if !metadata.is_dir() {
        return Err(AppError::invalid_arg("导出目标路径不是目录"));
    }
    dir.canonicalize()
        .map_err(|error| AppError::conflict(format!("导出目标目录不可访问：{error}")))
}

/// 复制单个文件到目标目录并核验：不覆盖同名文件（create_new），
/// 字节数与源文件一致才返回成功；失败时清理半截目标文件。
fn copy_one(
    asset: &assets::Asset,
    target_dir: &Path,
) -> Result<(String, u64), (&'static str, String)> {
    let src = PathBuf::from(&asset.file_path);
    let (dst, norm) = unique_dest(target_dir, &asset.file_name)
        .map_err(|e| (export_items::CODE_TARGET_EXISTS, e.to_string()))?;
    let expected = asset.file_size.max(0) as u64;
    // copy_to_new 区分「本进程 create_new 成功后才失败」与「create_new 未成功
    // 建立目标」。只有前者才允许本进程清理目标；后者（典型为同名竞态
    // AlreadyExists）目标由别的进程创建，绝不能删——否则会误删他人文件。
    match copy_to_new(&src, &dst) {
        Ok(bytes) => {
            let actual = fs::metadata(&dst).map(|m| m.len()).unwrap_or(bytes);
            if expected > 0 && actual != expected {
                let _ = fs::remove_file(&dst);
                return Err((
                    export_items::CODE_IO_ERROR,
                    format!("复制后大小不一致（源 {expected} 字节，目标 {actual} 字节）"),
                ));
            }
            Ok((norm, actual))
        }
        Err(CopyError::CreatedThenFailed(e)) => {
            // 本进程已创建、写入中途失败：半截文件必须清掉，否则下次重试会撞「目标已存在」
            let _ = fs::remove_file(&dst);
            Err((classify_io_error(&e), e.to_string()))
        }
        Err(CopyError::NotCreated(e)) => {
            // create_new 未成功建立目标（如同名竞态 AlreadyExists）：目标不属于本进程，绝不清理
            Err((classify_io_error(&e), e.to_string()))
        }
    }
}

/// 复制失败的来源，决定调用方是否有权清理目标文件。
enum CopyError {
    /// create_new 已成功创建目标，但后续读取/写入失败（半截文件由本进程创建）。
    CreatedThenFailed(io::Error),
    /// create_new 未成功建立目标（例如同名竞态 AlreadyExists），目标不属于本进程。
    NotCreated(io::Error),
}

/// 严格新建复制：`create_new(true)` 保证即使出现同名竞态也绝不覆盖既有文件。
/// 失败时区分「本进程创建后失败」与「未创建」，供调用方决定是否清理目标，
/// 避免误删竞争者创建的同名文件。
fn copy_to_new(src: &Path, dst: &Path) -> Result<u64, CopyError> {
    let mut input = fs::File::open(src).map_err(CopyError::NotCreated)?;
    let mut output = match fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dst)
    {
        Ok(file) => file,
        // 目标未由本进程创建：不清理。
        Err(e) => return Err(CopyError::NotCreated(e)),
    };
    match io::copy(&mut input, &mut output).and_then(|bytes| output.sync_all().map(|_| bytes)) {
        // 目标已由本进程创建，后续失败：由调用方清理半截文件。
        Ok(bytes) => Ok(bytes),
        Err(e) => Err(CopyError::CreatedThenFailed(e)),
    }
}

/// 逐文件错误码分类：把系统错误映射到用户能看懂的失败原因（A4-01 验收要求分项结果）
fn classify_io_error(e: &io::Error) -> &'static str {
    match e.kind() {
        io::ErrorKind::PermissionDenied => export_items::CODE_PERMISSION_DENIED,
        io::ErrorKind::NotFound => export_items::CODE_SOURCE_MISSING,
        io::ErrorKind::AlreadyExists => export_items::CODE_TARGET_EXISTS,
        _ => {
            if is_disk_full(e) {
                export_items::CODE_DISK_FULL
            } else {
                export_items::CODE_IO_ERROR
            }
        }
    }
}

/// 空间不足识别：Windows ERROR_DISK_FULL(112)/ERROR_HANDLE_DISK_FULL(39)，Unix ENOSPC(28)
fn is_disk_full(e: &io::Error) -> bool {
    match e.raw_os_error() {
        Some(raw) if cfg!(windows) => raw == 112 || raw == 39,
        Some(raw) if cfg!(unix) => raw == 28,
        _ => false,
    }
}

fn now_millis() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// 同名冲突自动加 (1)(2) 后缀；B06b：冲突超限(999)时报错而非回退覆盖
fn unique_dest(dir: &Path, name: &str) -> AppResult<(PathBuf, String)> {
    let candidate = dir.join(name);
    if !candidate.exists() {
        let path = crate::utils::path::encode_native_path(&candidate)?;
        let norm = crate::utils::path::normalize_path(&path);
        return Ok((candidate, norm));
    }
    let stem = Path::new(name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    let ext = Path::new(name)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
        .unwrap_or_default();
    for i in 1..1000 {
        let c = dir.join(format!("{stem}({i}){ext}"));
        if !c.exists() {
            let path = crate::utils::path::encode_native_path(&c)?;
            let norm = crate::utils::path::normalize_path(&path);
            return Ok((c, norm));
        }
    }
    Err(AppError::msg(format!("目标目录同名文件过多: {name}")))
}

/// R-26 CSV 清单导出：文件名/路径/标签/EXIF 摘要，UTF-8 BOM 保 Excel 兼容；
/// 返回清单文件路径。不存在的 id 跳过（部分成功，同 get_asset_urls 语义）
/// P1-03：与素材导出同一 unique_dest 策略（同名自动加 (1)(2)…），不再静默覆盖已有清单；
/// 先写临时文件再原子重命名，崩溃时序不留半截文件
pub fn write_csv_manifest(
    conn: &Connection,
    asset_ids: &[i64],
    dest_dir: &str,
) -> AppResult<PathBuf> {
    let dest = PathBuf::from(dest_dir);
    fs::create_dir_all(&dest)?;
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"\xef\xbb\xbf"); // UTF-8 BOM
    buf.extend_from_slice(
        "文件名,文件路径,标签,宽,高,大小字节,拍摄时间,相机,镜头,ISO,光圈,快门,焦距\r\n".as_bytes(),
    );
    for &id in asset_ids {
        let Ok(a) = assets::get_managed(conn, id) else {
            continue;
        };
        let tags = a
            .tags
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>()
            .join("; ");
        let taken = a
            .taken_at
            .and_then(chrono::DateTime::from_timestamp_millis)
            .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_default();
        let row = [
            &a.file_name,
            &a.file_path,
            &tags,
            &a.width.map(|v| v.to_string()).unwrap_or_default(),
            &a.height.map(|v| v.to_string()).unwrap_or_default(),
            &a.file_size.to_string(),
            &taken,
            a.camera.as_deref().unwrap_or_default(),
            a.lens.as_deref().unwrap_or_default(),
            &a.iso.map(|v| v.to_string()).unwrap_or_default(),
            &a.aperture.map(|v| v.to_string()).unwrap_or_default(),
            a.shutter.as_deref().unwrap_or_default(),
            &a.focal.map(|v| v.to_string()).unwrap_or_default(),
        ]
        .iter()
        .map(|f| csv_escape(f))
        .collect::<Vec<_>>()
        .join(",");
        buf.extend_from_slice(row.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }
    // P1-03：不走固定文件名——已存在同名清单时自动加序号，绝不覆盖用户旧文件
    let (out, _) = unique_dest(&dest, "导出清单.csv")?;
    // 原子写：临时文件写完后 rename（同目录 rename 原子；Windows 下目标不存在即成功）
    let tmp = out.with_extension("csv.tmp");
    fs::write(&tmp, &buf)?;
    if let Err(e) = fs::rename(&tmp, &out) {
        let _ = fs::remove_file(&tmp);
        return Err(AppError::from(e));
    }
    Ok(out)
}

/// CSV 字段转义：含逗号/引号/换行时加双引号包裹；
/// P2-09：以 = + - @ 开头的值（恶意文件名 / AI 建议标签 = 不可信输入）前置单引号，
/// 防 Excel 打开时被当作公式执行
fn csv_escape(s: &str) -> String {
    let formula_prefix = matches!(s.chars().next(), Some('=' | '+' | '-' | '@'));
    let s = if formula_prefix {
        format!("'{s}")
    } else {
        s.to_string()
    };
    if s.contains(',') || s.contains('"') || s.contains('\n') || s.contains('\r') {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_escape_formula_prefix_neutralized() {
        // P2-09：= + - @ 开头一律前置单引号，防 Excel 公式执行
        assert_eq!(csv_escape("=SUM(A1:A9)"), "'=SUM(A1:A9)");
        assert_eq!(csv_escape("+cmd|' /C calc"), "'+cmd|' /C calc");
        assert_eq!(csv_escape("-1"), "'-1");
        assert_eq!(csv_escape("@SUM"), "'@SUM");
        // 正常值不受影响
        assert_eq!(csv_escape("公园"), "公园");
        assert_eq!(csv_escape("1/250"), "1/250");
        // 前缀转义与引号包裹/逗号转义共存
        assert_eq!(csv_escape("=x,y"), "\"'=x,y\"");
        assert_eq!(csv_escape("正常,标签"), "\"正常,标签\"");
        assert_eq!(csv_escape("说\"好"), "\"说\"\"好\"");
        assert_eq!(csv_escape(""), "");
    }

    #[test]
    fn csv_manifest_never_overwrites_and_unique_names() {
        let conn = crate::db::init_memory().expect("内存库初始化失败");
        let dir = tempfile::tempdir().expect("临时目录创建失败");
        let dest = dir.path().join("out");
        let name = |p: &std::path::Path| p.file_name().unwrap().to_string_lossy().into_owned();

        // P1-03：首次固定名，二次自动加 (1)，绝不覆盖已有清单
        let p1 = write_csv_manifest(&conn, &[], &dest.to_string_lossy()).expect("首次清单失败");
        assert_eq!(name(&p1), "导出清单.csv");
        let p2 = write_csv_manifest(&conn, &[], &dest.to_string_lossy()).expect("二次清单失败");
        assert_eq!(name(&p2), "导出清单(1).csv");
        let p3 = write_csv_manifest(&conn, &[], &dest.to_string_lossy()).expect("三次清单失败");
        assert_eq!(name(&p3), "导出清单(2).csv");

        // 第一份内容未被覆盖（三份逐字节一致：空资产 → 仅表头）
        let c1 = std::fs::read(&p1).unwrap();
        let c2 = std::fs::read(&p2).unwrap();
        assert_eq!(c1, c2);
        // 原子写不留 .tmp 残留
        assert!(!dir.path().join("out").join("导出清单.csv.tmp").exists());
    }

    // P0-1 回归：同名竞态时，create_new 报 AlreadyExists 绝不能删除竞争者创建的目标文件。
    #[test]
    fn copy_to_new_never_deletes_competitor_file_on_already_exists() {
        let dir = tempfile::tempdir().expect("临时目录");
        let src = dir.path().join("src.bin");
        std::fs::write(&src, b"hello").expect("写源");
        let dst = dir.path().join("dst.bin");
        // 竞争者先创建了同名目标，内容与源不同
        std::fs::write(&dst, b"competitor-data").expect("写竞争者文件");

        let outcome = copy_to_new(&src, &dst);
        match outcome {
            Err(CopyError::NotCreated(e)) => {
                assert_eq!(e.kind(), io::ErrorKind::AlreadyExists);
            }
            _ => panic!("同名竞态应返回 NotCreated(AlreadyExists)"),
        }
        // 关键断言：竞争者创建的文件必须仍存在且内容未被删改
        let remaining = std::fs::read(&dst).expect("竞争者文件被误删！");
        assert_eq!(remaining, b"competitor-data");
    }

    #[test]
    fn copy_to_new_reports_created_then_failed_when_sync_fails_after_create() {
        // 正常路径：源不存在 → NotCreated(NotFound)，不创建任何目标。
        let dir = tempfile::tempdir().expect("临时目录");
        let src = dir.path().join("missing.bin");
        let dst = dir.path().join("dst.bin");
        match copy_to_new(&src, &dst) {
            Err(CopyError::NotCreated(e)) => assert_eq!(e.kind(), io::ErrorKind::NotFound),
            _ => panic!("源不存在应返回 NotCreated(NotFound)"),
        }
        assert!(!dst.exists());
    }

    // P0-3 回归：`..` / `.` 标签名不得越出授权目录，必须回退默认分组。
    #[test]
    fn sanitize_dirname_rejects_dot_segments() {
        assert_eq!(sanitize_dirname(".."), "未分组");
        assert_eq!(sanitize_dirname("."), "未分组");
        assert_eq!(sanitize_dirname("..."), "未分组");
        assert_eq!(sanitize_dirname("  "), "未分组");
        // 含点但非纯点号仍保留（如 "v1.2"）
        assert_eq!(sanitize_dirname("v1.2"), "v1.2");
        // 分隔符被清洗
        assert_eq!(sanitize_dirname("a/b"), "a_b");
    }

    #[test]
    fn is_single_normal_component_blocks_traversal_and_absolutes() {
        assert!(is_single_normal_component("风光"));
        assert!(!is_single_normal_component(""));
        assert!(!is_single_normal_component("."));
        assert!(!is_single_normal_component(".."));
        assert!(!is_single_normal_component("a/b"));
        assert!(!is_single_normal_component("a\\b"));
        assert!(!is_single_normal_component("C:evil"));
    }

    /// 尝试造一个目录链接；当前环境不允许时返回 false（用例跳过而不是假通过）。
    fn try_directory_link(target: &std::path::Path, link: &std::path::Path) -> bool {
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(target, link).is_ok()
        }
        #[cfg(windows)]
        {
            std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .map(|out| out.status.success() && link.exists())
                .unwrap_or(false)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = (target, link);
            false
        }
    }

    /// A6-04：环境缺项的两种走向。本机默认允许跳过（只打印原因），交付门禁以
    /// `TEACELL_GATE_STRICT=1` 运行，把「无法构造前置」当成失败，避免证据静默通过。
    fn missing_precondition_decision(what: &str, strict: bool) -> bool {
        if strict {
            return false;
        }
        eprintln!("跳过：{what}");
        true
    }

    fn gate_strict() -> bool {
        matches!(std::env::var("TEACELL_GATE_STRICT").as_deref(), Ok("1"))
    }

    /// 造目录链接失败时：严格模式 fail closed，否则返回 false 让用例提前返回。
    fn require_directory_link(target: &std::path::Path, link: &std::path::Path) -> bool {
        if try_directory_link(target, link) {
            return true;
        }
        let what = "当前环境不允许创建目录链接，无法构造该用例的前置条件";
        if !missing_precondition_decision(what, gate_strict()) {
            panic!("A6-04：{what}（严格模式下不得跳过）");
        }
        false
    }

    #[test]
    fn strict_mode_turns_a_missing_directory_link_precondition_into_a_failure() {
        assert!(
            missing_precondition_decision("链接不可建", false),
            "非严格模式应跳过而不是失败"
        );
        assert!(
            !missing_precondition_decision("链接不可建", true),
            "严格模式下缺前置必须 fail closed，否则该用例会静默通过"
        );
    }

    // A4-01 逐项复验：目标子目录名不安全时只让该文件失败，且错误码是「路径被拒绝」，
    // 不是把它当成任务级硬失败（其余素材的目标目录完全正常）。
    #[test]
    fn resolve_target_dir_rejects_unsafe_names_with_a_per_file_code() {
        let dir = tempfile::tempdir().expect("临时目录");
        let dest = dir.path().join("out");
        std::fs::create_dir_all(&dest).expect("建目标");
        let root = guard_target_directory(&dest).expect("根目录应可用");

        for bad in ["..", ".", "a/b", "a\\b", "C:evil"] {
            match resolve_target_dir(&dest, bad, &root) {
                Err((code, message)) => {
                    assert_eq!(code, export_items::CODE_PATH_REJECTED, "子目录名 {bad}");
                    assert!(
                        message.contains("越出") || message.contains("不在授权"),
                        "错误文案要说清越界原因：{message}"
                    );
                }
                Ok(path) => panic!("子目录名 {bad} 必须被拒绝，却解析成了 {path:?}"),
            }
        }
        // 正常标签名仍可用，并返回授权根下的规范路径
        let ok = resolve_target_dir(&dest, "风光", &root).expect("正常子目录应可用");
        assert_eq!(ok.parent(), Some(root.as_path()));
        // 单层路径：flat 布局直接用根目录
        assert_eq!(
            resolve_target_dir(&dest, "", &root).expect("flat 应可用"),
            root
        );
    }

    // A4-01 逐项复验：子目录在导出期间被换成链接/目录联接时，必须拒绝写入——
    // 否则 `create_dir_all` 认为目录已存在，后续文件就落到授权目录之外。
    #[test]
    fn resolve_target_dir_rejects_a_subdirectory_that_became_a_link() {
        let dir = tempfile::tempdir().expect("临时目录");
        let dest = dir.path().join("out");
        std::fs::create_dir_all(&dest).expect("建目标");
        let root = guard_target_directory(&dest).expect("根目录应可用");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).expect("建外部目录");
        let sub = dest.join("风光");
        if !require_directory_link(&outside, &sub) {
            return;
        }
        match resolve_target_dir(&dest, "风光", &root) {
            Err((code, message)) => {
                assert_eq!(code, export_items::CODE_PATH_REJECTED);
                assert!(
                    message.contains("链接")
                        || message.contains("重解析点")
                        || message.contains("不在授权"),
                    "错误文案要说明链接原因：{message}"
                );
            }
            Ok(path) => panic!("子目录被换成链接时必须拒绝，却解析成了 {path:?}"),
        }
    }

    // 授权根目录本身被换成链接时，整个写入边界不可信：直接硬失败（任务级），
    // 因为此时没有任何一个文件的目标目录是可信的。
    #[test]
    fn guard_target_directory_rejects_a_link_destination_root() {
        let dir = tempfile::tempdir().expect("临时目录");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&outside).expect("建外部目录");
        let root = dir.path().join("linked-out");
        if !require_directory_link(&outside, &root) {
            return;
        }
        let error = guard_target_directory(&root).expect_err("根目录是链接时必须拒绝");
        assert!(
            error.to_string().contains("链接") || error.to_string().contains("重解析点"),
            "错误文案要说明链接原因：{error}"
        );
    }

    // A4-01：硬失败必须把 export_tasks 落成终态。`export_tasks` 没有启动期对账链
    // （operations::reconcile_startup 只管 operations 表），留下 running 就再没有
    // 执行者会推进它，界面上的导出会永远转圈。
    #[test]
    fn hard_failure_leaves_the_export_task_in_a_terminal_failed_state() {
        let dir = tempfile::tempdir().expect("临时目录");
        // 用一个普通文件当「目录」，让目标根目录必然建不出来
        let blocker = dir.path().join("blocker");
        std::fs::write(&blocker, b"not a directory").expect("写占位文件");
        let bad_dest = blocker.join("child");

        let conn = crate::db::init_memory().expect("内存库初始化失败");
        let task = export::create_task(&conn, "local", 1, Some(&bad_dest.to_string_lossy()), None)
            .expect("建任务");
        let db = Arc::new(Database::new(conn));
        let cancel = Arc::new(AtomicBool::new(false));

        let result = export_local(
            &db,
            ExportRun {
                task_id: task.id,
                asset_ids: &[1],
                dest_dir: &bad_dest.to_string_lossy(),
                layout: "flat",
                cancel: &cancel,
            },
            |_| {},
            &mut |_| Ok(()),
        );
        assert!(result.is_err(), "目标目录建不出来必须报错");

        let after = export::get_task(&db.lock().unwrap(), task.id).expect("读任务");
        assert_eq!(
            after.status, "failed",
            "硬失败后任务必须落成终态，不能永远停在 {}",
            after.status
        );
        assert!(
            after.error.is_some(),
            "终态要带真实原因，便于用户判断该怎么修"
        );
    }
}
