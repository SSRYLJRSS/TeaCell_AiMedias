//! 视频兼容代理命令（指导书 §8.3）：按需生成 H.264/AAC MP4、查询状态、取消、清理。
//! 生成在 spawn_blocking（不阻塞 UI 主线程）；转码为单 ffmpeg 子进程并带超时 + 取消。

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use tauri::State;

use crate::db::video_proxy::VideoProxy;
use crate::error::{AppError, AppResult};
use crate::services::{video, video_proxy};
use crate::state::AppState;

fn lock_db(state: &AppState) -> AppResult<std::sync::MutexGuard<'_, rusqlite::Connection>> {
    state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))
}

/// P1-03：三个视频代理命令共享的 variant 解析。
/// variant 省略时**不再前端/API 层硬编码 h264_mp4**，而由后端按平台默认
/// （Linux→vp8_webm，Windows/macOS→h264_mp4）决定，避免 UI 初始化时序决定编码格式。
/// 显式传入的 variant 仍按严格枚举校验，拒绝未知值。
fn resolve_variant(variant: Option<String>) -> AppResult<(String, video::ProxyVariant)> {
    let variant =
        variant.unwrap_or_else(|| crate::services::platform::preferred_video_proxy().to_string());
    let parsed = video::ProxyVariant::parse(&variant)?;
    Ok((variant, parsed))
}

#[tauri::command]
pub async fn ensure_video_proxy(
    state: State<'_, AppState>,
    asset_id: i64,
    variant: Option<String>,
) -> AppResult<VideoProxy> {
    // 三端复核 R2 + P1-03：省略时取后端平台默认；严格枚举校验拒绝未知变体。
    let (variant, parsed_variant) = resolve_variant(variant)?;
    let db = Arc::clone(&state.db);
    let proxy_dir = state.data_dir.join("proxies");
    let cancel = Arc::new(AtomicBool::new(false));
    let key = format!("{asset_id}:{variant}");
    let proxy_cancel_reg = Arc::clone(&state.video_proxy_cancel);
    {
        let mut m = proxy_cancel_reg
            .lock()
            .map_err(|_| AppError::msg("取消注册表锁中毒"))?;
        m.insert(key.clone(), Arc::clone(&cancel));
    }

    tauri::async_runtime::spawn_blocking(move || -> AppResult<VideoProxy> {
        let r = video_proxy::get_or_create_proxy(
            &db,
            &proxy_dir,
            asset_id,
            &variant,
            &cancel,
            // 按变体分派编码器（H.264/AAC MP4 或 VP8/Vorbis WebM）。
            move |src, tmp, c| video::transcode_variant(src, tmp, parsed_variant, Some(c)),
        );
        // 收尾清理取消标志
        if let Ok(mut m) = proxy_cancel_reg.lock() {
            m.remove(&key);
        } else {
            tracing::error!("视频代理取消注册表锁中毒，{key} 未清理");
        }
        r
    })
    .await
    .map_err(|e| AppError::msg(format!("代理线程异常: {e}")))?
}

/// 查询代理状态（不触发生成）。
#[tauri::command]
pub fn get_video_proxy_status(
    state: State<AppState>,
    asset_id: i64,
    variant: Option<String>,
) -> AppResult<Option<VideoProxy>> {
    let (variant, _) = resolve_variant(variant)?; // R2 严格枚举 + P1-03 平台默认
    let conn = lock_db(&state)?;
    crate::db::video_proxy::get(&conn, asset_id, &variant)
}

/// 取消正在生成的代理。
#[tauri::command]
pub fn cancel_video_proxy(
    state: State<AppState>,
    asset_id: i64,
    variant: Option<String>,
) -> AppResult<()> {
    let (variant, _) = resolve_variant(variant)?; // R2 严格枚举 + P1-03 平台默认
    let key = format!("{asset_id}:{variant}");
    let m = state
        .video_proxy_cancel
        .lock()
        .map_err(|_| AppError::msg("取消注册表锁中毒"))?;
    if let Some(flag) = m.get(&key) {
        flag.store(true, std::sync::atomic::Ordering::Relaxed);
    }
    Ok(())
}

/// 清理某个素材的代理缓存（不影响原文件）。
#[tauri::command]
pub fn clear_video_proxy(state: State<AppState>, asset_id: i64) -> AppResult<()> {
    let proxy_dir = state.data_dir.join("proxies");
    video_proxy::delete_proxy_for_asset(&state.db, &proxy_dir, asset_id)
}

/// 代理缓存统计（§6.7「视频代理缓存：占用、数量」）：ready 文件数、磁盘占用字节。
#[tauri::command]
pub fn video_proxy_cache_stats(state: State<AppState>) -> AppResult<(i64, u64)> {
    let proxy_dir = state.data_dir.join("proxies");
    video_proxy::proxy_cache_stats(&state.db, &proxy_dir)
}

/// 清理全部视频代理缓存（不影响原文件）。返回删除的文件数。
#[tauri::command]
pub fn clear_all_video_proxies(state: State<AppState>) -> AppResult<u64> {
    let proxy_dir = state.data_dir.join("proxies");
    video_proxy::clear_all_proxies(&state.db, &proxy_dir)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// P1-03：variant=None → 后端平台默认（Linux vp8_webm，Windows/macOS h264_mp4）。
    #[test]
    fn resolve_variant_none_uses_platform_default() {
        let (s, v) = resolve_variant(None).unwrap();
        let expected = crate::services::platform::preferred_video_proxy();
        assert_eq!(s, expected);
        assert_eq!(v, video::ProxyVariant::parse(expected).unwrap());
        if cfg!(target_os = "linux") {
            assert_eq!(s, "vp8_webm");
        } else {
            assert_eq!(s, "h264_mp4");
        }
    }

    /// P1-03：显式变体优先，且严格枚举——未知值拒绝。
    #[test]
    fn resolve_variant_explicit_is_validated() {
        assert_eq!(
            resolve_variant(Some("vp8_webm".into())).unwrap().0,
            "vp8_webm"
        );
        assert_eq!(
            resolve_variant(Some("h264_mp4".into())).unwrap().0,
            "h264_mp4"
        );
        assert!(resolve_variant(Some("h265_hevc".into())).is_err());
        assert!(resolve_variant(Some("".into())).is_err());
    }
}
