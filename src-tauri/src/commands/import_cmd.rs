use std::sync::atomic::Ordering;

use serde::Serialize;
use tauri::{AppHandle, Emitter, Manager, State};

use crate::db::assets::ImportResult;
use crate::db::settings;
use crate::error::{AppError, AppResult};
use crate::services::importer::{self, ImportOptions, ImportProgress};
use crate::services::{media_refill, thumbnail::ThumbnailService};
use crate::state::AppState;

/// FB4-03（§6.4）：导入后置色板完成事件。只允许导入后置任务发送；手动设置页回算不发。
/// 发送条件（全部满足）：由 import_files 后台后置任务触发 + 成功抢到 refill gate +
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

/// 入库：async + spawn_blocking 工作线程（不堵主线程 IPC，取消即时生效）；
/// collection/rename 来自入库页选项；总库位置以设置为准（R-32，单一事实源）
#[tauri::command]
pub async fn import_files(
    app: AppHandle,
    state: State<'_, AppState>,
    paths: Vec<String>,
    collection: Option<String>,
    rename_pattern: Option<String>,
) -> AppResult<ImportResult> {
    if paths.is_empty() {
        return Err(AppError::invalid_arg("未选择任何文件"));
    }
    // 防止绕过入库页直接调用 IPC：正式复制/写库前再次确认所有图片都能生成
    // 快速缩略图。不可预览项必须先从队列剔除，不能进入半成功的导入任务。
    let preflight_paths = paths.clone();
    let preflight =
        tauri::async_runtime::spawn_blocking(move || importer::inspect_paths(&preflight_paths))
            .await
            .map_err(|e| AppError::msg(format!("入库预检查线程异常: {e}")))?;
    let blocked: Vec<String> = preflight
        .items
        .iter()
        .filter(|item| {
            matches!(
                item.preview_status,
                importer::ImportPreviewStatus::Unsupported
            )
        })
        .map(|item| item.path.clone())
        .collect();
    if !blocked.is_empty() {
        let shown = blocked
            .iter()
            .take(5)
            .cloned()
            .collect::<Vec<_>>()
            .join("、");
        let suffix = if blocked.len() > 5 {
            format!(" 等 {} 个文件", blocked.len())
        } else {
            String::new()
        };
        return Err(AppError::unsupported(format!(
            "有 {} 个文件无法解析入库缩略图，请先从队列剔除：{}{}",
            blocked.len(),
            shown,
            suffix
        )));
    }
    state.import_cancel.store(false, Ordering::Relaxed);
    // W5c：入库进行中标志（restore_db 阻断依据）
    state.import_running.store(true, Ordering::Relaxed);
    let db = std::sync::Arc::clone(&state.db);
    let data_dir = state.data_dir.clone();
    let cancel = std::sync::Arc::clone(&state.import_cancel);
    // 后置色板任务也需要 emit：AppHandle 是 Clone，先克隆一份供第二个 spawn_blocking 使用
    let post_app = app.clone();

    let result = match tauri::async_runtime::spawn_blocking(move || {
        let thumbs = ThumbnailService::new(&data_dir)?;
        let library_root = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            settings::get_settings(&conn)?.library_root
        };
        let opts = ImportOptions {
            library_root: if library_root.trim().is_empty() {
                None
            } else {
                Some(library_root)
            },
            collection,
            rename_pattern: rename_pattern.filter(|p| !p.trim().is_empty()),
        };
        importer::import_paths(&db, &thumbs, &paths, &opts, &cancel, |p: ImportProgress| {
            let _ = app.emit("import://progress", p);
        })
    })
    .await
    {
        Ok(r) => r,
        Err(e) => {
            state.import_running.store(false, Ordering::Relaxed);
            return Err(AppError::msg(format!("入库线程异常: {e}")));
        }
    };
    state.import_running.store(false, Ordering::Relaxed);
    let result = result?;

    // FB2-08（§14.7）：入库完成后自动补算色板。
    // 不塞进 importer 热路径：placeholder 生成是 par_iter 并行块，图像不驻留内存，
    // 要在那里取图得改 extract_placeholder 签名（影响 3 个调用点）。走 missing scope
    // 读 placeholder 文件重算，256px WebP 解码成本可忽略，且天然幂等（FX-11）。
    // 失败不影响入库结果（色板是增强信息）；闸被占用时跳过（下次手动回算补上）；
    // on_progress 传空闭包，不与导入进度事件混淆前端状态机。
    if result.imported > 0 {
        let gate = std::sync::Arc::clone(&state.refill_running);
        let pdb = std::sync::Arc::clone(&state.db);
        let pcancel = std::sync::Arc::clone(&state.media_refill_cancel);
        tauri::async_runtime::spawn_blocking(move || {
            let ids = {
                let conn = match pdb.lock() {
                    Ok(c) => c,
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
                    Ok(v) => v,
                    Err(e) => {
                        tracing::warn!(
                            operation = "import_palette_refill",
                            result = "failed",
                            reason = "list_ids",
                            error = %e,
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
            match media_refill::try_rescan_palette_exclusive(&gate, &pdb, &ids, &pcancel, |_| {}) {
                Some(Ok(s)) => {
                    tracing::info!(
                        operation = "import_palette_refill",
                        result = "done",
                        total = s.total,
                        success = s.success,
                        skipped = s.skipped,
                        failed = s.failed,
                        "入库后置色板补算完成"
                    );
                    // FB4-03（§6.4）：只有真实写库成功的素材非空才发全局事件（供 App 定向同步）。
                    // 闸被占用（None）、扫描错误（Some(Err)）、零更新（updatedIds 空）均不发。
                    if let Some(ev) = PaletteUpdatedEvent::from_import_summary(&s) {
                        let _ = post_app.emit("palette://updated", ev);
                    }
                }
                Some(Err(e)) => tracing::warn!(
                    operation = "import_palette_refill",
                    result = "failed",
                    error = %e,
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
    Ok(result)
}

/// 扫描路径生成待入库清单统计（两段式入库，不落库）
/// B07：改 async + spawn_blocking，大目录扫描不卡主线程
/// FB2-03：对清单内每个原文件逐路径放行 asset 协议，使入库页可在卡片内播放视频。
/// 安全边界与 list_assets 一致——只放行用户主动选择/拖入的路径，只读，不放宽 scope、不用 allow_directory。
#[tauri::command]
pub async fn inspect_import(app: AppHandle, paths: Vec<String>) -> AppResult<importer::ImportPlan> {
    let plan = tauri::async_runtime::spawn_blocking(move || importer::inspect_paths(&paths))
        .await
        .map_err(|e| AppError::msg(format!("扫描线程异常: {e}")))?;
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

#[tauri::command]
pub fn cancel_import(state: State<AppState>) {
    state.import_cancel.store(true, Ordering::Relaxed);
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
