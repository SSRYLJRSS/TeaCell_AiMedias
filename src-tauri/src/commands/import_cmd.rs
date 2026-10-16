use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::contracts::agent::ResourceRef;
use crate::contracts::caller::CallerContext;
use crate::db::settings;
use crate::error::{AppError, AppResult};
use crate::services::authorization::{self as auth_service, TrustedAdapterIdentity};
use crate::services::import_prepare::{
    self, ImportPlanVerification, ImportPlanView, PrepareImportRequest,
};
use crate::services::importer::{self, ImportProgress};
use crate::services::{import_execute, media_refill, thumbnail::ThumbnailService};
use crate::state::AppState;

fn gui_caller(state: &AppState) -> AppResult<CallerContext> {
    let conn = state.db.lock()?;
    auth_service::build_caller_context(
        &conn,
        TrustedAdapterIdentity::Gui {
            session_id: state.authorization_session_id.clone(),
            instance_id: state.authorization_instance_id.clone(),
        },
        ResourceRef::generated("request"),
        Vec::new(),
    )
}

struct ImportOperationGuard {
    running: Arc<AtomicBool>,
}

impl ImportOperationGuard {
    fn acquire(state: &AppState) -> AppResult<Self> {
        state
            .import_running
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Relaxed)
            .map_err(|_| AppError::conflict("已有文件检查或入库任务正在运行"))?;
        Ok(Self {
            running: Arc::clone(&state.import_running),
        })
    }
}

impl Drop for ImportOperationGuard {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
    }
}

fn validate_task_id(task_id: &str) -> AppResult<()> {
    if task_id.is_empty()
        || task_id.len() > 128
        || !task_id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(AppError::invalid_arg("导入任务 ID 不合法"));
    }
    Ok(())
}

fn emit_import_terminal(app: &AppHandle, task_id: &str, phase: &str, message: String) {
    let _ = app.emit(
        "import://progress",
        ImportProgress {
            task_id: task_id.to_string(),
            operation_id: None,
            phase: phase.to_string(),
            phase_current: 0,
            phase_total: None,
            file: None,
            imported: 0,
            duplicates: 0,
            failed: 0,
            message: Some(message),
        },
    );
}

/// FB4-03（§6.4）：导入后置色板完成事件。只允许导入后置任务发送；手动设置页回算不发。
/// 发送条件（全部满足）：由已受理导入操作触发 + 成功抢到 refill gate +
/// 后台任务实际执行并返回 Ok(summary) + updatedIds 非空。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct PaletteUpdatedEvent {
    /// 固定 "import"（前端据此区分来源）
    source: &'static str,
    total: i64,
    success: i64,
    failed: i64,
    skipped: i64,
    updated_ids: Vec<i64>,
}

impl PaletteUpdatedEvent {
    /// 事件判定（§6.4/§10.7）：只在真实写库成功的素材非空时构造事件。
    /// 零更新（updated_ids 为空）→ None，不发送；gate 占用 / 扫描错误在调用点
    /// （Option<AppResult<RefillSummary>> 解包）就不进入本函数，天然不发。
    fn from_import_summary(s: &media_refill::RefillSummary) -> Option<Self> {
        if s.updated_ids.is_empty() {
            return None;
        }
        Some(Self {
            source: "import",
            total: s.total,
            success: s.success,
            failed: s.failed,
            skipped: s.skipped,
            updated_ids: s.updated_ids.clone(),
        })
    }
}

struct PrepareCancellationRegistration {
    registry: Arc<std::sync::Mutex<std::collections::HashMap<String, Arc<AtomicBool>>>>,
    key: String,
}

impl Drop for PrepareCancellationRegistration {
    fn drop(&mut self) {
        if let Ok(mut entries) = self.registry.lock() {
            entries.remove(&self.key);
        }
    }
}

fn register_prepare_cancellation(
    state: &AppState,
    task_id: &str,
) -> AppResult<(Arc<AtomicBool>, PrepareCancellationRegistration)> {
    let cancel = Arc::new(AtomicBool::new(false));
    let registry = Arc::clone(&state.import_prepare_cancels);
    let key = task_id.to_string();
    {
        let mut entries = registry
            .lock()
            .map_err(|_| AppError::internal("准备任务锁中毒"))?;
        if entries.contains_key(&key) {
            return Err(AppError::conflict("准备任务 ID 已在使用"));
        }
        entries.insert(key.clone(), Arc::clone(&cancel));
    }
    Ok((cancel, PrepareCancellationRegistration { registry, key }))
}

fn schedule_import_palette_refill(app: AppHandle, state: &AppState) {
    let gate = Arc::clone(&state.refill_running);
    let db = Arc::clone(&state.db);
    let cancel = Arc::clone(&state.media_refill_cancel);
    tauri::async_runtime::spawn_blocking(move || {
        let ids = {
            let conn = match db.lock() {
                Ok(conn) => conn,
                Err(_) => {
                    tracing::warn!(
                        operation = "import_palette_refill",
                        result = "skipped",
                        reason = "db_lock",
                        "入库后置色板补算跳过：数据库锁中毒"
                    );
                    return;
                }
            };
            match crate::db::assets::list_ids_needing_palette(&conn) {
                Ok(ids) => ids,
                Err(error) => {
                    tracing::warn!(
                        operation = "import_palette_refill",
                        result = "failed",
                        reason = "list_ids",
                        error = %error,
                        "入库后置色板补算失败：无法读取待补算素材"
                    );
                    return;
                }
            }
        };
        if ids.is_empty() {
            tracing::debug!(
                operation = "import_palette_refill",
                result = "skipped",
                reason = "no_missing",
                "入库后置色板补算跳过：没有缺失项"
            );
            return;
        }
        match media_refill::try_rescan_palette_exclusive(&gate, &db, &ids, &cancel, |_| {}) {
            Some(Ok(summary)) => {
                tracing::info!(
                    operation = "import_palette_refill",
                    result = "done",
                    total = summary.total,
                    success = summary.success,
                    skipped = summary.skipped,
                    failed = summary.failed,
                    "入库后置色板补算完成"
                );
                if let Some(event) = PaletteUpdatedEvent::from_import_summary(&summary) {
                    let _ = app.emit("palette://updated", event);
                }
            }
            Some(Err(error)) => tracing::warn!(
                operation = "import_palette_refill",
                result = "failed",
                error = %error,
                "入库后置色板补算结束但结果失败"
            ),
            None => tracing::warn!(
                operation = "import_palette_refill",
                result = "skipped",
                reason = "gate_busy",
                "入库后置色板补算跳过：已有回填任务运行"
            ),
        }
    });
}

/// 扫描路径生成待入库清单统计（两段式入库，不落库）
/// B07：改 async + spawn_blocking，大目录扫描不卡主线程
/// FB2-03：对清单内每个原文件逐路径放行 asset 协议，使入库页可在卡片内播放视频。
/// 安全边界与 list_assets 一致——只放行用户主动选择/拖入的路径，只读，不放宽 scope、不用 allow_directory。
#[tauri::command]
pub async fn inspect_import(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    task_id: String,
) -> AppResult<importer::ImportPlan> {
    if paths.is_empty() {
        return Err(AppError::invalid_arg("未选择任何文件"));
    }
    validate_task_id(&task_id)?;
    let _operation = ImportOperationGuard::acquire(&state)?;
    let (cancel, registration) = register_prepare_cancellation(&state, &task_id)?;
    let library_root = {
        let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        settings::get_settings(&conn)?.library_root
    };
    let task_id_for_worker = task_id.clone();
    let worker_app = app.clone();
    let plan = tauri::async_runtime::spawn_blocking(move || {
        let _registration = registration;
        importer::validate_library_root(&library_root)?;
        importer::inspect_paths_with_control(&paths, task_id_for_worker, &cancel, |progress| {
            let _ = worker_app.emit("import://progress", progress);
        })
    })
    .await
    .map_err(|e| AppError::msg(format!("扫描线程异常: {e}")))?;
    let plan = match plan {
        Ok(plan) => plan,
        Err(error) => {
            let phase = if error.code() == "CANCELLED" {
                "cancelled"
            } else {
                "failed"
            };
            emit_import_terminal(&app, &task_id, phase, error.to_string());
            return Err(error);
        }
    };
    for item in &plan.items {
        allow_import_asset(&app, &item.path);
    }
    Ok(plan)
}

/// 与 assets_cmd::allow_asset 同语义的本地放行函数（仅在 inspect_import 内使用）
fn allow_import_asset(app: &AppHandle, path: &str) {
    let _ = app
        .asset_protocol_scope()
        .allow_file(std::path::Path::new(path));
}

/// 改名模板预览（前端 RenameBuilder 实时预览用）：直调后端 render_name，
/// 消除前后端双实现的规则 drift（单一事实源）
#[tauri::command]
pub fn preview_rename(
    template: String,
    collection: String,
    orig_stem: String,
    seq: Option<usize>,
) -> String {
    importer::preview_rename(&template, &collection, &orig_stem, seq.unwrap_or(1))
}

/// 用系统默认应用打开待入库原文件（§10 FB-04：PeningItem 双击打开）。
/// 校验路径存在；仅允许打开，不做任何文件修改。
#[tauri::command]
pub fn open_file_external(app: tauri::AppHandle, path: String) -> AppResult<()> {
    if path.trim().is_empty() {
        return Err(AppError::invalid_arg("路径为空"));
    }
    let p = std::path::Path::new(&path);
    if !p.exists() {
        return Err(AppError::not_found(format!("文件不存在: {path}")));
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .open_path(&path, None::<&str>)
        .map_err(|e| AppError::msg(format!("系统打开文件失败: {e}")))
}

/// A2-01：把扫描/检查变成固定范围的准备计划。只读候选、分类与改名预览，不复制、
/// 不落正式资产、不外发；授权目录来源经 A1-03 目录句柄解析。
#[tauri::command]
pub async fn prepare_import(
    state: State<'_, AppState>,
    task_id: ResourceRef,
    request: PrepareImportRequest,
) -> AppResult<ImportPlanView> {
    let caller = gui_caller(&state)?;
    let key = task_id.as_str().to_string();
    let (cancel, registration) = register_prepare_cancellation(&state, &key)?;
    let db = Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || {
        let _registration = registration;
        import_prepare::prepare_import_with_cancel(&db, &caller, request, &cancel)
    })
    .await
    .map_err(|error| AppError::internal(format!("准备线程异常: {error}")))?
}

#[tauri::command]
pub fn cancel_prepare_import(state: State<AppState>, task_id: ResourceRef) -> AppResult<()> {
    let entries = state
        .import_prepare_cancels
        .lock()
        .map_err(|_| AppError::internal("准备任务锁中毒"))?;
    let cancel = entries
        .get(task_id.as_str())
        .ok_or_else(|| AppError::not_found("准备任务不存在或已结束"))?;
    cancel.store(true, Ordering::Relaxed);
    Ok(())
}

#[tauri::command]
pub fn get_import_plan(state: State<AppState>, plan_ref: ResourceRef) -> AppResult<ImportPlanView> {
    let caller = gui_caller(&state)?;
    import_prepare::get_import_plan(&state.db, &caller, &plan_ref)
}

/// Resolve ephemeral file paths for trusted GUI thumbnails/open actions.
/// Persistent and agent-facing import plan DTOs remain path-free.
#[tauri::command]
pub fn get_import_plan_preview_paths(
    state: State<AppState>,
    plan_ref: ResourceRef,
) -> AppResult<std::collections::BTreeMap<String, String>> {
    let caller = gui_caller(&state)?;
    import_prepare::preview_paths_for_plan(&state.db, &caller, &plan_ref)
}

/// Path-free durable result counts and per-item failures for the owning workbench.
#[tauri::command]
pub async fn get_import_result(
    state: State<'_, AppState>,
    operation_ref: ResourceRef,
) -> AppResult<import_execute::ImportResultView> {
    let caller = gui_caller(&state)?;
    let db = Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || {
        import_execute::import_result(&db, &caller, &operation_ref)
    })
    .await
    .map_err(|_| AppError::internal("导入结果查询线程异常"))?
}

#[tauri::command]
pub async fn verify_import_plan(
    state: State<'_, AppState>,
    plan_ref: ResourceRef,
) -> AppResult<ImportPlanVerification> {
    let caller = gui_caller(&state)?;
    let db = Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || {
        import_prepare::verify_import_plan(&db, &caller, &plan_ref)
    })
    .await
    .map_err(|_| AppError::internal("导入复检线程异常"))?
}

/// Accept once; return the durable operation immediately. Progress is a
/// projection of the operation, never an acknowledgement of completed files.
#[tauri::command]
pub async fn confirm_import(
    app: AppHandle,
    state: State<'_, AppState>,
    plan_ref: ResourceRef,
    idempotency_key: ResourceRef,
) -> AppResult<crate::contracts::agent::SubmissionResult> {
    let caller = gui_caller(&state)?;
    let db = Arc::clone(&state.db);
    let submitted = tauri::async_runtime::spawn_blocking(move || {
        crate::services::import_submission::confirm(&db, &caller, &plan_ref, &idempotency_key)
    })
    .await
    .map_err(|_| AppError::internal("导入受理线程异常"))??;
    if !submitted.replayed
        || matches!(
            submitted.operation.state,
            crate::contracts::agent::OperationState::Queued
        )
    {
        wake_import_queue(app);
    }
    Ok(submitted)
}

/// GUI wakeup only. The service owns claim/release and all business dispatch.
pub(crate) fn wake_import_queue(app: AppHandle) {
    tauri::async_runtime::spawn_blocking(move || {
        let state = app.state::<AppState>();
        // Coordinate with scans while accepted imports run. No database lock is held while waiting.
        let guard = loop {
            match ImportOperationGuard::acquire(&state) {
                Ok(guard) => break guard,
                Err(_) => std::thread::sleep(std::time::Duration::from_millis(50)),
            }
        };
        let result = ThumbnailService::new(&state.data_dir).and_then(|thumbs| {
            crate::services::import_dispatch::drain(
                &state.db,
                &state.operation_queue,
                &thumbs,
                |progress| {
                    let _ = app.emit("import://progress", progress);
                },
            )
        });
        drop(guard);
        match result {
            Ok(imported) if imported > 0 => schedule_import_palette_refill(app.clone(), &state),
            Ok(_) => {}
            Err(error) => tracing::error!(error_code=%error.code(), "导入队列调度失败"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::media_refill::RefillSummary;

    /// §10.7：导入后置真实写库成功且 updatedIds 非空时发一次事件，payload source 固定 "import"。
    #[test]
    fn import_palette_event_emitted_when_updated_ids_non_empty() {
        let s = RefillSummary {
            total: 3,
            success: 2,
            failed: 1,
            skipped: 0,
            updated_ids: vec![11, 22],
        };
        let ev = PaletteUpdatedEvent::from_import_summary(&s).expect("updatedIds 非空应发事件");
        assert_eq!(ev.source, "import");
        assert_eq!(ev.total, 3);
        assert_eq!(ev.success, 2);
        assert_eq!(ev.failed, 1);
        assert_eq!(ev.skipped, 0);
        assert_eq!(ev.updated_ids, vec![11, 22]);
    }

    /// §10.7：零更新（updatedIds 空）不发事件 —— 不得把"扫描了但没写库"冒充成功完成。
    #[test]
    fn import_palette_event_not_emitted_on_zero_updates() {
        let s = RefillSummary {
            total: 5,
            success: 0,
            failed: 0,
            skipped: 5,
            updated_ids: vec![],
        };
        assert!(
            PaletteUpdatedEvent::from_import_summary(&s).is_none(),
            "零更新不得发事件"
        );
    }

    /// §10.7：gate 占用 / 扫描错误不发事件 —— 调用点只对 Some(Ok(summary)) 构造事件，
    /// 其余路径（None / Some(Err)）根本不会进入 from_import_summary；这里再补一个显式回归：
    /// 失败与跳过素材的 id 绝不能出现在 updated_ids（resolved ids 伪装禁止）。
    #[test]
    fn failed_and_skipped_ids_never_masquerade_as_updated() {
        let s = RefillSummary {
            total: 4,
            success: 1,
            failed: 2,
            skipped: 1,
            updated_ids: vec![7],
        };
        let ev = PaletteUpdatedEvent::from_import_summary(&s).unwrap();
        assert_eq!(ev.updated_ids, vec![7]);
        assert!(
            !ev.updated_ids.contains(&999),
            "失败/跳过素材 id 不得进入 updatedIds"
        );
    }
}
