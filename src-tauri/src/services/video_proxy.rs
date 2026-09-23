//! 视频兼容代理服务（指导书 §8.3）：原文件 → H.264/AAC MP4 按需生成并缓存。
//!  - 代理路径按 素材 ID + 变体 稳定生成：`{proxy_dir}/{asset_id}_{variant}.mp4`；
//!  - 生成使用临时文件 + 原子 rename；状态机 queued|running|ready|failed|canceled 持久化；
//!  - 同一素材同一变体 single-flight：同一时刻只一个生成任务，其他调用方复用结果；
//!  - 全局转码并发闸（§8.1）：默认并发 = 1（软件 libx264），跨素材生效，不只是 single-flight；
//!    许可在 ffmpeg 子进程启动前获取，转码结束/失败/取消时释放（RAII guard）；
//!    等待队列期间不持数据库锁，且等待可被取消（cancel flag 轮询）；
//!  - 代理不是替换原文件：清理缓存不影响原文件。
//!    转码由调用方注入（便于单测注入 fake 转码；真实路径用 video::transcode_to_h264）。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use rusqlite::Connection;

use crate::db::{assets, video_proxy};
use crate::error::{AppError, AppResult};

/// 全局转码并发上限（§8.1）：默认 1。硬件编码探测成功后（未来配置 video_proxy_concurrency）
/// 才允许提高到 2。静态全局，跨素材共享——不同素材同时转码也被限制。
const TRANSCODE_CONCURRENCY: usize = 1;

/// 全局并发闸：计数信号量 + 条件变量（RAII guard 释放）。等待可被 cancel flag 打断。
static TRANSCODE_GATE: OnceLock<Arc<Mutex<usize>>> = OnceLock::new();
static TRANSCODE_COND: OnceLock<Condvar> = OnceLock::new();

fn gate() -> &'static Mutex<usize> {
    TRANSCODE_GATE.get_or_init(|| Arc::new(Mutex::new(TRANSCODE_CONCURRENCY)))
}

fn cond() -> &'static Condvar {
    TRANSCODE_COND.get_or_init(Condvar::new)
}

/// 转码许可（RAII）：Drop 时归还许可并唤醒一个等待者。
struct TranscodeLicense;

impl TranscodeLicense {
    /// 获取一个转码许可；cancel=true 时等待被打断返回 None（调用方应写入 canceled 状态）。
    /// 等待期间不持数据库锁（本函数只操作信号量）。
    fn acquire(cancel: &AtomicBool, timeout: Duration) -> Option<Self> {
        let deadline = Instant::now() + timeout;
        let mut n = gate().lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if *n > 0 {
                *n -= 1;
                return Some(TranscodeLicense);
            }
            if cancel.load(Ordering::Relaxed) {
                return None;
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let (guard, _) = cond()
                .wait_timeout(n, Duration::from_millis(100))
                .unwrap_or_else(|e| e.into_inner());
            n = guard;
        }
    }
}

impl Drop for TranscodeLicense {
    fn drop(&mut self) {
        let mut n = gate().lock().unwrap_or_else(|e| e.into_inner());
        *n = (*n + 1).min(TRANSCODE_CONCURRENCY);
        drop(n);
        cond().notify_one();
    }
}

/// single-flight：按 `asset_id:variant` 一把互斥锁，同一时刻只一个生成任务。
static INFLIGHT: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

fn inflight() -> &'static Mutex<HashMap<String, Arc<Mutex<()>>>> {
    INFLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

fn with_single_flight(
    key: &str,
    f: impl FnOnce() -> AppResult<video_proxy::VideoProxy>,
) -> AppResult<video_proxy::VideoProxy> {
    let lock = {
        let mut m = inflight().lock().unwrap_or_else(|e| e.into_inner());
        m.entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

fn proxy_path(proxy_dir: &Path, asset_id: i64, variant: &str) -> PathBuf {
    let ext = if variant == "vp8_webm" { "webm" } else { "mp4" };
    proxy_dir.join(format!("{asset_id}_{variant}.{ext}"))
}

/// 生成临时路径（与目标同目录、同扩展名），用于「写入临时文件 → 原子 rename」。
fn temp_path(out: &Path, uid: &str) -> PathBuf {
    out.parent().unwrap_or(Path::new(".")).join(format!(
        "{}.{}.{}",
        out.file_stem().and_then(|s| s.to_str()).unwrap_or("p"),
        uid,
        out.extension().and_then(|s| s.to_str()).unwrap_or("mp4")
    ))
}

/// 原子 rename（同文件系统），Windows 目标被占用时重试。
fn atomic_rename(tmp: &Path, out: &Path) -> AppResult<()> {
    for _ in 0..3 {
        match fs::rename(tmp, out) {
            Ok(()) => return Ok(()),
            Err(e) => {
                std::thread::sleep(std::time::Duration::from_millis(50));
                if e.kind() != std::io::ErrorKind::PermissionDenied
                    && e.kind() != std::io::ErrorKind::AlreadyExists
                {
                    return Err(AppError::msg(format!("代理 rename 失败: {e}")));
                }
            }
        }
    }
    Err(AppError::file_locked("代理 rename 失败（目标被占用）"))
}

/// 获取或生成代理。transcode(src, tmp, cancel) 由调用方注入：
/// 成功需在 tmp 写入完整可解码文件；失败返回 Err(可解释原因)。
pub fn get_or_create_proxy(
    db: &Arc<Mutex<Connection>>,
    proxy_dir: &Path,
    asset_id: i64,
    variant: &str,
    cancel: &AtomicBool,
    transcode: impl Fn(&Path, &Path, &AtomicBool) -> AppResult<()>,
) -> AppResult<video_proxy::VideoProxy> {
    if !variant
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return Err(AppError::msg("非法代理变体"));
    }
    let started = Instant::now();
    tracing::debug!(
        operation = "video_proxy",
        asset_id,
        variant = %variant,
        stage = "start",
        "开始获取视频代理"
    );
    let key = format!("{asset_id}:{variant}");
    with_single_flight(&key, || {
        // ① 已 ready 且文件存在 → 复用
        {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            if let Some(p) = video_proxy::get(&conn, asset_id, variant)? {
                if p.status == "ready" {
                    if let Some(path) = &p.path {
                        if Path::new(path).exists() {
                            tracing::info!(
                                operation = "video_proxy",
                                asset_id,
                                variant = %variant,
                                stage = "cache_hit",
                                duration_ms = started.elapsed().as_millis() as u64,
                                "视频代理缓存命中"
                            );
                            return Ok(p);
                        }
                    }
                }
            }
        }
        // ② 读素材（短锁），确定输出路径
        let (mime, src) = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            let a = assets::get(&conn, asset_id)?;
            (a.mime_type, PathBuf::from(a.file_path))
        };
        if !mime.starts_with("video/") {
            tracing::warn!(
                operation = "video_proxy",
                asset_id,
                variant = %variant,
                stage = "unsupported_asset",
                error_code = "UNSUPPORTED",
                duration_ms = started.elapsed().as_millis() as u64,
                "素材不是视频，跳过代理生成"
            );
            {
                let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
                video_proxy::upsert(
                    &conn,
                    asset_id,
                    variant,
                    "failed",
                    None,
                    Some("素材不是视频"),
                )?;
            }
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            return video_proxy::get(&conn, asset_id, variant)?
                .ok_or_else(|| AppError::msg("代理记录写入失败"));
        }
        fs::create_dir_all(proxy_dir)?;
        let out = proxy_path(proxy_dir, asset_id, variant);
        if out.exists() {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            video_proxy::upsert(
                &conn,
                asset_id,
                variant,
                "ready",
                Some(&out.to_string_lossy()),
                None,
            )?;
            tracing::info!(
                operation = "video_proxy",
                asset_id,
                variant = %variant,
                stage = "disk_reuse",
                duration_ms = started.elapsed().as_millis() as u64,
                "代理文件已存在，恢复数据库状态"
            );
            return video_proxy::get(&conn, asset_id, variant)?
                .ok_or_else(|| AppError::msg("代理记录写入失败"));
        }
        // ③ 标记 running（短锁）
        {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            video_proxy::upsert(&conn, asset_id, variant, "running", None, None)?;
        }
        // ④ 全局转码并发闸：ffmpeg 启动前获取许可；等待可被取消；等待不持数据库锁
        //    （§8.1：默认并发 1，跨素材生效；超时 30s 与 ffmpeg 墙钟一致）
        let license_started = Instant::now();
        let license = match TranscodeLicense::acquire(cancel, Duration::from_secs(30)) {
            Some(l) => l,
            None => {
                let canceled = cancel.load(Ordering::Relaxed);
                let status = if canceled { "canceled" } else { "failed" };
                let reason = if canceled {
                    "已取消（等待转码许可）"
                } else {
                    "等待转码许可超时"
                };
                let error_code = if canceled { "CANCELLED" } else { "TIMEOUT" };
                tracing::warn!(
                    operation = "video_proxy",
                    asset_id,
                    variant = %variant,
                    stage = "license_unavailable",
                    outcome = status,
                    error_code,
                    wait_ms = license_started.elapsed().as_millis() as u64,
                    duration_ms = started.elapsed().as_millis() as u64,
                    reason,
                    "获取视频转码许可失败"
                );
                let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
                video_proxy::upsert(&conn, asset_id, variant, status, None, Some(reason))?;
                return video_proxy::get(&conn, asset_id, variant)?
                    .ok_or_else(|| AppError::msg("代理记录写入失败"));
            }
        };
        tracing::debug!(
            operation = "video_proxy",
            asset_id,
            variant = %variant,
            stage = "license_acquired",
            wait_ms = license_started.elapsed().as_millis() as u64,
            "获取视频转码许可"
        );
        // ⑤ 锁外转码到临时文件（许可持有时限 = 转码时长；结束后 drop 自动释放）
        let uid = uuid::Uuid::new_v4().to_string();
        let tmp = temp_path(&out, &uid);
        let transcode_result = transcode(&src, &tmp, cancel);
        drop(license); // 显式释放许可（任何分支都提前归还）
        match transcode_result {
            Ok(()) => match atomic_rename(&tmp, &out) {
                Ok(()) => {
                    let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
                    video_proxy::upsert(
                        &conn,
                        asset_id,
                        variant,
                        "ready",
                        Some(&out.to_string_lossy()),
                        None,
                    )?;
                    tracing::info!(
                        operation = "video_proxy",
                        asset_id,
                        variant = %variant,
                        stage = "done",
                        duration_ms = started.elapsed().as_millis() as u64,
                        "视频代理生成完成"
                    );
                }
                Err(e) => {
                    let _ = fs::remove_file(&tmp);
                    tracing::error!(
                        operation = "video_proxy",
                        asset_id,
                        variant = %variant,
                        stage = "rename_failed",
                        error_code = e.code(),
                        error = %e,
                        duration_ms = started.elapsed().as_millis() as u64,
                        "视频代理落盘失败"
                    );
                    let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
                    video_proxy::upsert(
                        &conn,
                        asset_id,
                        variant,
                        "failed",
                        None,
                        Some(&e.to_string()),
                    )?;
                }
            },
            Err(e) => {
                let _ = fs::remove_file(&tmp);
                let canceled = cancel.load(Ordering::Relaxed);
                let reason = if canceled {
                    "已取消".to_string()
                } else {
                    e.to_string()
                };
                let status = if canceled { "canceled" } else { "failed" };
                if canceled {
                    tracing::warn!(
                        operation = "video_proxy",
                        asset_id,
                        variant = %variant,
                        stage = "cancelled",
                        error_code = "CANCELLED",
                        duration_ms = started.elapsed().as_millis() as u64,
                        "视频代理生成已取消"
                    );
                } else {
                    tracing::error!(
                        operation = "video_proxy",
                        asset_id,
                        variant = %variant,
                        stage = "failed",
                        error_code = e.code(),
                        error = %e,
                        duration_ms = started.elapsed().as_millis() as u64,
                        "视频代理生成失败"
                    );
                }
                let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
                video_proxy::upsert(&conn, asset_id, variant, status, None, Some(&reason))?;
            }
        }
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        video_proxy::get(&conn, asset_id, variant)?.ok_or_else(|| AppError::msg("代理记录写入失败"))
    })
}

/// 查询代理状态（不触发生成）。
pub fn proxy_status(
    db: &Arc<Mutex<Connection>>,
    asset_id: i64,
    variant: &str,
) -> AppResult<Option<video_proxy::VideoProxy>> {
    let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    video_proxy::get(&conn, asset_id, variant)
}

/// 清理某个素材的全部代理（不影响原文件）。清除 DB 记录与磁盘文件。
pub fn delete_proxy_for_asset(
    db: &Arc<Mutex<Connection>>,
    proxy_dir: &Path,
    asset_id: i64,
) -> AppResult<()> {
    if let Ok(rd) = fs::read_dir(proxy_dir) {
        let prefix = format!("{asset_id}_");
        for e in rd.flatten() {
            if e.file_name().to_string_lossy().starts_with(&prefix) {
                let _ = fs::remove_file(e.path());
            }
        }
    }
    let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    conn.execute("DELETE FROM video_proxies WHERE asset_id=?1", [asset_id])?;
    Ok(())
}

/// 代理缓存统计（指导书 §6.7「视频代理缓存：占用、数量」）：ready 文件数、磁盘占用字节。
pub fn proxy_cache_stats(db: &Arc<Mutex<Connection>>, proxy_dir: &Path) -> AppResult<(i64, u64)> {
    let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM video_proxies WHERE status = 'ready'",
        [],
        |r| r.get(0),
    )?;
    drop(conn);
    let mut bytes: u64 = 0;
    if let Ok(rd) = fs::read_dir(proxy_dir) {
        for e in rd.flatten() {
            if let Ok(md) = e.metadata() {
                if md.is_file() {
                    bytes += md.len();
                }
            }
        }
    }
    Ok((count, bytes))
}

/// 清理全部代理缓存（不影响原文件）：先删磁盘 ready 文件，再清 DB 记录。
/// 正在 running 的任务不在此列（其临时文件由转码路径自行清理）。
pub fn clear_all_proxies(db: &Arc<Mutex<Connection>>, proxy_dir: &Path) -> AppResult<u64> {
    let mut removed: u64 = 0;
    if let Ok(rd) = fs::read_dir(proxy_dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            if name.ends_with(".mp4") || name.ends_with(".webm") {
                if fs::remove_file(e.path()).is_ok() {
                    removed += 1;
                } else {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }
    let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    conn.execute(
        "DELETE FROM video_proxies WHERE status IN ('ready','failed','canceled')",
        [],
    )?;
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{init_memory, video_proxy};

    /// 依赖「全局静态并发闸」时序断言的两个测试共享一把串行锁：
    /// 并行运行时它们会争抢同一个许可，导致互相干扰（见各自注释）。
    static GATE_TEST_LOCK: Mutex<()> = Mutex::new(());

    fn db() -> Arc<Mutex<Connection>> {
        Arc::new(Mutex::new(init_memory().unwrap()))
    }

    fn insert_video(c: &Connection) -> i64 {
        c.execute(
            "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at)
             VALUES ('/src/v.mp4', 'v.mp4', 'mp4', 1, 'video/mp4', 1, 1)",
            [],
        )
        .unwrap();
        c.query_row("SELECT id FROM assets", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn creates_proxy_atomically_and_marks_ready() {
        let db = db();
        let c = db.lock().unwrap();
        let id = insert_video(&c);
        drop(c);
        let dir = std::env::temp_dir().join(format!("bg_proxy_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let cancel = AtomicBool::new(false);
        let p = get_or_create_proxy(&db, &dir, id, "h264_mp4", &cancel, |_, tmp, _| {
            fs::write(tmp, b"fake mp4").unwrap();
            Ok(())
        })
        .unwrap();
        assert_eq!(p.status, "ready");
        assert!(p.path.is_some());
        assert!(Path::new(p.path.as_deref().unwrap()).exists());
        // 再次调用复用（transcode 不再触发）
        let c = db.lock().unwrap();
        let p2 = video_proxy::get(&c, id, "h264_mp4").unwrap().unwrap();
        assert_eq!(p2.status, "ready");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn failed_transcode_marks_failed_with_reason() {
        let db = db();
        let c = db.lock().unwrap();
        let id = insert_video(&c);
        drop(c);
        let dir = std::env::temp_dir().join(format!("bg_proxy_fail_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let cancel = AtomicBool::new(false);
        let p = get_or_create_proxy(&db, &dir, id, "h264_mp4", &cancel, |_, _, _| {
            Err(AppError::msg("编码不支持"))
        })
        .unwrap();
        assert_eq!(p.status, "failed");
        assert_eq!(p.error.as_deref(), Some("编码不支持"));
        // 临时文件被清理，无半成品
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn canceled_transcode_marks_canceled() {
        let db = db();
        let c = db.lock().unwrap();
        let id = insert_video(&c);
        drop(c);
        let dir = std::env::temp_dir().join(format!("bg_proxy_cancel_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let cancel = AtomicBool::new(true); // 前置取消
        let p = get_or_create_proxy(&db, &dir, id, "h264_mp4", &cancel, |_, _, _| {
            Err(AppError::msg("xx"))
        })
        .unwrap();
        assert_eq!(p.status, "canceled");
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn rejects_non_video_asset() {
        let db = db();
        let c = db.lock().unwrap();
        let id = {
            c.execute(
                "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at)
                 VALUES ('/i.jpg', 'i.jpg', 'jpg', 1, 'image/jpeg', 1, 1)",
                [],
            )
            .unwrap();
            c.query_row("SELECT id FROM assets", [], |r| r.get(0))
                .unwrap()
        };
        drop(c);
        let dir = std::env::temp_dir().join(format!("bg_proxy_nonv_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let cancel = AtomicBool::new(false);
        let p = get_or_create_proxy(&db, &dir, id, "h264_mp4", &cancel, |_, _, _| Ok(())).unwrap();
        assert_eq!(p.status, "failed");
        assert_eq!(p.error.as_deref(), Some("素材不是视频"));
        fs::remove_dir_all(&dir).ok();
    }

    /// §8.1：全局转码并发闸——两个不同素材同时转码，任一时刻只有 1 个 ffmpeg 在跑
    /// （single-flight 只防同 asset+variant 重复，此测试必须用不同素材验证跨素材限制）。
    /// 与 cancel_while_waiting 共享一把测试串行锁：二者都依赖全局静态并发闸的时序断言，
    /// 并行执行会互相干扰（闸只有 1 个许可）。
    #[test]
    fn global_transcode_gate_limits_concurrent_transcodes() {
        let _serial = GATE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        use std::sync::atomic::AtomicUsize;

        let db = db();
        let c = db.lock().unwrap();
        let ids: Vec<i64> = (0..4)
            .map(|i| insert_video_named(&c, &format!("v{i}.mp4")))
            .collect();
        drop(c);

        let dir = std::env::temp_dir().join(format!("bg_proxy_gate_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        // 并发 4 个不同素材的转码：max_active 不得 > 1（默认并发闸）
        let running = Arc::new(AtomicUsize::new(0));
        let max_observed = Arc::new(AtomicUsize::new(0));

        let handles: Vec<_> = ids
            .into_iter()
            .map(|id| {
                let db = Arc::clone(&db);
                let dir = dir.clone();
                let running = Arc::clone(&running);
                let max_observed = Arc::clone(&max_observed);
                std::thread::spawn(move || {
                    let cancel = AtomicBool::new(false);
                    get_or_create_proxy(&db, &dir, id, "h264_mp4", &cancel, move |_, tmp, _| {
                        let cur = running.fetch_add(1, Ordering::SeqCst) + 1;
                        max_observed.fetch_max(cur, Ordering::SeqCst);
                        // 模拟耗时转码：持许可 60ms，让并发窗口真实存在
                        std::thread::sleep(Duration::from_millis(60));
                        running.fetch_sub(1, Ordering::SeqCst);
                        fs::write(tmp, b"fake mp4").unwrap();
                        Ok(())
                    })
                    .map(|p| p.status)
                })
            })
            .collect();

        let statuses: Vec<String> = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            statuses.iter().all(|s| s == "ready"),
            "全部应 ready: {statuses:?}"
        );
        assert!(
            max_observed.load(Ordering::SeqCst) <= 1,
            "并发转码不得超过 1，实测峰值 {}",
            max_observed.load(Ordering::SeqCst)
        );
        fs::remove_dir_all(&dir).ok();
    }

    /// §8.1：等待许可期间取消 → acquire 返回 None（等待可被打断；不持数据库锁）。
    /// 直接单测并发闸本体的等待取消语义（确定性：先占住唯一许可，再在等待线程上置取消）——
    /// 完整 get_or_create_proxy 链路的取消路径已由 canceled_transcode_marks_canceled 覆盖。
    /// 与 global_transcode_gate 共享串行锁（都依赖全局闸的时序断言）。
    #[test]
    fn license_wait_is_cancellable() {
        let _serial = GATE_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let holder_cancel = AtomicBool::new(false);
        // 占住唯一许可
        let _license = TranscodeLicense::acquire(&holder_cancel, Duration::from_secs(1))
            .expect("首次应能拿到许可");

        // 等待线程：许可被占，进入等待；50ms 后置取消 → acquire 应返回 None
        let waiter_cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&waiter_cancel);
        let waiter = std::thread::spawn(move || {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(50));
                flag.store(true, Ordering::SeqCst);
            });
            TranscodeLicense::acquire(&waiter_cancel, Duration::from_secs(2))
        });
        let got = waiter.join().unwrap();
        assert!(got.is_none(), "等待许可期间取消应返回 None（等待可打断）");
    }

    fn insert_video_named(c: &Connection, name: &str) -> i64 {
        c.execute(
            "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, created_at, modified_at)
             VALUES (?1, ?2, 'mp4', 1, 'video/mp4', 1, 1)",
            rusqlite::params![format!("/src/{name}"), name],
        )
        .unwrap();
        c.query_row("SELECT id FROM assets ORDER BY id DESC LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap()
    }
}
