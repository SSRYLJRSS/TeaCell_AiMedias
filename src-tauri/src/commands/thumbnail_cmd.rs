use tauri::State;

use crate::db::assets;
use crate::error::{AppError, AppResult};
use crate::services::preview::PreviewService;
use crate::services::thumbnail::ThumbnailService;
use crate::state::AppState;

/// 双层缩略图：kind = "placeholder" | "hd"；返回本地路径（前端 convertFileSrc 使用）
#[tauri::command]
pub async fn get_thumbnail(
    state: State<'_, AppState>,
    asset_id: i64,
    kind: String,
    size: Option<u32>,
) -> AppResult<String> {
    let data_dir = state.data_dir.clone();
    let db = std::sync::Arc::clone(&state.db);
    // 高清解码是重活，spawn_blocking 防堵主线程（否则连 asset 协议都被饿死 → 全白+卡死）
    tauri::async_runtime::spawn_blocking(move || {
        let thumbs = ThumbnailService::new(&data_dir)?;
        match kind.as_str() {
            "placeholder" => Ok(thumbs
                .get_or_create_placeholder(&db, asset_id)?
                .to_string_lossy()
                .into_owned()),
            "hd" => {
                let p = thumbs.get_or_create_hd(&db, asset_id, size)?;
                Ok(p.to_string_lossy().into_owned())
            }
            _ => Err(AppError::invalid_arg("非法缩略图类型")),
        }
    })
    .await
    .map_err(|e| AppError::msg(format!("缩略图线程异常: {e}")))?
}

/// 待入库文件预览（PRD v2.6）：未入库文件出 320px 小图；失败返回 None 由前端显示占位图标
/// B24：校验绝对路径 + 支持的文件类型，拒绝任意路径读取
#[tauri::command]
pub async fn get_preview(state: State<'_, AppState>, path: String) -> AppResult<Option<String>> {
    // B24：校验绝对路径
    if !crate::utils::path::ensure_absolute(&path) {
        return Err(AppError::invalid_arg("路径无效"));
    }
    // B24：校验支持的文件类型
    let p = std::path::Path::new(&path);
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default();
    if crate::utils::mime::asset_type_from_ext(ext).is_none() {
        return Err(AppError::unsupported("不支持的文件类型"));
    }
    let dir = state.data_dir.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let svc = PreviewService::new(&dir)?;
        match svc.get_or_create(std::path::Path::new(&path)) {
            Ok(p) => Ok(Some(p.to_string_lossy().into_owned())),
            Err(_) => Ok(None),
        }
    })
    .await
    .map_err(|e| AppError::msg(format!("预览任务失败: {e}")))?
}

/// 清空缩略图缓存；B27：删文件后回写 DB（placeholder_path/hd_thumbnail_path = NULL），
/// 避免 DB 仍指向已删文件导致前端破图
/// P2-06：文件删除与 DB 回写拆锁——缓存较大/磁盘慢时不再长时间阻塞全部 DB 读写。
/// 顺序固定为「短锁清库字段 → 锁外删文件」：先清 DB 后删文件，任何一步失败都不会
/// 留下「DB 指向已删文件」的破图态（B27 语义保持）
#[tauri::command]
pub fn clear_thumbnail_cache(state: State<AppState>, kind: Option<String>) -> AppResult<()> {
    {
        let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        match kind.as_deref() {
            Some("placeholder") => {
                // B27：回写 placeholder_path = NULL
                assets::clear_all_placeholder_paths(&conn)?;
            }
            Some("hd") => {
                // B27：回写 hd_thumbnail_path = NULL
                assets::clear_all_hd_thumbnail_paths(&conn)?;
            }
            _ => {
                // B27：回写所有缩略图路径 = NULL
                assets::clear_all_placeholder_paths(&conn)?;
                assets::clear_all_hd_thumbnail_paths(&conn)?;
            }
        }
    } // 短锁即放，文件删除在锁外执行
    let thumbs = ThumbnailService::new(&state.data_dir)?;
    thumbs.clear(kind.as_deref())?;
    Ok(())
}
