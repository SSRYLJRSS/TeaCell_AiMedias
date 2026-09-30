use tauri::State;

use crate::commands::assets_cmd;
use crate::db::reset as db_reset;
use crate::db::settings::{self, Settings};
use crate::error::{AppError, AppResult};
use crate::services::backup_restore;
use crate::state::AppState;

const HELP_PAGE_URL: &str =
    "https://my.feishu.cn/wiki/RGLmw1ExbiNcVvkk240cXJxjnmf?from=from_copylink";
const AGNES_API_KEY_DOCS_URL: &str = "https://platform.agnes-ai.com/";
const PROJECT_URL: &str = "https://github.com/SSRYLJRSS/TeaCell_AiMedias";
const FEEDBACK_URL: &str = "https://github.com/SSRYLJRSS/TeaCell_AiMedias/issues/new";
const LICENSE_URL: &str = "https://github.com/SSRYLJRSS/TeaCell_AiMedias/blob/main/LICENSE";
const AUTHOR_URL: &str = "https://www.xiaohongshu.com/user/profile/68294317000000000e01ca6d";

#[tauri::command]
pub fn get_settings(state: State<AppState>) -> AppResult<Settings> {
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    settings::get_settings(&conn)
}

#[tauri::command]
pub fn save_settings(state: State<AppState>, s: Settings) -> AppResult<()> {
    let _config_guard = state
        .ai_config_guard
        .lock()
        .map_err(|_| AppError::msg("AI 配置锁中毒"))?;
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    let has_running_ai_batch = !state
        .ai_cancel
        .lock()
        .map_err(|_| AppError::msg("AI 任务锁中毒"))?
        .is_empty();
    if has_running_ai_batch {
        let current = settings::get_settings(&conn)?;
        let prompt_changed =
            s.ai.confidence_min_suggest.to_bits() != current.ai.confidence_min_suggest.to_bits();
        let video_config_changed = s.ai.video_tagging != current.ai.video_tagging
            || s.ai.video_tagging_mode != current.ai.video_tagging_mode
            || s.ai.video_frame_count != current.ai.video_frame_count;
        if prompt_changed || video_config_changed {
            return Err(AppError::conflict(
                "AI 打标任务运行期间不能修改置信度或视频打标配置，请等待任务结束后重试。",
            ));
        }
    }
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
pub fn get_data_dir(state: State<AppState>) -> AppResult<String> {
    crate::utils::path::encode_native_path(&state.data_dir)
}

/// 在系统文件管理器中打开数据目录
#[tauri::command]
pub fn open_data_dir(app: tauri::AppHandle, state: State<AppState>) -> AppResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let data_dir = crate::utils::path::encode_native_path(&state.data_dir)?;
    app.opener()
        .open_path(&data_dir, None::<&str>)
        .map_err(|e| AppError::msg(format!("打开文件夹失败: {e}")))
}

/// W0-9：打开日志目录（data_dir/logs，tracing-appender 滚动文件所在处）
#[tauri::command]
pub fn open_logs_dir(app: tauri::AppHandle, state: State<AppState>) -> AppResult<()> {
    use tauri_plugin_opener::OpenerExt;
    let logs_dir = state.data_dir.join("logs");
    let logs_dir = crate::utils::path::encode_native_path(&logs_dir)?;
    app.opener()
        .open_path(&logs_dir, None::<&str>)
        .map_err(|e| AppError::msg(format!("打开日志目录失败: {e}")))
}

/// 打开使用帮助（固定飞书文档；通过系统默认浏览器访问）。
#[tauri::command]
pub fn open_help_page(app: tauri::AppHandle) -> AppResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(HELP_PAGE_URL, None::<&str>)
        .map_err(|e| AppError::msg(format!("打开使用帮助失败: {e}")))
}

/// 返回与系统默认浏览器打开动作相同的使用帮助地址，供失败时手动复制。
#[tauri::command]
pub fn get_help_page_url() -> String {
    HELP_PAGE_URL.to_string()
}

/// 打开固定的公开项目主页。
#[tauri::command]
pub fn open_project_page(app: tauri::AppHandle) -> AppResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(PROJECT_URL, None::<&str>)
        .map_err(|e| AppError::msg(format!("打开项目主页失败: {e}")))
}

#[tauri::command]
pub fn get_project_page_url() -> String {
    PROJECT_URL.to_string()
}

#[tauri::command]
pub fn open_license_page(app: tauri::AppHandle) -> AppResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(LICENSE_URL, None::<&str>)
        .map_err(|e| AppError::msg(format!("打开许可证页面失败: {e}")))
}

#[tauri::command]
pub fn get_license_page_url() -> String {
    LICENSE_URL.to_string()
}

/// 打开作者的小红书主页（使用稳定主页地址，不带分享令牌）。
#[tauri::command]
pub fn open_author_page(app: tauri::AppHandle) -> AppResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(AUTHOR_URL, None::<&str>)
        .map_err(|e| AppError::msg(format!("打开作者主页失败: {e}")))
}

#[tauri::command]
pub fn get_author_page_url() -> String {
    AUTHOR_URL.to_string()
}

/// 打开固定的 GitHub 新建问题页面。
#[tauri::command]
pub fn open_feedback_page(app: tauri::AppHandle) -> AppResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(FEEDBACK_URL, None::<&str>)
        .map_err(|e| AppError::msg(format!("打开反馈页面失败: {e}")))
}

#[tauri::command]
pub fn get_feedback_page_url() -> String {
    FEEDBACK_URL.to_string()
}

/// 在系统默认浏览器打开 Agnes 官方 API Key 申请与接入说明。
#[tauri::command]
pub fn open_agnes_api_key_docs(app: tauri::AppHandle) -> AppResult<()> {
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_url(AGNES_API_KEY_DOCS_URL, None::<&str>)
        .map_err(|e| AppError::msg(format!("打开 Agnes API Key 说明失败: {e}")))
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
    // DB 行、keyring 与后置缓存/日志文件清理分别由协调器排序：keyring 不在
    // Database guard 内，文件清理在事务提交并释放 DB guard 后执行。
    let mut report = tauri::async_runtime::spawn_blocking(move || {
        let (report, clear_cache, clear_logs) = if db_selection.ai_connections {
            let _operations = crate::services::credentials::lock_operations()?;
            let credential_refs = {
                let conn = db.lock()?;
                crate::db::ai_connections::list(&conn)?
                    .into_iter()
                    .filter_map(|connection| connection.api_key_ref)
                    .collect::<Vec<_>>()
            };
            crate::services::credentials::delete_many_with_commit(
                &crate::services::credentials::SystemCredentialBackend,
                &credential_refs,
                || {
                    let mut conn = db.lock()?;
                    db_reset::reset_db(&mut conn, &db_selection)
                },
            )?
        } else {
            let mut conn = db.lock()?;
            db_reset::reset_db(&mut conn, &db_selection)?
        };
        // The credential-operation guard from the branch above has been dropped;
        // neither the DB mutex nor a credential lock spans cache/log filesystem IO.
        db_reset::finish_reset(&data_dir, report, clear_cache, clear_logs)
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

/// W5c：恢复数据库（指导书 §W5c）。备份校验和同目录暂存由 service 完成；
/// 独占数据库生命周期门期间只短暂持连接 mutex，文件交换/迁移完成后请求应用重启。
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
    // ③ 先在当前库仍可用时准备/复验暂存副本，再取得独占 lifecycle 门。
    // 门内二次检查任务，避免预检后新任务抢先开始；连接 mutex 不跨文件 IO。
    let db = std::sync::Arc::clone(&state.db);
    let data_dir = state.data_dir.clone();
    let import_running = std::sync::Arc::clone(&state.import_running);
    let refill_running = std::sync::Arc::clone(&state.refill_running);
    let export_cancel = std::sync::Arc::clone(&state.export_cancel);
    let ai_cancel = std::sync::Arc::clone(&state.ai_cancel);
    tauri::async_runtime::spawn_blocking(move || -> AppResult<()> {
        let prepared = backup_restore::prepare_restore(&data_dir, &source)?;
        let maintenance = db.maintenance()?;
        maintenance.with_connection(|conn| {
            guard_no_running_tasks_with(
                &import_running,
                &refill_running,
                &export_cancel,
                &ai_cancel,
                conn,
                "恢复备份",
                false,
            )
        })?;
        backup_restore::install_prepared_restore(&maintenance, prepared)
    })
    .await
    .map_err(|e| AppError::msg(format!("恢复线程异常: {e}")))??;
    // ⑥ 冷启动衔接：恢复成功即重启进程加载新库（restart 不返回）
    app.restart()
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
    guard_no_running_tasks_with(
        &state.import_running,
        &state.refill_running,
        &state.export_cancel,
        &state.ai_cancel,
        conn,
        action,
        allow_pending_ai_batches,
    )
}

fn guard_no_running_tasks_with(
    import_running: &std::sync::atomic::AtomicBool,
    refill_running: &std::sync::atomic::AtomicBool,
    export_cancel: &std::sync::Mutex<
        std::collections::HashMap<i64, std::sync::Arc<std::sync::atomic::AtomicBool>>,
    >,
    ai_cancel: &std::sync::Mutex<
        std::collections::HashMap<i64, std::sync::Arc<std::sync::atomic::AtomicBool>>,
    >,
    conn: &rusqlite::Connection,
    action: &str,
    allow_pending_ai_batches: bool,
) -> AppResult<()> {
    if import_running.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(AppError::msg(format!(
            "文件入库进行中，请等它结束或取消后再{action}"
        )));
    }
    if refill_running.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(AppError::msg(format!(
            "回填/色板任务进行中，请等它结束或取消后再{action}"
        )));
    }
    if !export_cancel
        .lock()
        .map_err(|_| AppError::msg("锁中毒"))?
        .is_empty()
    {
        return Err(AppError::msg(format!(
            "导出任务进行中，请等它结束或取消后再{action}"
        )));
    }
    if !ai_cancel
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
}
