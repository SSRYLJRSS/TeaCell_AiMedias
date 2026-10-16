//! 媒体元数据后台回填（指导书 §7.5）：对批量素材「重新读取媒体属性」，可取消、可失败隔离、有进度。
//! 探测在锁外进行（ffprobe/图片解码不持 DB 锁）；只对单行做短锁回写。
//! 单个坏文件不回退整个批次（记录 metadata_error）。

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde::Serialize;

use super::{exif_meta, video};
use crate::db::assets::{self, Asset, MediaProbeUpdate};
use crate::error::{AppError, AppResult};
use crate::state::Database;

/// 回填互斥闸 RAII（FX-12）：Drop 时释放，保证 panic / 提前 return 都不会永久占闸。
pub(crate) struct RefillGateGuard(pub Arc<AtomicBool>);
impl Drop for RefillGateGuard {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// 抢互斥闸：已有回填在跑时返回 None（不静默复位对方的取消标志）。
pub(crate) fn try_acquire_gate(gate: &Arc<AtomicBool>) -> Option<RefillGateGuard> {
    gate.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .ok()
        .map(|_| RefillGateGuard(Arc::clone(gate)))
}

/// FB2-08（FX-11）：抢互斥闸后跑色板回算；闸已被占用时返回 None（调用方决定是否提示用户）。
/// 导入后置等场景用：失败被包在返回值里，不 panic、不阻塞调用方。
pub fn try_rescan_palette_exclusive(
    gate: &Arc<AtomicBool>,
    db: &Arc<Database>,
    ids: &[i64],
    cancel: &AtomicBool,
    on_progress: impl FnMut(&RefillProgress),
) -> Option<AppResult<RefillSummary>> {
    let _guard = try_acquire_gate(gate)?;
    // 持闸后才重置取消标志：上一轮被用户取消过时标志仍为 true，
    // 不重置会让本轮在第一个素材前就 break（表现为"入库后色板一个都没算"）。
    cancel.store(false, Ordering::Relaxed);
    Some(rescan_assets_palette(db, ids, cancel, on_progress))
}

/// 一次探测的归一化结果（图片/视频共用）。error 非空 = 读取失败（可辨识原因）。
#[derive(Debug, Default, Clone)]
pub struct MediaMetadata {
    pub media_kind: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    pub container_format: Option<String>,
    pub video_profile: Option<String>,
    pub pixel_format: Option<String>,
    pub frame_rate: Option<f64>,
    pub rotation: Option<i64>,
    pub media_metadata_json: Option<String>,
    pub camera: Option<String>,
    pub lens: Option<String>,
    pub iso: Option<i64>,
    pub aperture: Option<f64>,
    pub shutter: Option<String>,
    pub focal: Option<f64>,
    pub taken_at: Option<i64>,
    /// GPS 定位（带符号十进制度；图片来自 EXIF，视频来自 ffprobe format.tags 的 ISO 6709）
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RefillProgress {
    pub done: i64,
    pub total: i64,
    pub success: i64,
    pub failed: i64,
    pub skipped: i64,
    pub current_id: i64,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct RefillSummary {
    pub total: i64,
    pub success: i64,
    pub failed: i64,
    pub skipped: i64,
    /// FB4-03（§5.6）：本轮成功执行 set_palette 的素材 id（元数据命令忽略；色板命令转成 updatedIds）。
    /// 只在写库成功后 push，失败/跳过/仅被扫描的素材不得进入。
    #[serde(skip_serializing)]
    pub updated_ids: Vec<i64>,
}

/// 探测单个素材（视频走 ffprobe；图片读尺寸 + EXIF；其余无法识别 → error）。
pub fn probe_asset(asset: &Asset) -> MediaMetadata {
    if asset.mime_type.starts_with("video/") {
        match video::probe(Path::new(&asset.file_path)) {
            Ok(vm) => MediaMetadata {
                media_kind: "video".into(),
                width: vm.width,
                height: vm.height,
                duration_ms: vm.duration_ms,
                video_codec: vm.video_codec,
                audio_codec: vm.audio_codec,
                container_format: vm.container_format,
                video_profile: vm.video_profile,
                pixel_format: vm.pixel_format,
                frame_rate: vm.frame_rate,
                rotation: vm.rotation,
                media_metadata_json: vm.raw_json,
                latitude: vm.latitude,
                longitude: vm.longitude,
                taken_at: vm.taken_at,
                error: None,
                ..Default::default()
            },
            Err(e) => MediaMetadata {
                media_kind: "video".into(),
                error: Some(e.to_string()),
                ..Default::default()
            },
        }
    } else if asset.mime_type.starts_with("image/") {
        let mut m = MediaMetadata {
            media_kind: "image".into(),
            ..Default::default()
        };
        if let Ok((w, h)) = image::image_dimensions(Path::new(&asset.file_path)) {
            m.width = Some(w as i64);
            m.height = Some(h as i64);
        }
        let ex = exif_meta::extract(Path::new(&asset.file_path));
        m.camera = ex.camera;
        m.lens = ex.lens;
        m.iso = ex.iso;
        m.aperture = ex.aperture;
        m.shutter = ex.shutter;
        m.focal = ex.focal;
        m.taken_at = ex.taken_at;
        m.latitude = ex.latitude;
        m.longitude = ex.longitude;
        if m.width.is_none() {
            m.error = Some("无法读取图片尺寸".into());
        }
        m
    } else {
        MediaMetadata {
            media_kind: "unknown".into(),
            error: Some("无法识别的媒体类型".into()),
            ..Default::default()
        }
    }
}

fn to_probe_update(m: &MediaMetadata) -> MediaProbeUpdate {
    MediaProbeUpdate {
        media_kind: Some(m.media_kind.clone()),
        width: m.width,
        height: m.height,
        duration_ms: m.duration_ms,
        video_codec: m.video_codec.clone(),
        audio_codec: m.audio_codec.clone(),
        container_format: m.container_format.clone(),
        video_profile: m.video_profile.clone(),
        pixel_format: m.pixel_format.clone(),
        frame_rate: m.frame_rate,
        rotation: m.rotation,
        media_metadata_json: m.media_metadata_json.clone(),
        metadata_version: if m.error.is_none() { Some(1) } else { None },
        error: m.error.clone(),
    }
}

/// 对一组素材做回填（可注入 probe 以便单测）。取消检查在每个素材之间。
/// 探测在锁外进行：每行只短暂加锁读素材 + 回写结果，ffprobe/图片解码不持 DB 锁。
/// 单个素材失败不终止整个批次：记录 metadata_error，计入 failed。
pub fn rescan_assets_with(
    db: &Arc<Database>,
    asset_ids: &[i64],
    cancel: &AtomicBool,
    probe: impl Fn(&Asset) -> MediaMetadata,
    mut on_progress: impl FnMut(&RefillProgress),
) -> AppResult<RefillSummary> {
    let mut summary = RefillSummary {
        total: asset_ids.len() as i64,
        ..Default::default()
    };
    for (i, &id) in asset_ids.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        // 短锁读素材
        let asset = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            assets::get_managed(&conn, id).ok() // 素材被删/不可读 → skipped
        };
        let Some(asset) = asset else {
            summary.skipped += 1;
            continue;
        };
        let meta = probe(&asset); // 锁外探测（ffprobe / 图片解码）
        let update = to_probe_update(&meta);
        let ok = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            let r = assets::update_media_metadata(&conn, id, &update).is_ok();
            if r && meta.error.is_none() && meta.media_kind == "image" {
                let ex = assets::ExifPatch {
                    camera: meta.camera.as_deref(),
                    lens: meta.lens.as_deref(),
                    iso: meta.iso,
                    aperture: meta.aperture,
                    shutter: meta.shutter.as_deref(),
                    focal: meta.focal,
                    taken_at: meta.taken_at,
                    latitude: meta.latitude,
                    longitude: meta.longitude,
                };
                let _ = assets::set_exif(&conn, id, &ex);
            }
            // V18：视频重扫时同步补定位与拍摄时间（COALESCE 只补空，不覆盖已有值）
            if r && meta.error.is_none() && meta.media_kind == "video" {
                let _ =
                    assets::set_geo_taken(&conn, id, meta.latitude, meta.longitude, meta.taken_at);
            }
            r
        };
        if ok {
            if meta.error.is_none() {
                summary.success += 1;
            } else {
                summary.failed += 1;
            }
        } else {
            summary.failed += 1;
        }
        on_progress(&RefillProgress {
            done: (i + 1) as i64,
            total: summary.total,
            success: summary.success,
            failed: summary.failed,
            skipped: summary.skipped,
            current_id: id,
        });
    }
    Ok(summary)
}

/// 实际回填：用 `probe_asset` 探测（ffprobe / 图片解码，需真实文件）。
pub fn rescan_assets(
    db: &Arc<Database>,
    asset_ids: &[i64],
    cancel: &AtomicBool,
    on_progress: impl FnMut(&RefillProgress),
) -> AppResult<RefillSummary> {
    rescan_assets_with(db, asset_ids, cancel, probe_asset, on_progress)
}

/// V18 定位/拍摄时间回填的单次探测结果。
#[derive(Debug, Default, Clone)]
pub struct GeoTakenProbe {
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    pub taken_at: Option<i64>,
    /// 非空 = 探测失败（可辨识原因）；为 None 且字段全空 = 源素材无数据（不是错误，计 skipped）
    pub error: Option<String>,
}

/// 探测单个素材的定位/拍摄时间（V18 回填用）：
/// 图片重读 EXIF GPS；视频优先解析已存 ffprobe 原始 JSON（format.tags 的 location/creation_time，
/// 免拉起子进程），仅当存储 JSON 缺失/损坏才重新 ffprobe。
pub fn probe_geo_taken(asset: &Asset) -> GeoTakenProbe {
    if asset.mime_type.starts_with("image/") {
        let ex = exif_meta::extract(Path::new(&asset.file_path));
        GeoTakenProbe {
            latitude: ex.latitude,
            longitude: ex.longitude,
            taken_at: ex.taken_at,
            error: None,
        }
    } else if asset.mime_type.starts_with("video/") {
        if let Some(json) = asset.media_metadata_json.as_deref() {
            if let Ok(meta) = video::parse_ffprobe_json(json.as_bytes()) {
                return GeoTakenProbe {
                    latitude: meta.latitude,
                    longitude: meta.longitude,
                    taken_at: meta.taken_at,
                    error: None,
                };
            }
        }
        match video::probe(Path::new(&asset.file_path)) {
            Ok(vm) => GeoTakenProbe {
                latitude: vm.latitude,
                longitude: vm.longitude,
                taken_at: vm.taken_at,
                error: None,
            },
            Err(e) => GeoTakenProbe {
                error: Some(e.to_string()),
                ..Default::default()
            },
        }
    } else {
        GeoTakenProbe {
            error: Some("无法识别的媒体类型".into()),
            ..Default::default()
        }
    }
}

/// V18 存量回填：GPS 定位 + 视频拍摄时间（仅补空，COALESCE 不覆盖）。
/// 复用 rescan 骨架（短锁读/写 + 取消 + 进度），计数三分：
/// 已齐全/源素材无对应数据 → skipped（不是错误）；探测/写库失败 → failed；实际补写字段 → success。
pub fn rescan_assets_geo_taken_with(
    db: &Arc<Database>,
    asset_ids: &[i64],
    cancel: &AtomicBool,
    probe: impl Fn(&Asset) -> GeoTakenProbe,
    mut on_progress: impl FnMut(&RefillProgress),
) -> AppResult<RefillSummary> {
    let lock = || {
        db.lock()
            .map_err(|_| crate::error::AppError::msg("数据库锁中毒"))
    };
    let mut summary = RefillSummary {
        total: asset_ids.len() as i64,
        ..Default::default()
    };
    for (i, &id) in asset_ids.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let asset = {
            let conn = lock()?;
            assets::get_managed(&conn, id).ok()
        };
        let Some(asset) = asset else {
            summary.skipped += 1;
            emit_progress(&mut on_progress, i + 1, &summary, id);
            continue;
        };
        // R0-2：图片也要补 taken_at（JPG 解析修复前存量全空）
        let need_geo = asset.latitude.is_none();
        let need_taken = asset.taken_at.is_none();
        // 已齐全：不再探测（只补空语义）
        if !need_geo && !need_taken {
            summary.skipped += 1;
            emit_progress(&mut on_progress, i + 1, &summary, id);
            continue;
        }
        let p = probe(&asset); // 锁外探测（EXIF 解析 / ffprobe）
        if p.error.is_some() {
            summary.failed += 1;
            emit_progress(&mut on_progress, i + 1, &summary, id);
            continue;
        }
        // 探测结果未覆盖任何缺失字段：源素材确实无数据 —— 不是错误，计 skipped
        let geo_useful = need_geo && p.latitude.is_some();
        let taken_useful = need_taken && p.taken_at.is_some();
        if !geo_useful && !taken_useful {
            summary.skipped += 1;
            emit_progress(&mut on_progress, i + 1, &summary, id);
            continue;
        }
        let ok = {
            let conn = lock()?;
            assets::set_geo_taken(&conn, id, p.latitude, p.longitude, p.taken_at).is_ok()
        };
        if ok {
            summary.success += 1;
        } else {
            summary.failed += 1;
        }
        emit_progress(&mut on_progress, i + 1, &summary, id);
    }
    Ok(summary)
}

fn emit_progress(
    on_progress: &mut impl FnMut(&RefillProgress),
    done: usize,
    summary: &RefillSummary,
    current_id: i64,
) {
    on_progress(&RefillProgress {
        done: done as i64,
        total: summary.total,
        success: summary.success,
        failed: summary.failed,
        skipped: summary.skipped,
        current_id,
    });
}

/// 实际定位/拍摄时间回填：用 `probe_geo_taken` 探测。
pub fn rescan_assets_geo_taken(
    db: &Arc<Database>,
    asset_ids: &[i64],
    cancel: &AtomicBool,
    on_progress: impl FnMut(&RefillProgress),
) -> AppResult<RefillSummary> {
    rescan_assets_geo_taken_with(db, asset_ids, cancel, probe_geo_taken, on_progress)
}

/// W1-4：图片宽高回填（RAW 存量修复）。复用 rescan 骨架（短锁读/写 + 取消 + 进度）。
/// 探测在锁外：image_dimensions 优先，RAW 扩展名失败走 rawler probe_dimensions。
/// 探测不出宽高（损坏文件等）计 failed；写库失败计 failed；成功计 success。
pub fn rescan_assets_dimensions(
    db: &Arc<Database>,
    asset_ids: &[i64],
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(&RefillProgress),
) -> AppResult<RefillSummary> {
    let lock = || {
        db.lock()
            .map_err(|_| crate::error::AppError::msg("数据库锁中毒"))
    };
    let mut summary = RefillSummary {
        total: asset_ids.len() as i64,
        ..Default::default()
    };
    for (i, &id) in asset_ids.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let asset = {
            let conn = lock()?;
            assets::get_managed(&conn, id).ok()
        };
        let Some(asset) = asset else {
            summary.skipped += 1;
            emit_progress(&mut on_progress, i + 1, &summary, id);
            continue;
        };
        // 已齐全（只补空语义）：不再解码
        if asset.width.is_some() && asset.height.is_some() {
            summary.skipped += 1;
            emit_progress(&mut on_progress, i + 1, &summary, id);
            continue;
        }
        let path = std::path::PathBuf::from(&asset.file_path);
        // 锁外探测：先 image_dimensions，RAW 失败走 rawler
        let dims =
            crate::services::imaging::probe_dimensions(&path).map(|(w, h)| (w as i64, h as i64));
        let Some((w, h)) = dims else {
            summary.failed += 1;
            emit_progress(&mut on_progress, i + 1, &summary, id);
            continue;
        };
        let ok = {
            let conn = lock()?;
            assets::set_dimensions(&conn, id, Some(w), Some(h)).is_ok()
        };
        if ok {
            summary.success += 1;
        } else {
            summary.failed += 1;
        }
        emit_progress(&mut on_progress, i + 1, &summary, id);
    }
    Ok(summary)
}

/// FB2-08（§14.6/14.7）：存量色板回算。复用 rescan_assets_with 的骨架（短锁读/写 + 取消 + 进度）。
/// 取材（FX-13）：图片 placeholder（256px 入库即生成）→ hd → 原图；视频只认 hd 封面。
/// 计数三分（FX-10）：无可信取材/色板为空 → skipped（不是错误）；解码/写库失败 → failed；
/// 成功写入 palette_json + dominant_* → success。
pub fn rescan_assets_palette(
    db: &Arc<Database>,
    asset_ids: &[i64],
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(&RefillProgress),
) -> AppResult<RefillSummary> {
    const PALETTE_VERSION: i64 = 1;
    let lock = || {
        db.lock()
            .map_err(|_| crate::error::AppError::msg("数据库锁中毒"))
    };
    let mut summary = RefillSummary {
        total: asset_ids.len() as i64,
        ..Default::default()
    };
    for (i, &id) in asset_ids.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let asset = {
            let conn = lock()?;
            assets::get_managed(&conn, id).ok()
        };
        let Some(asset) = asset else {
            summary.skipped += 1;
            continue;
        };
        // FX-13：ids scope 由调用方指定，可能包含视频/不可解码素材。取材必须是"真实的素材像素"：
        // 图片走 placeholder（入库即生成，256px 足够）→ hd → 原图；
        // 视频只认 hd 封面（真实抽帧），不用 placeholder —— 后者可能是 write_generic 的 UI 占位图。
        let is_image = asset.mime_type.starts_with("image/");
        let is_video = asset.mime_type.starts_with("video/");
        let src: Option<std::path::PathBuf> = if is_image {
            asset
                .placeholder_path
                .as_deref()
                .map(std::path::PathBuf::from)
                .or_else(|| {
                    asset
                        .hd_thumbnail_path
                        .as_deref()
                        .map(std::path::PathBuf::from)
                })
                .or_else(|| Some(std::path::PathBuf::from(&asset.file_path)))
        } else if is_video {
            asset
                .hd_thumbnail_path
                .as_deref()
                .map(std::path::PathBuf::from)
        } else {
            None
        };
        let Some(src) = src.filter(|p| p.exists()) else {
            summary.skipped += 1; // 没有可信取材 —— 不是错误
            on_progress(&RefillProgress {
                done: (i + 1) as i64,
                total: summary.total,
                success: summary.success,
                failed: summary.failed,
                skipped: summary.skipped,
                current_id: id,
            });
            continue;
        };

        // 锁外计算。WHY 不 acquire：decode_thumb 内部已取全局解码并发闸（imaging.rs），
        // 这里再 acquire 是同一线程双持 permit，会把 4 并发闸压成 2（FX-08）。
        // FX-13：图片走 placeholder 时还要认一下这张图是不是 write_generic 的 UI 占位图 ——
        // 按 mime 过滤只挡住了视频，HEIC/RAW/损坏图片的 placeholder 同样是占位图。
        let decoded = crate::services::imaging::decode_thumb(&src, 100);
        if decoded
            .as_ref()
            .is_some_and(crate::services::thumbnail::looks_like_generic_placeholder)
        {
            summary.skipped += 1; // 取材是 UI 占位图 —— 不是错误，等真实缩略图生成后下轮再算
            on_progress(&RefillProgress {
                done: (i + 1) as i64,
                total: summary.total,
                success: summary.success,
                failed: summary.failed,
                skipped: summary.skipped,
                current_id: id,
            });
            continue;
        }
        let palette = decoded.map(|img| crate::services::palette::compute_palette(&img));

        // FX-10：三分支计数 —— 色板为空是素材本身不适用（skipped，不覆盖已有结果），
        // 解码失败/写库失败才是真错误（failed）。混计会让全库黑白素材显示成大面积失败。
        let outcome = match palette {
            Some(palette) if !palette.is_empty() => {
                let entries: Vec<serde_json::Value> = palette
                    .iter()
                    .map(|e| {
                        serde_json::json!({
                            "hex": e.hex, "r": e.r, "g": e.g, "b": e.b, "ratio": e.ratio
                        })
                    })
                    .collect();
                let json = serde_json::to_string(&entries).unwrap_or_else(|_| "[]".to_string());
                let p0 = &palette[0];
                let (hue, sat, lum) = crate::services::palette::dominant_from_rgb(p0.r, p0.g, p0.b);
                let conn = lock()?;
                if assets::set_palette(&conn, id, &json, PALETTE_VERSION, hue, sat, lum).is_ok() {
                    // FB4-03：只有真实写库成功的 id 才进 updated_ids（失败/跳过不得冒充更新成功）
                    summary.updated_ids.push(id);
                    Ok(())
                } else {
                    Err(()) // 写库失败 = 真错误
                }
            }
            // 色板为空：素材本身无有效彩色（FX-04 回退后极少见，但仍可能：0×0 图）。
            Some(_) => {
                summary.skipped += 1;
                on_progress(&RefillProgress {
                    done: (i + 1) as i64,
                    total: summary.total,
                    success: summary.success,
                    failed: summary.failed,
                    skipped: summary.skipped,
                    current_id: id,
                });
                continue;
            }
            // 解码失败 = 真错误（文件损坏 / 格式不支持）
            None => Err(()),
        };
        match outcome {
            Ok(()) => summary.success += 1,
            Err(()) => summary.failed += 1,
        }
        on_progress(&RefillProgress {
            done: (i + 1) as i64,
            total: summary.total,
            success: summary.success,
            failed: summary.failed,
            skipped: summary.skipped,
            current_id: id,
        });
    }
    Ok(summary)
}

/// W5d（§W5d）：感知哈希存量回填（骨架照抄 palette 回算：读行短锁、解码锁外、进度 + 取消）。
/// 差异点（计划书明确要求）：写库走批量事务，**每 500 条提交一次** —— 解码在锁外，
/// 只在批量落库瞬间短锁，绝不把解码时长压进 DB 锁。
pub fn rescan_assets_phash(
    db: &Arc<Database>,
    asset_ids: &[i64],
    cancel: &AtomicBool,
    mut on_progress: impl FnMut(&RefillProgress),
) -> AppResult<RefillSummary> {
    const BATCH: usize = 500;
    let lock = || {
        db.lock()
            .map_err(|_| crate::error::AppError::msg("数据库锁中毒"))
    };
    let mut summary = RefillSummary {
        total: asset_ids.len() as i64,
        ..Default::default()
    };
    // 累积待写 (id, phash)，攒满 BATCH 一次性短锁落库（见 flush_phash_batch）
    let mut pending: Vec<(i64, u64)> = Vec::new();

    for (i, &id) in asset_ids.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let asset = {
            let conn = lock()?;
            assets::get_managed(&conn, id).ok()
        };
        let Some(asset) = asset else {
            summary.skipped += 1;
            continue;
        };
        if !asset.mime_type.starts_with("image/") {
            summary.skipped += 1;
            continue;
        }
        // 取材：placeholder（入库即生成，已解码像素的最小载体）→ hd → 原图
        let src = asset
            .placeholder_path
            .as_deref()
            .map(std::path::PathBuf::from)
            .or_else(|| {
                asset
                    .hd_thumbnail_path
                    .as_deref()
                    .map(std::path::PathBuf::from)
            })
            .or_else(|| Some(std::path::PathBuf::from(&asset.file_path)));
        let Some(src) = src.filter(|p| p.exists()) else {
            summary.skipped += 1;
            on_progress(&RefillProgress {
                done: (i + 1) as i64,
                total: summary.total,
                success: summary.success,
                failed: summary.failed,
                skipped: summary.skipped,
                current_id: id,
            });
            continue;
        };
        // 锁外解码（decode_thumb 内部已取全局并发闸，这里不再 acquire —— FX-08）
        let decoded = crate::services::imaging::decode_thumb(&src, 128);
        let Some(phash) = decoded.map(|img| crate::services::perceptual::dhash(&img)) else {
            summary.failed += 1;
            on_progress(&RefillProgress {
                done: (i + 1) as i64,
                total: summary.total,
                success: summary.success,
                failed: summary.failed,
                skipped: summary.skipped,
                current_id: id,
            });
            continue;
        };
        if phash == 0 {
            // 全纯色/无差分图：0 是 set_phash 的哨兵，跳过（不是错误）
            summary.skipped += 1;
            continue;
        }
        summary.success += 1;
        pending.push((id, phash));
        if pending.len() >= BATCH {
            flush_phash_batch(db, &mut summary, &mut pending)?;
        }
        on_progress(&RefillProgress {
            done: (i + 1) as i64,
            total: summary.total,
            success: summary.success,
            failed: summary.failed,
            skipped: summary.skipped,
            current_id: id,
        });
    }
    flush_phash_batch(db, &mut summary, &mut pending)?;
    Ok(summary)
}

/// 批量落库一批 phash：单事务写入 → commit（失败整体回滚）→ 计数归位。
/// decode 阶段已把整批计进 success，这里把写库失败的条目扣回 success 转记 failed；
/// updated_ids 只收真实写库成功的 id（与 palette 命令语义一致，绝不虚报）。
fn flush_phash_batch(
    db: &Arc<Database>,
    summary: &mut RefillSummary,
    pending: &mut Vec<(i64, u64)>,
) -> AppResult<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let conn = db
        .lock()
        .map_err(|_| crate::error::AppError::msg("数据库锁中毒"))?;
    let tx = conn.unchecked_transaction()?;
    let mut ok_ids: Vec<i64> = Vec::new();
    for &(id, phash) in pending.iter() {
        if assets::set_phash(&tx, id, phash).is_ok() {
            ok_ids.push(id);
        }
    }
    tx.commit()?;
    let n = pending.len();
    let written = ok_ids.len();
    summary.success -= (n - written) as i64;
    summary.failed += (n - written) as i64;
    summary.updated_ids.extend(ok_ids);
    pending.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::init_memory;
    use rusqlite::Connection;

    fn db() -> Arc<Database> {
        Arc::new(Database::new(init_memory().unwrap()))
    }

    fn mark_managed_fixture(c: &Connection, asset_id: i64) {
        c.execute(
            "UPDATE libraries
                SET health='ready',root_path='C:/fixture-root',root_identity='fixture-root',
                    library_epoch=lower(hex(randomblob(16)))
              WHERE singleton=1 AND (health!='ready' OR root_identity IS NULL)",
            [],
        )
        .unwrap();
        let library = crate::db::libraries::current(c).unwrap().unwrap();
        c.execute(
            "UPDATE managed_asset_locations
                SET status='verified',reason='verified',library_id=?1,library_epoch=?2,
                    relative_path=printf('asset-%d',asset_id),
                    file_identity='fixture-file',content_sha256=?3,
                    ownership_receipt='fixture-receipt'
              WHERE asset_id=?4",
            rusqlite::params![
                library.library_id,
                library.library_epoch,
                "a".repeat(64),
                asset_id
            ],
        )
        .unwrap();
    }

    fn insert_asset(c: &Connection, id_name: &str, mime: &str, duration: Option<i64>) -> i64 {
        c.execute(
            "INSERT INTO assets (file_path, file_name, file_ext, file_size, mime_type, duration_ms, created_at, modified_at)
             VALUES (?1, ?2, ?3, 100, ?4, ?5, 1, 1)",
            rusqlite::params![id_name, "a", "mp4", mime, duration],
        )
        .unwrap();
        let id = c
            .query_row("SELECT id FROM assets WHERE file_path=?1", [id_name], |r| {
                r.get(0)
            })
            .unwrap();
        mark_managed_fixture(c, id);
        id
    }

    #[test]
    fn list_video_ids_needing_metadata_only_videos() {
        let db = db();
        let c = db.lock().unwrap();
        let vid = insert_asset(&c, "/v1.mp4", "video/mp4", None); // 缺 metadata
        let vid2 = insert_asset(&c, "/v2.mp4", "video/mp4", Some(5000)); // 已有 duration 但 video_codec=null
        let img = insert_asset(&c, "/i.jpg", "image/jpeg", None);
        let ids = assets::list_video_ids_needing_metadata(&c).unwrap();
        assert!(ids.contains(&vid));
        assert!(ids.contains(&vid2));
        assert!(!ids.contains(&img));
    }

    #[test]
    fn rescan_success_and_failed_counting() {
        let db = db();
        let c = db.lock().unwrap();
        let vid = insert_asset(&c, "/v1.mp4", "video/mp4", None);
        let img = insert_asset(&c, "/i.jpg", "image/jpeg", None);
        let ids = vec![vid, img];
        drop(c);
        let cancel = AtomicBool::new(false);
        // 注入 probe：video 模拟成功有 duration，图片模拟失败
        let summary = rescan_assets_with(
            &db,
            &ids,
            &cancel,
            |a| {
                if a.mime_type.starts_with("video/") {
                    MediaMetadata {
                        media_kind: "video".into(),
                        duration_ms: Some(12345),
                        width: Some(1920),
                        height: Some(1080),
                        error: None,
                        ..Default::default()
                    }
                } else {
                    MediaMetadata {
                        media_kind: "image".into(),
                        error: Some("图片损坏".into()),
                        ..Default::default()
                    }
                }
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(summary.total, 2);
        assert_eq!(summary.success, 1);
        assert_eq!(summary.failed, 1);
        assert_eq!(summary.skipped, 0);
        // 视频写回持久化
        let c = db.lock().unwrap();
        let a = assets::get(&c, vid).unwrap();
        assert_eq!(a.duration_ms, Some(12345));
        assert_eq!(a.media_kind.as_deref(), Some("video"));
        assert_eq!(a.metadata_error.as_deref(), None);
        // 图片写回失败原因
        let b = assets::get(&c, img).unwrap();
        assert_eq!(b.metadata_error.as_deref(), Some("图片损坏"));
    }

    #[test]
    fn cancel_stops_after_current() {
        let db = db();
        let c = db.lock().unwrap();
        let a = insert_asset(&c, "/v1.mp4", "video/mp4", None);
        let b = insert_asset(&c, "/v2.mp4", "video/mp4", None);
        let ids = vec![a, b];
        drop(c);
        let cancel = AtomicBool::new(false);
        let mut calls = 0;
        let summary = rescan_assets_with(
            &db,
            &ids,
            &cancel,
            |_| {
                cancel.store(true, Ordering::Relaxed); // 处理第一个后即取消
                MediaMetadata {
                    media_kind: "video".into(),
                    duration_ms: Some(1),
                    error: None,
                    ..Default::default()
                }
            },
            |_| calls += 1,
        )
        .unwrap();
        // 取消后只探了第一个
        assert_eq!(summary.success, 1);
        assert_eq!(calls, 1);
    }

    /// FX-13：图片的 placeholder 是 write_generic 的 UI 占位图时必须计 skipped，
    /// dominant_* 保持 NULL —— 否则按颜色检索会被占位色（灰白 + 品牌色）污染，
    /// 且 palette_json 一旦非空，missing scope 就永远不会重算这一行。
    #[test]
    fn palette_rescan_skips_generic_placeholder_images() {
        let dir = std::env::temp_dir().join(format!("bg_refill_generic_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let ph = dir.join("generic.webp");
        crate::services::thumbnail::ThumbnailService::write_generic(&ph, false);

        let db = db();
        let id = {
            let c = db.lock().unwrap();
            // HEIC 之类"mime 是图片但解不出来"的素材：placeholder 落到通用占位图
            let id = insert_asset(&c, "/broken.heic", "image/heic", None);
            assets::set_placeholder_path(&c, id, &ph.to_string_lossy()).unwrap();
            id
        };

        let cancel = AtomicBool::new(false);
        let summary = rescan_assets_palette(&db, &[id], &cancel, |_| {}).unwrap();
        assert_eq!(summary.skipped, 1, "占位图取材应计 skipped");
        assert_eq!(summary.success, 0);
        assert_eq!(summary.failed, 0);

        let a = assets::get(&db.lock().unwrap(), id).unwrap();
        assert!(a.palette.is_none(), "不得写入占位图的色板");
        assert!(a.dominant_hue.is_none(), "dominant_hue 必须保持 NULL");

        std::fs::remove_dir_all(&dir).ok();
    }

    // ── V18 定位 / 拍摄时间回填 ──

    /// 注入 probe 返回数据：缺字段的素材补写成功，已齐全的计 skipped，不覆盖已有值。
    #[test]
    fn geo_taken_rescan_fills_missing_and_skips_complete() {
        let db = db();
        let (img, vid, vid_done) = {
            let c = db.lock().unwrap();
            let img = insert_asset(&c, "/i.jpg", "image/jpeg", None);
            let vid = insert_asset(&c, "/v.mp4", "video/mp4", None);
            let vid_done = insert_asset(&c, "/v2.mp4", "video/mp4", None);
            assets::set_geo_taken(&c, vid_done, Some(1.0), Some(2.0), Some(123)).unwrap();
            (img, vid, vid_done)
        };
        let cancel = AtomicBool::new(false);
        let summary = rescan_assets_geo_taken_with(
            &db,
            &[img, vid, vid_done],
            &cancel,
            |_| GeoTakenProbe {
                latitude: Some(30.25),
                longitude: Some(120.16),
                taken_at: Some(1_710_484_200_000),
                error: None,
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(summary.total, 3);
        assert_eq!(summary.success, 2);
        assert_eq!(summary.skipped, 1, "已齐全的视频应计 skipped");
        assert_eq!(summary.failed, 0);
        let c = db.lock().unwrap();
        let a = assets::get(&c, img).unwrap();
        assert_eq!(a.latitude, Some(30.25));
        assert_eq!(a.longitude, Some(120.16));
        let v = assets::get(&c, vid).unwrap();
        assert_eq!(
            v.taken_at,
            Some(1_710_484_200_000),
            "视频 taken_at 应被补上"
        );
        let d = assets::get(&c, vid_done).unwrap();
        assert_eq!(d.latitude, Some(1.0), "已有值不得被覆盖");
        assert_eq!(d.taken_at, Some(123), "已有 taken_at 不得被覆盖");
    }

    /// 探测无任何数据（源素材真没有）→ 计 skipped 而非 failed；探测报错 → failed。
    #[test]
    fn geo_taken_rescan_no_data_skipped_probe_error_failed() {
        let db = db();
        let (img, vid) = {
            let c = db.lock().unwrap();
            (
                insert_asset(&c, "/i.jpg", "image/jpeg", None),
                insert_asset(&c, "/v.mp4", "video/mp4", None),
            )
        };
        let cancel = AtomicBool::new(false);
        let summary = rescan_assets_geo_taken_with(
            &db,
            &[img, vid],
            &cancel,
            |a| {
                if a.mime_type.starts_with("image/") {
                    GeoTakenProbe::default() // 无数据 → skipped
                } else {
                    GeoTakenProbe {
                        error: Some("ffprobe 失败".into()),
                        ..Default::default()
                    }
                }
            },
            |_| {},
        )
        .unwrap();
        assert_eq!(summary.skipped, 1, "源素材无定位应计 skipped 而非 failed");
        assert_eq!(summary.failed, 1, "探测报错应计 failed");
        assert_eq!(summary.success, 0);
    }

    /// probe_geo_taken 视频分支：优先解析已存 ffprobe 原始 JSON（location/creation_time），免拉子进程。
    #[test]
    fn probe_geo_taken_video_prefers_stored_json() {
        let json = r#"{"format":{"tags":{"location":"+30.2500+120.1670/","creation_time":"2024-03-15T06:30:00Z"}}}"#;
        let db = db();
        let id = {
            let c = db.lock().unwrap();
            insert_asset(&c, "/v.mp4", "video/mp4", None)
        };
        {
            let c = db.lock().unwrap();
            let u = assets::MediaProbeUpdate {
                media_metadata_json: Some(json.to_string()),
                ..Default::default()
            };
            assets::update_media_metadata(&c, id, &u).unwrap();
        }
        let a = assets::get(&db.lock().unwrap(), id).unwrap();
        let p = probe_geo_taken(&a);
        assert!(p.error.is_none());
        assert_eq!(p.latitude, Some(30.25));
        assert_eq!(p.longitude, Some(120.167));
        assert_eq!(p.taken_at, Some(1_710_484_200_000));
    }
    /// W5d：phash 回填 —— 可解码图片写入 dHash，视频/坏文件失败或跳过。
    #[test]
    fn phash_rescan_writes_for_decodable_images() {
        use image::{Rgb, RgbImage};
        let dir = std::env::temp_dir().join(format!("bg_phash_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // 左黑右白的渐变图（dHash 应产出非 0 值）
        let png = dir.join("grad.png");
        let mut im = RgbImage::new(64, 32);
        for (_x, _y, p) in im.enumerate_pixels_mut() {
            // 左白右黑：差分边界处 255>0 → dHash 置位（全黑/全白/左黑右白差分恒 false → hash 0 会被哨兵跳过）
            *p = Rgb([255, 255, 255]);
        }
        for (x, _y, p) in im.enumerate_pixels_mut() {
            if x >= 32 {
                *p = Rgb([0, 0, 0]);
            }
        }
        im.save(&png).unwrap();

        let db = db();
        let c = db.lock().unwrap();
        let img_id = insert_asset(&c, png.to_str().unwrap(), "image/png", None);
        let vid_id = insert_asset(&c, "/nope.mp4", "video/mp4", None);
        // 存在但无法解码的坏文件 → failed（不存在=跳过，测不了 failed 分支）
        let bad = dir.join("broken.jpg");
        std::fs::write(&bad, b"this is not a jpeg").unwrap();
        let bad_id = insert_asset(&c, bad.to_str().unwrap(), "image/jpeg", None);
        drop(c);
        let cancel = AtomicBool::new(false);
        let summary = rescan_assets_phash(&db, &[img_id, vid_id, bad_id], &cancel, |_| {}).unwrap();
        assert_eq!(summary.total, 3);
        assert_eq!(summary.success, 1, "只有可解码图片写入 phash");
        assert_eq!(summary.failed, 1, "坏图片解码失败");
        assert_eq!(summary.skipped, 1, "视频跳过");
        assert_eq!(summary.updated_ids, vec![img_id]);

        let c = db.lock().unwrap();
        let a = assets::get(&c, img_id).unwrap();
        assert!(a.phash.is_some_and(|p| p > 0), "渐变图应有非 0 phash");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
