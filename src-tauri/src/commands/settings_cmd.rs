use tauri::State;

use crate::commands::assets_cmd;
use crate::db::reset as db_reset;
use crate::db::settings::{self, Settings};
use crate::error::{AppError, AppResult};
use crate::state::AppState;

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> AppResult<Settings> {
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    settings::get_settings(&conn)
}

#[tauri::command]
pub fn save_settings(state: State<AppState>, s: Settings) -> AppResult<()> {
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    settings::save_settings(&conn, &s)?;
    drop(conn);
    if let Err(e) = crate::observability::set_log_level(&s.log_level) {
        // 设置已落库；宿主未初始化 subscriber 或 reload 失败时不能把保存结果伪装成失败。
        tracing::warn!("设置已保存，但日志级别未能立即切换: {e}");
    }
    Ok(())
}

/// 软件数据保存位置（R-33：数据库/缩略图所在目录，便于备份转移）
#[tauri::command]
pub fn get_data_dir(state: State<AppState>) -> String {
    state.data_dir.to_string_lossy().into_owned()
}

/// 在系统文件管理器中打开数据目录
#[tauri::command]
pub fn open_data_dir(app: tauri::AppHandle, state: State<AppState>) -> AppResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(state.data_dir.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|e| AppError::msg(format!("打开文件夹失败: {e}")))
}

/// W0-9：打开日志目录（data_dir/logs，tracing-appender 滚动文件所在处）
#[tauri::command]
pub fn open_logs_dir(app: tauri::AppHandle, state: State<AppState>) -> AppResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let logs_dir = state.data_dir.join("logs");
    app.opener()
        .open_path(logs_dir.to_string_lossy().as_ref(), None::<&str>)
        .map_err(|e| AppError::msg(format!("打开日志目录失败: {e}")))
}

/// 分类重置应用数据（设置页「存储与维护 → 重置数据」勾选传入）。
/// 原始素材文件选项会先复用删除命令的锁外文件流程；磁盘删除失败的素材记录会保留，
/// 其它数据库项仍按所选分类在事务内完成。
#[tauri::command]
pub async fn reset_app_data(
    state: State<'_, AppState>,
    selection: db_reset::ResetSelection,
) -> AppResult<db_reset::ResetReport> {
    if !selection.any() {
        return Err(AppError::invalid_arg("请先勾选要重置的数据"));
    }
    // 会改数据库或删除素材文件的项需要避开入库/回填/导出/AI 批次；
    // 只清前端搜索草稿或诊断日志不影响后台任务，可直接执行。
    if selection_requires_task_guard(&selection) {
        let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        guard_reset_no_running_tasks(&state, &conn, &selection)?;
    }

    // 原始文件删除必须在数据库锁外执行，并复用素材删除命令的 B03 语义：
    // 成功删除的文件连同记录一起移除，失败项保留记录供用户修复占用后重试。
    let asset_file_result = if selection.asset_files {
        let db = std::sync::Arc::clone(&state.db);
        let data_dir = state.data_dir.clone();
        Some(
            tauri::async_runtime::spawn_blocking(move || {
                assets_cmd::delete_all_asset_files_for_reset(db, data_dir)
            })
            .await
            .map_err(|e| AppError::msg(format!("原始文件删除线程异常: {e}")))??,
        )
    } else {
        None
    };

    // 文件删除流程已经清理了成功项的素材记录；若“素材库记录”也被勾选，
    // 不再用全表 DELETE 覆盖文件删除失败项，避免数据库失去重试线索。
    let mut db_selection = selection.clone();
    if selection.asset_files {
        db_selection.assets = false;
        db_selection.asset_files = false;
        // 原文件对应的缩略图/预览/代理均为派生数据，随本次高风险操作一并清理。
        db_selection.caches = true;
    }
    let db = std::sync::Arc::clone(&state.db);
    let data_dir = state.data_dir.clone();
    // 删大缓存目录是文件 IO 重活，spawn_blocking 防堵主线程
    let mut report = tauri::async_runtime::spawn_blocking(move || {
        let mut conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        db_reset::reset(&mut conn, &data_dir, &db_selection)
    })
    .await
    .map_err(|e| AppError::msg(format!("重置线程异常: {e}")))??;

    if let Some(file_result) = asset_file_result {
        report.asset_files_deleted = file_result.deleted;
        report.asset_files_failed = file_result.failed_files.len() as u64;
        // 成功删除原文件时，素材记录也已由 delete_assets_blocking 删除；
        // 合并进统一报告，前端可展示一次完整的“素材”数量。
        report.assets_deleted += file_result.deleted as i64;
    }

    Ok(report)
}

fn selection_requires_task_guard(selection: &db_reset::ResetSelection) -> bool {
    selection.assets
        || selection.asset_files
        || selection.export_tasks
        || selection.tags
        || selection.ai_tasks
        || selection.ai_connections
        || selection.preferences
        || selection.caches
}

/// W5c：备份数据库（指导书 §W5c）。短锁内 `VACUUM INTO` 生成单文件快照。
#[tauri::command]
pub async fn backup_db(state: State<'_, AppState>, target: String) -> AppResult<()> {
    let db = std::sync::Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        crate::db::backup::backup_to(&conn, std::path::Path::new(&target))
    })
    .await
    .map_err(|e| AppError::msg(format!("备份线程异常: {e}")))?
}

/// W5c：恢复数据库（指导书 §W5c）。
/// 校验（quick_check + user_version 只拒高版本 + 关键表）→ 运行中任务阻断
/// → 现库 `.old` 保底 → 覆盖 → 迁移升级 → 热替换连接 → `app.restart()`（不返回）。
#[tauri::command]
pub async fn restore_db(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    source: String,
) -> AppResult<()> {
    let source = std::path::PathBuf::from(&source);
    // ① 校验备份（锁外；失败直接给用户可读原因）
    crate::db::backup::validate_backup(&source)?;
    // ② 运行中任务阻断（含冷启动自愈后仍可靠的 ai_batches 检查：启动时 processing 已被标记 interrupted）
    {
        let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        guard_no_running_tasks(&state, &conn, "恢复备份", false)?;
    }
    // ③ 换文件 + 换连接（持锁；复制与迁移是文件 IO 重活，spawn_blocking）
    let db = std::sync::Arc::clone(&state.db);
    let data_dir = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || -> AppResult<()> {
        let mut guard = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        perform_restore(&mut guard, &data_dir, &source)
    })
    .await
    .map_err(|e| AppError::msg(format!("恢复线程异常: {e}")))??;
    // ⑥ 冷启动衔接：恢复成功即重启进程加载新库（restart 不返回）
    app.restart()
}

/// 恢复核心（B4/X-09）：换文件 + 换连接，纯文件系统 + 连接操作，可用临时库做故障注入测试。
///
/// 不变量：函数返回时（无论成功或失败），只要磁盘上的 `library.db` 是完好的，`guard` 就必须
/// 持有指向它的**可验证真实连接**，绝不把空的内存占位连接当作真实库留下；只有连真实库都打不开
/// 时才保留占位连接并返回明确错误（提示重启/进入恢复），文案不得谎报“已还原”。
fn perform_restore(
    guard: &mut rusqlite::Connection,
    data_dir: &std::path::Path,
    source: &std::path::Path,
) -> AppResult<()> {
    let db_path = data_dir.join("library.db");
    let old_path = data_dir.join("library.db.old");

    // 磁盘 library.db 完好时，重开可验证连接放回 guard；返回还原状态便于拼装用户可读文案。
    fn reopen_into(guard: &mut rusqlite::Connection, path: &std::path::Path) -> Result<(), String> {
        match crate::db::init(path) {
            Ok(conn) => {
                *guard = conn;
                Ok(())
            }
            Err(e) => Err(e.to_string()),
        }
    }

    // 旧连接收尾：checkpoint 截断 WAL → 换入内存占位连接 → 关闭旧连接释放文件句柄
    //（Windows 上文件被占用时 rename/copy 会失败，必须先关）
    let old = std::mem::replace(
        &mut *guard,
        rusqlite::Connection::open_in_memory()
            .map_err(|e| AppError::msg(format!("占位连接创建失败: {e}")))?,
    );
    // checkpoint 是尽力而为（失败不阻断），但要留痕而非静默吞掉。
    if let Err(e) = old.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);") {
        tracing::warn!("恢复备份：旧库 WAL checkpoint 失败（继续收尾）：{e}");
    }
    // close 失败意味着文件句柄可能未释放，Windows 上后续 rename/copy 会连带失败——必须留痕。
    if let Err((conn, e)) = old.close() {
        drop(conn);
        tracing::warn!("恢复备份：旧库连接关闭失败（文件句柄可能未释放）：{e}");
    }

    // ④ .old 保底：覆盖失败/新库打不开时能回滚回现库
    if old_path.exists() {
        let _ = std::fs::remove_file(&old_path);
    }
    // 改名保底失败：磁盘上的 library.db 原封未动，重开它放回 guard 后再报错。
    if let Err(e) = std::fs::rename(&db_path, &old_path) {
        let reopen = reopen_into(guard, &db_path);
        return Err(AppError::msg(match reopen {
            Ok(()) => format!("现库改名保底失败（未改动原库，已继续使用原库）：{e}"),
            Err(re) => {
                format!("现库改名保底失败：{e}；且原库重新打开失败：{re}。请重启应用进入恢复。")
            }
        }));
    }
    // 覆盖失败：把 .old 还原回 library.db，再重开它放回 guard。
    if let Err(e) = std::fs::copy(source, &db_path) {
        let _ = std::fs::remove_file(&db_path);
        let restored = std::fs::rename(&old_path, &db_path);
        let msg = match (restored, reopen_into(guard, &db_path)) {
            (Ok(()), Ok(())) => format!("覆盖库文件失败（已还原原库）：{e}"),
            (Ok(()), Err(re)) => format!(
                "覆盖库文件失败：{e}；原库文件已还原但重新打开失败：{re}。请重启应用进入恢复。"
            ),
            (Err(rre), _) => format!(
                "覆盖库文件失败：{e}；且原库还原失败：{rre}。原库文件保留在 {}，请勿覆盖并联系支持。",
                old_path.display()
            ),
        };
        return Err(AppError::msg(msg));
    }
    // ⑤ 打开新库（老版本备份在此自动迁移升级）
    match crate::db::init(&db_path) {
        Ok(new_conn) => {
            *guard = new_conn;
        }
        Err(e) => {
            // 回滚：新库打不开 → 还原 .old（保证应用重启后仍是原库）
            let _ = std::fs::remove_file(&db_path);
            let restored = std::fs::rename(&old_path, &db_path);
            let msg = match (restored, reopen_into(guard, &db_path)) {
                (Ok(()), Ok(())) => format!("恢复后的库无法打开（已还原原库）：{e}"),
                (Ok(()), Err(re)) => format!(
                    "恢复后的库无法打开：{e}；原库文件已还原但重新打开失败：{re}。请重启应用进入恢复。"
                ),
                (Err(rre), _) => format!(
                    "恢复后的库无法打开：{e}；且原库还原失败：{rre}。原库文件保留在 {}，请勿覆盖并联系支持。",
                    old_path.display()
                ),
            };
            return Err(AppError::msg(msg));
        }
    }
    Ok(())
}

/// 重置前运行中任务守卫。
///
/// 用户勾选“AI 打标任务”时，遗留的 pending/processing 批次正是本次要删除的对象，
/// 不能因为数据库里存在这些批次就反过来阻止删除；真正执行中的批次仍由 ai_cancel 注册表拦截。
fn guard_reset_no_running_tasks(
    state: &AppState,
    conn: &rusqlite::Connection,
    selection: &db_reset::ResetSelection,
) -> AppResult<()> {
    guard_no_running_tasks(state, conn, "重置数据", selection.ai_tasks)
}

/// 运行中任务守卫：入库 / 回填类 / 导出 / AI 批次任一进行中即拒绝。
///
/// `allow_pending_ai_batches` 仅用于重置并明确选择删除 AI 任务的场景；恢复备份始终为 false。
fn guard_no_running_tasks(
    state: &AppState,
    conn: &rusqlite::Connection,
    action: &str,
    allow_pending_ai_batches: bool,
) -> AppResult<()> {
    if state
        .import_running
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(AppError::msg(format!(
            "文件入库进行中，请等它结束或取消后再{action}"
        )));
    }
    if state
        .refill_running
        .load(std::sync::atomic::Ordering::Relaxed)
    {
        return Err(AppError::msg(format!(
            "回填/色板任务进行中，请等它结束或取消后再{action}"
        )));
    }
    if !state
        .export_cancel
        .lock()
        .map_err(|_| AppError::msg("锁中毒"))?
        .is_empty()
    {
        return Err(AppError::msg(format!(
            "导出任务进行中，请等它结束或取消后再{action}"
        )));
    }
    if !state
        .ai_cancel
        .lock()
        .map_err(|_| AppError::msg("锁中毒"))?
        .is_empty()
    {
        return Err(AppError::msg(format!(
            "AI 打标批次进行中，请等它结束或取消后再{action}"
        )));
    }
    if !allow_pending_ai_batches {
        let n = crate::db::ai::count_pending_or_processing(conn)?;
        if n > 0 {
            return Err(AppError::msg(format!(
                "有待处理的 AI 打标批次，请先取消批次后再{action}"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    use super::*;
    use crate::db::init_memory;

    fn state_with_pending_batch() -> AppState {
        let conn = init_memory().unwrap();
        conn.execute(
            "INSERT INTO ai_batches (status, mode, total, created_at)
             VALUES ('pending', 'cloud', 1, 1)",
            [],
        )
        .unwrap();
        AppState::new(conn, std::env::temp_dir())
    }

    #[test]
    fn reset_with_ai_tasks_selected_allows_and_deletes_pending_batches() {
        let state = state_with_pending_batch();
        let selection = db_reset::ResetSelection {
            ai_tasks: true,
            ..Default::default()
        };
        {
            let conn = state.db.lock().unwrap();
            guard_reset_no_running_tasks(&state, &conn, &selection).unwrap();
        }

        let mut conn = state.db.lock().unwrap();
        let report = db_reset::reset(&mut conn, &state.data_dir, &selection).unwrap();
        assert_eq!(report.ai_tasks_deleted, 1);
        assert_eq!(
            crate::db::ai::count_pending_or_processing(&conn).unwrap(),
            0
        );
    }

    #[test]
    fn reset_without_ai_tasks_selected_still_blocks_pending_batches() {
        let state = state_with_pending_batch();
        let conn = state.db.lock().unwrap();
        let err = guard_reset_no_running_tasks(
            &state,
            &conn,
            &db_reset::ResetSelection {
                tags: true,
                ..Default::default()
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("有待处理的 AI 打标批次"));
    }

    #[test]
    fn reset_with_ai_tasks_selected_still_blocks_running_batch() {
        let state = state_with_pending_batch();
        state
            .ai_cancel
            .lock()
            .unwrap()
            .insert(1, Arc::new(AtomicBool::new(false)));
        let conn = state.db.lock().unwrap();
        let err = guard_reset_no_running_tasks(
            &state,
            &conn,
            &db_reset::ResetSelection {
                ai_tasks: true,
                ..Default::default()
            },
        )
        .unwrap_err();

        assert!(err.to_string().contains("AI 打标批次进行中"));
    }

    // ---- B4/X-09：restore 故障注入（临时库，不碰用户真实库）----

    /// 判别真实库 vs 空内存占位：真实 library.db 有业务表，占位连接无表。
    fn table_count(conn: &rusqlite::Connection) -> i64 {
        conn.query_row(
            "SELECT count(*) FROM sqlite_master WHERE type='table'",
            [],
            |r| r.get(0),
        )
        .unwrap_or(-1)
    }

    /// 在目录里建一个真实库文件并返回其打开的连接（用作 guard 当前库）。
    fn make_library(dir: &std::path::Path) -> rusqlite::Connection {
        let p = dir.join("library.db");
        // 先建库释放句柄，再重新打开，模拟运行时 guard 持有的活动连接。
        crate::db::init(&p).unwrap();
        crate::db::init(&p).unwrap()
    }

    #[test]
    fn restore_happy_path_leaves_working_connection() {
        let data_dir = tempfile::tempdir().unwrap();
        let mut guard = make_library(data_dir.path());
        // 有效备份源：另建一个真实库文件，释放句柄后取其路径。
        let src_dir = tempfile::tempdir().unwrap();
        let src = src_dir.path().join("backup.db");
        drop(crate::db::init(&src).unwrap());

        perform_restore(&mut guard, data_dir.path(), &src).unwrap();
        // 成功后 guard 必须是指向新库的真实连接（有业务表）。
        assert!(table_count(&guard) > 0, "恢复成功后 guard 应为真实库连接");
    }

    #[test]
    fn restore_copy_failure_rolls_back_and_reopens_real_db() {
        let data_dir = tempfile::tempdir().unwrap();
        let mut guard = make_library(data_dir.path());
        let before = table_count(&guard);
        // 源不存在 → std::fs::copy 失败 → 必须还原 .old 并重开真实库。
        let missing = data_dir.path().join("does-not-exist.db");

        let err = perform_restore(&mut guard, data_dir.path(), &missing).unwrap_err();
        assert!(
            err.to_string().contains("已还原原库"),
            "文案应说明已还原：{err}"
        );
        // 关键不变量：guard 不是空占位，而是重开的真实原库。
        assert_eq!(table_count(&guard), before, "回滚后 guard 应重开原库");
        // .old 已被还原回 library.db，不应残留。
        assert!(!data_dir.path().join("library.db.old").exists());
        assert!(data_dir.path().join("library.db").exists());
    }

    #[test]
    fn restore_unopenable_new_db_rolls_back_and_reopens_real_db() {
        let data_dir = tempfile::tempdir().unwrap();
        let mut guard = make_library(data_dir.path());
        let before = table_count(&guard);
        // 源可复制但不是合法 sqlite（复制成功、db::init 失败）→ 回滚还原原库。
        let src_dir = tempfile::tempdir().unwrap();
        let garbage = src_dir.path().join("garbage.db");
        std::fs::write(&garbage, b"this is definitely not a sqlite database").unwrap();

        let err = perform_restore(&mut guard, data_dir.path(), &garbage).unwrap_err();
        assert!(
            err.to_string().contains("已还原原库"),
            "文案应说明已还原：{err}"
        );
        assert_eq!(table_count(&guard), before, "回滚后 guard 应重开原库");
        assert!(!data_dir.path().join("library.db.old").exists());
    }
}
