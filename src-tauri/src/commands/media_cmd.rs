//! 媒体元数据回填命令（指导书 §7.5）：全部视频 / 选中素材 / 仅缺字段 三种范围；
//! 运行在 spawn_blocking（不阻塞 UI 主线程），带进度事件 + 取消；单个坏文件不阻塞批次。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::db::assets;
use crate::error::{AppError, AppResult};
use crate::services::media_refill;
use crate::state::AppState;

/// 抢互斥闸：已有回填在跑时明确拒绝，不静默复位对方的取消标志（FX-12）。
/// RAII guard（RefillGateGuard）与抢闸逻辑在 media_refill 内实现，供导入后置（FX-11）复用。
fn acquire_refill_gate(gate: &Arc<AtomicBool>) -> AppResult<media_refill::RefillGateGuard> {
    media_refill::try_acquire_gate(gate).ok_or_else(|| {
        AppError::conflict("已有回填任务进行中（媒体元数据回填或色板回算），请等待完成或先取消")
    })
}

/// 回填任务运行许可：回填互斥闸（RAII，move 进工作线程后持有到任务结束）。
pub struct RefillRunToken {
    _gate: media_refill::RefillGateGuard,
}

/// 开一轮回填：抢回填互斥闸，成功后才重置取消标志（FX-12）。
/// 顺序不能反：抢不到闸的调用方若已经复位了标志，正在跑的那一批就丢掉了用户点过的"取消"。
fn begin_refill(state: &AppState, _task: &str, cancel: &AtomicBool) -> AppResult<RefillRunToken> {
    let guard = acquire_refill_gate(&state.refill_running)?;
    cancel.store(false, Ordering::Relaxed);
    Ok(RefillRunToken { _gate: guard })
}

/// 回填结果 JSON（camelCase 序列化）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RescanResult {
    pub total: i64,
    pub success: i64,
    pub failed: i64,
    pub skipped: i64,
}

/// FB4-03（§5.6）：色板回算独立返回类型 —— updatedIds 只包含本轮成功执行
/// set_palette 的素材 id（不包含失败、跳过或仅被扫描的 id）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PaletteRescanResult {
    pub total: i64,
    pub success: i64,
    pub failed: i64,
    pub skipped: i64,
    pub updated_ids: Vec<i64>,
}

#[tauri::command]
pub async fn rescan_asset_metadata(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Option<Vec<i64>>,
    scope: Option<String>,
) -> AppResult<RescanResult> {
    let scope = scope.unwrap_or_else(|| "missing".to_string());
    if scope != "all" && scope != "missing" && scope != "ids" {
        return Err(AppError::invalid_arg("scope 只允许 all | missing | ids"));
    }
    if scope == "ids" && ids.as_ref().is_none_or(|v| v.is_empty()) {
        return Err(AppError::invalid_arg("未选择任何素材"));
    }
    let db = Arc::clone(&state.db);
    let cancel = Arc::clone(&state.media_refill_cancel);
    // FX-12：抢闸失败时明确报错；token move 进闭包，覆盖所有退出路径。
    // 顺序要紧：先抢闸再重置取消标志（见 begin_refill）。X-10：并含维护活动租约。
    let _token = begin_refill(&state, "媒体元数据回填", &cancel)?;

    tauri::async_runtime::spawn_blocking(move || -> AppResult<RescanResult> {
        let _token = _token;
        // 锁外探测，只短锁读/写每行
        let resolved = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            match scope.as_str() {
                "ids" => ids.clone().unwrap_or_default(),
                "missing" => assets::list_video_ids_needing_metadata(&conn)?,
                _ => assets::list_video_ids(&conn)?,
            }
        };
        let summary = media_refill::rescan_assets(&db, &resolved, &cancel, |p| {
            let _ = app.emit("media_refill://progress", p);
        })?;
        Ok(RescanResult {
            total: summary.total,
            success: summary.success,
            failed: summary.failed,
            skipped: summary.skipped,
        })
    })
    .await
    .map_err(|e| AppError::msg(format!("回填线程异常: {e}")))?
}

/// FB2-08（§14.7）：算法色板回算（scope = all | missing | ids；复用 media_refill 取消标志与骨架）。
/// 独立命令而非塞进 rescan_asset_metadata：语义与耗时都不同，混在一起用户没法只跑其中一个。
/// FB4-03（§5.6）：返回 PaletteRescanResult，updatedIds 只含本轮真实写库成功的素材。
#[tauri::command]
pub async fn rescan_asset_palette(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Option<Vec<i64>>,
    scope: Option<String>,
) -> AppResult<PaletteRescanResult> {
    let scope = scope.unwrap_or_else(|| "missing".to_string());
    if scope != "all" && scope != "missing" && scope != "ids" {
        return Err(AppError::invalid_arg("scope 只允许 all | missing | ids"));
    }
    if scope == "ids" && ids.as_ref().is_none_or(|v| v.is_empty()) {
        return Err(AppError::invalid_arg("未选择任何素材"));
    }
    let db = Arc::clone(&state.db);
    let cancel = Arc::clone(&state.media_refill_cancel);
    // FX-12：与元数据回填互斥（抢闸失败时明确报错），先抢闸再重置取消标志。X-10：并含维护活动租约。
    let _token = begin_refill(&state, "色板回算", &cancel)?;

    tauri::async_runtime::spawn_blocking(move || -> AppResult<PaletteRescanResult> {
        let _token = _token;
        let resolved: Vec<i64> = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            match scope.as_str() {
                "ids" => ids.unwrap_or_default(),
                "missing" => assets::list_ids_needing_palette(&conn)?,
                _ => assets::list_all_ids(&conn)?,
            }
        };
        let summary = media_refill::rescan_assets_palette(&db, &resolved, &cancel, |p| {
            let _ = app.emit("media_refill://progress", p);
        })?;
        Ok(PaletteRescanResult {
            total: summary.total,
            success: summary.success,
            failed: summary.failed,
            skipped: summary.skipped,
            updated_ids: summary.updated_ids,
        })
    })
    .await
    .map_err(|e| AppError::msg(format!("色板回算线程异常: {e}")))?
}

/// C-1：从 palette_json 全量重建 asset_palette_colors（不解码图片，毫秒级；幂等）。
/// 照抄 rescan_asset_palette 的具名后台线程骨架（本命令轻量，无进度事件仍走 spawn_blocking）。
#[tauri::command]
pub async fn rescan_palette_colors(app: AppHandle, state: State<'_, AppState>) -> AppResult<i64> {
    let db = Arc::clone(&state.db);
    tauri::async_runtime::spawn_blocking(move || -> AppResult<i64> {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        let n = assets::rescan_palette_colors(&conn)?;
        let _ = app.emit("palette_colors://done", n);
        Ok(n)
    })
    .await
    .map_err(|e| AppError::msg(format!("色板关系表重建线程异常: {e}")))?
}

/// V18：GPS 定位 + 视频拍摄时间存量回填（scope = all | missing | ids）。
/// 独立命令：语义与媒体元数据回填/色板回算都不同，且视频优先解析已存 ffprobe JSON，
/// 大部分情况免拉子进程，单独跑成本低。只补空（COALESCE），不覆盖已有值。
#[tauri::command]
pub async fn rescan_asset_geo_taken(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Option<Vec<i64>>,
    scope: Option<String>,
) -> AppResult<RescanResult> {
    let scope = scope.unwrap_or_else(|| "missing".to_string());
    if scope != "all" && scope != "missing" && scope != "ids" {
        return Err(AppError::invalid_arg("scope 只允许 all | missing | ids"));
    }
    if scope == "ids" && ids.as_ref().is_none_or(|v| v.is_empty()) {
        return Err(AppError::invalid_arg("未选择任何素材"));
    }
    let db = Arc::clone(&state.db);
    let cancel = Arc::clone(&state.media_refill_cancel);
    // 与其他回填互斥：先抢闸再重置取消标志（FX-12）。X-10：并含维护活动租约。
    let _token = begin_refill(&state, "定位回填", &cancel)?;

    tauri::async_runtime::spawn_blocking(move || -> AppResult<RescanResult> {
        let _token = _token;
        let resolved: Vec<i64> = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            match scope.as_str() {
                "ids" => ids.unwrap_or_default(),
                "missing" => assets::list_ids_needing_geo_taken(&conn)?,
                _ => assets::list_geo_taken_all_ids(&conn)?,
            }
        };
        let summary = media_refill::rescan_assets_geo_taken(&db, &resolved, &cancel, |p| {
            let _ = app.emit("media_refill://progress", p);
        })?;
        Ok(RescanResult {
            total: summary.total,
            success: summary.success,
            failed: summary.failed,
            skipped: summary.skipped,
        })
    })
    .await
    .map_err(|e| AppError::msg(format!("定位回填线程异常: {e}")))?
}

/// W1-4：图片宽高存量回填（RAW 分辨率修复，scope = all | missing | ids）。
/// 独立命令：rawler decode_file 读整个文件，205 张 45MP 可能数分钟，必须可取消 + 有进度。
#[tauri::command]
pub async fn rescan_image_dimensions(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Option<Vec<i64>>,
    scope: Option<String>,
) -> AppResult<RescanResult> {
    let scope = scope.unwrap_or_else(|| "missing".to_string());
    if scope != "all" && scope != "missing" && scope != "ids" {
        return Err(AppError::invalid_arg("scope 只允许 all | missing | ids"));
    }
    if scope == "ids" && ids.as_ref().is_none_or(|v| v.is_empty()) {
        return Err(AppError::invalid_arg("未选择任何素材"));
    }
    let db = Arc::clone(&state.db);
    let cancel = Arc::clone(&state.media_refill_cancel);
    // 与其他回填互斥：先抢闸再重置取消标志（FX-12）。X-10：并含维护活动租约。
    let _token = begin_refill(&state, "图片宽高回填", &cancel)?;

    tauri::async_runtime::spawn_blocking(move || -> AppResult<RescanResult> {
        let _token = _token;
        let resolved: Vec<i64> = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            match scope.as_str() {
                "ids" => ids.unwrap_or_default(),
                "missing" => assets::list_ids_needing_dimensions(&conn)?,
                _ => assets::list_image_ids(&conn)?,
            }
        };
        let summary = media_refill::rescan_assets_dimensions(&db, &resolved, &cancel, |p| {
            let _ = app.emit("media_refill://progress", p);
        })?;
        Ok(RescanResult {
            total: summary.total,
            success: summary.success,
            failed: summary.failed,
            skipped: summary.skipped,
        })
    })
    .await
    .map_err(|e| AppError::msg(format!("宽高回填线程异常: {e}")))?
}

/// FB4-03（§5.3）：色板状态查询 —— totalAssets / eligible / ready / missing / unavailable。
/// 供设置页解释「为什么当前没有色条」并决定「生成缺失色条」按钮状态。
#[tauri::command]
pub fn get_palette_status(state: State<AppState>) -> AppResult<assets::PaletteStatus> {
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    assets::get_palette_status(&conn)
}

/// W5d（§W5d）：感知哈希存量回填（scope = all | missing | ids；照抄色板回算骨架）。
/// 入库新图时 phash 已在占位图生成路径搭车写入；此处只处理存量/升级前导入的图片。
/// 手动触发入口在设置页「数据与缓存 → 媒体元数据回填」旁的感知哈希回填行。
#[tauri::command]
pub async fn rescan_asset_phash(
    app: AppHandle,
    state: State<'_, AppState>,
    ids: Option<Vec<i64>>,
    scope: Option<String>,
) -> AppResult<RescanResult> {
    let scope = scope.unwrap_or_else(|| "missing".to_string());
    if scope != "all" && scope != "missing" && scope != "ids" {
        return Err(AppError::invalid_arg("scope 只允许 all | missing | ids"));
    }
    if scope == "ids" && ids.as_ref().is_none_or(|v| v.is_empty()) {
        return Err(AppError::invalid_arg("未选择任何素材"));
    }
    let db = Arc::clone(&state.db);
    let cancel = Arc::clone(&state.media_refill_cancel);
    // 与其他回填互斥：先抢闸再重置取消标志（FX-12）。X-10：并含维护活动租约。
    let _token = begin_refill(&state, "感知哈希回填", &cancel)?;

    tauri::async_runtime::spawn_blocking(move || -> AppResult<RescanResult> {
        let _token = _token;
        let resolved: Vec<i64> = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            assets::list_ids_needing_phash(&conn, &scope, ids.as_deref().unwrap_or_default())?
        };
        let summary = media_refill::rescan_assets_phash(&db, &resolved, &cancel, |p| {
            let _ = app.emit("media_refill://progress", p);
        })?;
        Ok(RescanResult {
            total: summary.total,
            success: summary.success,
            failed: summary.failed,
            skipped: summary.skipped,
        })
    })
    .await
    .map_err(|e| AppError::msg(format!("感知哈希回填线程异常: {e}")))?
}

/// FB4-03（§5.5）：按 id 定向读取色板补丁（前端分批，单次 ≤1000；只返回存在的 id）。
#[tauri::command]
pub fn get_asset_palette_patches(
    state: State<AppState>,
    ids: Vec<i64>,
) -> AppResult<Vec<assets::AssetPalettePatch>> {
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    assets::get_asset_palette_patches(&conn, &ids)
}

/// 取消正在进行的媒体元数据回填。
#[tauri::command]
pub fn cancel_media_refill(state: State<AppState>) -> AppResult<()> {
    state.media_refill_cancel.store(true, Ordering::Relaxed);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// FX-12：抢闸失败时不得改变闸状态；guard drop 后闸释放。
    #[test]
    fn refill_gate_is_exclusive_and_self_releasing() {
        let gate = Arc::new(AtomicBool::new(false));
        {
            let _g = acquire_refill_gate(&gate).expect("首次抢闸应成功");
            assert!(
                acquire_refill_gate(&gate).is_err(),
                "第二个任务必须抢不到闸"
            );
            assert!(gate.load(Ordering::Acquire), "抢闸失败不得复位闸");
        }
        assert!(!gate.load(Ordering::Acquire), "guard drop 后闸应释放");
    }

    /// FX-12 回归：抢不到闸的调用方不得复位取消标志。
    /// 反例（旧顺序）：用户点"取消"→ 另一处（如 Viewer 单张重算）发起回填 →
    /// 它先 store(false) 再抢闸失败，正在跑的批次就丢掉了取消请求。
    #[test]
    fn losing_gate_does_not_clear_cancel_flag() {
        let cancel = AtomicBool::new(false);

        let state =
            crate::state::AppState::new(crate::db::init_memory().unwrap(), std::env::temp_dir());
        let _running = begin_refill(&state, "回填", &cancel).expect("首轮应抢到闸");
        cancel.store(true, Ordering::Relaxed); // 用户点了取消

        assert!(
            begin_refill(&state, "回填", &cancel).is_err(),
            "第二轮必须抢不到闸（活动租约已被首轮持有，回填闸也被占）"
        );
        assert!(
            cancel.load(Ordering::Relaxed),
            "抢闸失败不得复位取消标志，否则正在跑的批次停不下来"
        );
    }

    /// begin_refill 抢到闸时必须重置取消标志：上一轮被取消过时标志仍为 true，
    /// 不重置会让新一轮在第一个素材前就 break。
    #[test]
    fn winning_gate_resets_cancel_flag() {
        let cancel = AtomicBool::new(true); // 上一轮被取消后的残留状态
        let state =
            crate::state::AppState::new(crate::db::init_memory().unwrap(), std::env::temp_dir());
        let _g = begin_refill(&state, "回填", &cancel).expect("应抢到闸");
        assert!(!cancel.load(Ordering::Relaxed), "新一轮应重置取消标志");
    }
}
