use std::path::{Path, PathBuf};

use serde::Serialize;
use tauri::{Manager, State};

use crate::db::assets::{self, Asset, AssetFilter, AssetPage, MetadataFacet};
use crate::db::dedup::{self, DupGroup};
use crate::db::search_query;
use crate::db::settings;
use crate::error::{AppError, AppResult};
use crate::services::thumbnail::ThumbnailService;
use crate::state::AppState;

// 参数取 &AppState（State<T> 经 Deref 自动转换），规避 State 生命周期标注
fn lock_db(state: &AppState) -> AppResult<std::sync::MutexGuard<'_, rusqlite::Connection>> {
    state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))
}

/// asset 协议逐路径放行（安全收敛）：仅列表/详情返回过的素材原文件可被 webview 读取
fn allow_asset(app: &tauri::AppHandle, path: &str) {
    let _ = app.asset_protocol_scope().allow_file(Path::new(path));
}

/// B02/B03：删除结果——deleted 为已从库删除数，failed_files 为磁盘删除失败的 asset id
/// （delete_file 策略下磁盘删除失败的 id 不从库中删除，避免假删除）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeleteResult {
    pub deleted: u64,
    pub failed_files: Vec<i64>,
}

#[tauri::command]
pub fn list_assets(
    app: tauri::AppHandle,
    state: State<AppState>,
    filter: AssetFilter,
) -> AppResult<AssetPage> {
    let conn = lock_db(&state)?;
    let page = assets::list(&conn, &filter)?;
    for a in &page.items {
        allow_asset(&app, &a.file_path);
    }
    Ok(page)
}

/// 重复素材扫描（M3-02 R-20）：hash 精确分组，单次 GROUP BY 毫秒级，无需异步进度
#[tauri::command]
pub fn dedup_scan(app: tauri::AppHandle, state: State<AppState>) -> AppResult<Vec<DupGroup>> {
    let conn = lock_db(&state)?;
    let groups = dedup::scan_groups(&conn)?;
    for g in &groups {
        for a in &g.assets {
            allow_asset(&app, &a.file_path);
        }
    }
    Ok(groups)
}

/// W5d（§W5d）：感知相似扫描（dHash 汉明 ≤ threshold；exclude_kinship 排除同源 RAW+JPG）。
/// threshold = 0 → 空（前端用 0 挡「相似图未启用/无 phash」）。全内存分桶，毫秒级，无需异步进度。
#[tauri::command]
pub fn dedup_scan_similar(
    app: tauri::AppHandle,
    state: State<AppState>,
    threshold: u32,
    exclude_kinship: bool,
) -> AppResult<Vec<DupGroup>> {
    let conn = lock_db(&state)?;
    let groups = dedup::scan_similar_groups(&conn, threshold, exclude_kinship, &[])?;
    for g in &groups {
        for a in &g.assets {
            allow_asset(&app, &a.file_path);
        }
    }
    Ok(groups)
}

/// 取当前筛选结果的全部 id（BUG-E：全选/反选/批量操作用）。
/// 只 SELECT id，不返回完整 Asset、不调 allow_asset——避免拉全量对象浪费 IPC/内存，
/// 且不暴露原文件路径、消除 asset 协议 scope 随全选无界增长。
#[tauri::command]
pub fn list_asset_ids(state: State<AppState>, filter: AssetFilter) -> AppResult<Vec<i64>> {
    let conn = lock_db(&state)?;
    assets::list_ids(&conn, &filter)
}

#[tauri::command]
pub fn list_metadata_facets(state: State<AppState>) -> AppResult<Vec<MetadataFacet>> {
    let conn = lock_db(&state)?;
    let library_root = settings::get_settings(&conn)?.library_root;
    assets::list_metadata_facets(&conn, Some(&library_root))
}

/// Phase 4（§5.3）+ V24（Phase 7-8）：数值字段的 NumericDomain 单一事实源
///（含预设/单位/边界/量纲阈值/运算符）+ 数值分面动态段（key = "facet:<facet_key>"）。
#[tauri::command]
pub fn get_numeric_domains(state: State<AppState>) -> AppResult<Vec<search_query::NumericDomain>> {
    let conn = state
        .db
        .lock()
        .map_err(|_| crate::error::AppError::msg("数据库锁中毒"))?;
    Ok(search_query::numeric_domains_with_facets(&conn))
}

#[tauri::command]
pub fn get_asset(app: tauri::AppHandle, state: State<AppState>, id: i64) -> AppResult<Asset> {
    let conn = lock_db(&state)?;
    let a = assets::get(&conn, id)?;
    allow_asset(&app, &a.file_path);
    Ok(a)
}

/// 删除双策略：remove_from_library（软删入回收站，R-22）| delete_file（连同原文件硬删）
/// B02：改 async + spawn_blocking，文件 IO 下沉工作线程，避免主线程阻塞卡 UI
/// B03：delete_file 策略下磁盘删除失败的 id 不从库删（消除假删除），收集失败列表返回前端
#[tauri::command]
pub async fn delete_assets(
    state: State<'_, AppState>,
    ids: Vec<i64>,
    strategy: String,
) -> AppResult<DeleteResult> {
    // 参数校验仍在主线程（快）
    if strategy != "remove_from_library" && strategy != "delete_file" {
        return Err(AppError::invalid_arg("非法删除策略"));
    }

    let db = std::sync::Arc::clone(&state.db);
    let data_dir = state.data_dir.clone();

    tauri::async_runtime::spawn_blocking(move || {
        delete_assets_blocking(db, data_dir, ids, strategy)
    })
    .await
    .map_err(|e| AppError::msg(format!("删除线程异常: {e}")))?
}

/// 删除命令的锁/文件 IO 分阶段实现。
///
/// 设置页的“原始素材文件”重置需要一次性处理在库和回收站中的全部素材，
/// 复用同一实现可保证磁盘删除失败时保留对应数据库记录（B03），不会出现假删除。
fn delete_assets_blocking(
    db: std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
    data_dir: PathBuf,
    ids: Vec<i64>,
    strategy: String,
) -> AppResult<DeleteResult> {
    let thumbs = ThumbnailService::new(&data_dir)?;

    // 阶段一：短锁收集待删文件路径（仅 delete_file 策略需要原文件路径）
    let paths: Vec<(i64, PathBuf)> = if strategy == "delete_file" {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        ids.iter()
            .filter_map(|&id| {
                // 删除只需要路径；不要为全库重置加载完整 Asset 与标签，
                // 否则素材量大时会额外占用大量内存和查询时间。
                conn.query_row("SELECT file_path FROM assets WHERE id = ?1", [id], |row| {
                    row.get::<_, String>(0)
                })
                .ok()
                .map(|path| (id, PathBuf::from(path)))
            })
            .collect()
    } else {
        Vec::new()
    };

    // 阶段二：锁外删磁盘文件 + 收集失败（B03：不再吞错）
    let failed_set: std::collections::HashSet<i64> = if strategy == "delete_file" {
        paths
            .iter()
            .filter_map(|(id, p)| {
                if std::fs::remove_file(p).is_err() {
                    Some(*id) // B03：记录磁盘删除失败的 id
                } else {
                    None
                }
            })
            .collect()
    } else {
        std::collections::HashSet::new()
    };

    // 阶段三：短锁写库——delete_file 只删磁盘删除成功的；remove_from_library 软删入回收站（R-22）
    let to_delete_db: Vec<i64> = if strategy == "delete_file" {
        ids.iter()
            .filter(|id| !failed_set.contains(id))
            .copied()
            .collect()
    } else {
        ids.clone()
    };
    let n = {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        if strategy == "delete_file" {
            assets::delete(&conn, &to_delete_db)?
        } else {
            assets::soft_delete(&conn, &to_delete_db)?
        }
    };

    // 阶段四：缩略图清理——仅硬删清理；软删保留缩略图供回收站预览/恢复（R-22）
    if strategy == "delete_file" {
        for &id in &to_delete_db {
            thumbs.delete_for_asset(id);
        }
    }

    let failed_files: Vec<i64> = failed_set.into_iter().collect();
    Ok(DeleteResult {
        deleted: n,
        failed_files,
    })
}

/// 设置页“删除原始素材文件”专用入口：收集全部在库与回收站素材 id，
/// 然后复用批量硬删流程。该函数只在 settings_cmd 的 spawn_blocking 中调用，
/// 不持有数据库锁执行文件 IO。
pub fn delete_all_asset_files_for_reset(
    db: std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
    data_dir: PathBuf,
) -> AppResult<DeleteResult> {
    let ids = {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        let mut stmt = conn.prepare("SELECT id FROM assets")?;
        let rows = stmt.query_map([], |r| r.get::<_, i64>(0))?;
        rows.collect::<Result<Vec<_>, _>>()?
    };
    delete_assets_blocking(db, data_dir, ids, "delete_file".to_owned())
}

/// R-22 回收站恢复：deleted_at 置空，素材回到在库状态（缩略图未删，无需重建）
#[tauri::command]
pub fn trash_restore(state: State<AppState>, ids: Vec<i64>) -> AppResult<u64> {
    let conn = lock_db(&state)?;
    assets::restore(&conn, &ids)
}

/// W2-8（② T17）：批量评级（0 = 清除）
#[tauri::command]
pub fn set_rating(state: State<AppState>, ids: Vec<i64>, rating: i64) -> AppResult<u64> {
    let conn = lock_db(&state)?;
    assets::set_rating(&conn, &ids, rating)
}

/// W2-8（② T17）：批量手动旋转（0/90/180/270，写 user_rotation）
#[tauri::command]
pub fn set_user_rotation(state: State<AppState>, ids: Vec<i64>, rotation: i64) -> AppResult<u64> {
    let conn = lock_db(&state)?;
    assets::set_user_rotation(&conn, &ids, rotation)
}

/// R-22 超期回收站自动清理（启动时调用，不常驻定时器）：
/// 短锁取清单 → 锁外删文件（失败保留记录，沿用 B03 语义）→ 短锁硬删 DB + 清缩略图
pub fn purge_expired_trash(
    db: &std::sync::Arc<std::sync::Mutex<rusqlite::Connection>>,
    data_dir: &std::path::Path,
    retention_days: i64,
) -> AppResult<()> {
    let cutoff = chrono::Utc::now().timestamp_millis() - retention_days * 86_400_000;
    let expired = {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        assets::list_expired_trash(&conn, cutoff)?
    };
    if expired.is_empty() {
        return Ok(());
    }
    let mut ok_ids: Vec<i64> = Vec::new();
    for (id, path) in &expired {
        if std::fs::remove_file(path).is_ok() {
            ok_ids.push(*id);
        } else {
            tracing::warn!("回收站清理：文件删除失败保留记录 id={id} path={path}");
        }
    }
    if ok_ids.is_empty() {
        return Ok(());
    }
    {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        assets::delete(&conn, &ok_ids)?;
    }
    let thumbs = ThumbnailService::new(data_dir)?;
    for &id in &ok_ids {
        thumbs.delete_for_asset(id);
    }
    tracing::info!("回收站自动清理完成：{} 项", ok_ids.len());
    Ok(())
}

/// 返回原始文件路径（前端 convertFileSrc 使用，禁止拼 file://）
/// B09：改为部分成功——跳过不存在的 id 而非整体 Err（删除后选中含已删 id 时复制路径不再整体失败）
#[tauri::command]
pub fn get_asset_urls(
    app: tauri::AppHandle,
    state: State<AppState>,
    ids: Vec<i64>,
) -> AppResult<Vec<String>> {
    let conn = lock_db(&state)?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        match assets::get(&conn, id) {
            Ok(a) => {
                allow_asset(&app, &a.file_path);
                out.push(a.file_path);
            }
            Err(_) => continue, // B09：跳过不存在的 id，不整体失败
        }
    }
    Ok(out)
}

/// 在系统文件管理器中定位文件（右键菜单「打开所在文件夹」）
/// B24：校验路径属于已入库文件，拒绝任意路径（防越权打开系统目录）
#[tauri::command]
pub fn reveal_in_folder(
    app: tauri::AppHandle,
    state: State<AppState>,
    path: String,
) -> AppResult<()> {
    // B24：校验路径属于已入库素材
    let norm = crate::utils::path::normalize_path(&path);
    {
        let conn = lock_db(&state)?;
        if assets::find_by_path(&conn, &norm)?.is_none() {
            return Err(AppError::msg("路径不属于已入库素材"));
        }
    }
    use tauri_plugin_opener::OpenerExt;
    app.opener()
        .reveal_item_in_dir(&path)
        .map_err(|e| AppError::msg(format!("打开所在文件夹失败: {e}")))
}

/// 一句话描述列表（标签与分类设置页展示；描述走 FTS 模糊搜索，不参与分面精确筛选）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContentDescription {
    pub asset_id: i64,
    pub file_name: String,
    pub description: String,
}

#[tauri::command]
pub fn list_content_descriptions(
    state: State<AppState>,
    limit: Option<i64>,
) -> AppResult<Vec<ContentDescription>> {
    let conn = lock_db(&state)?;
    let rows = assets::list_content_descriptions(&conn, limit.unwrap_or(200).clamp(1, 500))?;
    Ok(rows
        .into_iter()
        .map(|(asset_id, file_name, description)| ContentDescription {
            asset_id,
            file_name,
            description,
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{assets, init_memory};

    #[test]
    fn reset_file_helper_deletes_existing_files_but_keeps_failed_records() {
        let conn = init_memory().unwrap();
        let db = std::sync::Arc::new(std::sync::Mutex::new(conn));
        let data_dir = tempfile::tempdir().unwrap();
        let original = data_dir.path().join("keep-me.jpg");
        std::fs::write(&original, b"test").unwrap();
        let missing = data_dir.path().join("missing.jpg");

        let (existing_id, missing_id) = {
            let conn = db.lock().unwrap();
            let a = assets::insert(
                &conn,
                original.to_string_lossy().as_ref(),
                "keep-me.jpg",
                "jpg",
                4,
                "image/jpeg",
                1,
            )
            .unwrap();
            let b = assets::insert(
                &conn,
                missing.to_string_lossy().as_ref(),
                "missing.jpg",
                "jpg",
                0,
                "image/jpeg",
                1,
            )
            .unwrap();
            (a, b)
        };

        let report =
            delete_all_asset_files_for_reset(db.clone(), data_dir.path().to_path_buf()).unwrap();
        assert_eq!(report.deleted, 1);
        assert_eq!(report.failed_files, vec![missing_id]);
        assert!(!original.exists());
        let conn = db.lock().unwrap();
        assert!(assets::get(&conn, existing_id).is_err());
        assert!(assets::get(&conn, missing_id).is_ok());
    }
}
