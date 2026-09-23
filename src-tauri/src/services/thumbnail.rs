//! 双层缩略图（架构 §1.6）：
//! 占位层 = 入库时生成（256px webp；视频抽帧；HEIC/解码失败 → 通用类型占位图，网格永不空白）；
//! 高清层 = 浏览可见区按需生成（512px webp / 视频封面帧）并缓存，LRU 清理。
//! 注：EXIF 内嵌缩略图快速通道已评估放弃（kamadak-exif 不提供字节提取，需手解 TIFF 段，
//! 性价比低；image crate 直接解码缩到 256px 实测可接受）——决策日志 2026-08-08。

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use image::ImageEncoder;
use rusqlite::Connection;

use super::{imaging, video};
use crate::db::assets;
use crate::error::{AppError, AppResult};

pub const PLACEHOLDER_SIZE: u32 = 256;
pub const HD_SIZE: u32 = 512;
/// 缩略图解码修复后提升缓存代次，避免继续命中历史纯黑 RAW WebP。
const IMAGE_HD_CACHE_VERSION: &str = "v2";
/// 占位图修复代次。旧版本可能已经把解码失败结果固化成通用灰色图，
/// 修复时使用新文件名，避免 WebView/asset 协议继续命中同一路径的旧缓存。
const IMAGE_PLACEHOLDER_REPAIR_VERSION: &str = "v2";

/// 判断数据库中的图片高清缓存是否属于当前解码代次。
#[cfg(test)]
pub(crate) fn is_current_image_hd_cache_path(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.ends_with(&format!("_{IMAGE_HD_CACHE_VERSION}.webp")))
}

/// 通用占位图的三个颜色（`write_generic`）。暖灰底 + 中央色块（视频强调色 / 图片灰）。
/// 公开是给 FX-13 的取材校验用：色板回算必须能认出"这张图是 UI 占位图而不是素材"。
pub const GENERIC_BG: image::Rgba<u8> = image::Rgba([233, 233, 231, 255]);
pub const GENERIC_BLOCK_VIDEO: image::Rgba<u8> = image::Rgba([35, 131, 226, 255]);
pub const GENERIC_BLOCK_IMAGE: image::Rgba<u8> = image::Rgba([120, 119, 116, 255]);

/// B05：hd 缩略图生成计数器，每 LRU_CHECK_INTERVAL 次触发一次 LRU 清理
static HD_GEN_COUNT: AtomicU64 = AtomicU64::new(0);
const LRU_CHECK_INTERVAL: u64 = 100;

/// 指导书 §9.4.1 single-flight：按 `asset_id:size` 去重，同一时刻只执行一个生成任务。
/// 每个 key 一把互斥锁；后续调用方在锁上等待，锁释放后再二次检查文件已存在则直接复用。
static HD_INFLIGHT: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();

fn hd_inflight() -> &'static Mutex<HashMap<String, Arc<Mutex<()>>>> {
    HD_INFLIGHT.get_or_init(|| Mutex::new(HashMap::new()))
}

/// 生成一个与目标同目录、同扩展名（保留 .webp/.jpg）的临时路径，用于「写入临时文件 → 原子 rename」。
fn temp_path_for(out: &Path, uid: &str) -> PathBuf {
    let stem = out.file_stem().and_then(|s| s.to_str()).unwrap_or("thumb");
    let ext = out.extension().and_then(|s| s.to_str()).unwrap_or("bin");
    let dir = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    dir.join(format!("{stem}.{uid}.{ext}"))
}

/// 原子生成：`gen` 写入临时文件，成功后再 rename 到正式路径，杜绝正式路径出现半成品。
/// 失败/异常清理临时文件；Windows 下目标被占用（WebView/杀软）时重试数次并给出可解释失败。
fn atomic_generate(out: &Path, gen: impl FnOnce(&Path) -> bool) -> bool {
    let uid = uuid::Uuid::new_v4().to_string();
    let tmp = temp_path_for(out, &uid);
    let ok = gen(&tmp);
    if !ok {
        let _ = fs::remove_file(&tmp);
        return false;
    }
    // 同文件系统内 rename 原子；Windows：目标可能正被 WebView/杀软读取 → 重试
    let mut last_err = None;
    for _ in 0..3 {
        match fs::rename(&tmp, out) {
            Ok(()) => return true,
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
    let _ = fs::remove_file(&tmp);
    tracing::warn!(
        "缩略图原子 rename 失败（目标可能被占用）：{} 最后错误：{:?}",
        out.display(),
        last_err
    );
    false
}

/// 按 key 执行 single-flight：同一 key 同一时刻只有一个任务在锁内执行。
fn with_single_flight(key: &str, f: impl FnOnce() -> AppResult<PathBuf>) -> AppResult<PathBuf> {
    let lock = {
        let mut map = hd_inflight().lock().unwrap_or_else(|e| e.into_inner());
        map.entry(key.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    };
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
    f()
}

#[derive(Debug, Clone)]
pub struct ThumbnailService {
    placeholder_dir: PathBuf,
    hd_dir: PathBuf,
}

impl ThumbnailService {
    pub fn new(data_dir: &Path) -> AppResult<Self> {
        let svc = Self {
            placeholder_dir: data_dir.join("thumbnails").join("placeholder"),
            hd_dir: data_dir.join("thumbnails").join("hd"),
        };
        fs::create_dir_all(&svc.placeholder_dir)?;
        fs::create_dir_all(&svc.hd_dir)?;
        Ok(svc)
    }

    pub fn placeholder_path(&self, asset_id: i64) -> PathBuf {
        self.placeholder_dir.join(format!("{asset_id}.webp"))
    }

    fn repaired_placeholder_path(&self, asset_id: i64) -> PathBuf {
        self.placeholder_dir.join(format!(
            "{asset_id}_{IMAGE_PLACEHOLDER_REPAIR_VERSION}.webp"
        ))
    }

    fn image_hd_path(&self, asset_id: i64, size: u32) -> PathBuf {
        self.hd_dir
            .join(format!("{asset_id}_{size}_{IMAGE_HD_CACHE_VERSION}.webp"))
    }

    // ── 占位层 ──

    /// 提取/生成占位图（永不失败：全链路失败 → 通用类型占位图）。
    /// W5d：返回 (占位图路径, dHash) —— 图片解码成功时顺带算出感知哈希（零额外解码），
    /// 视频/失败为 None。入库用它搭车写 assets.phash（回填命令照抄 palette 骨架）。
    pub fn extract_placeholder(
        &self,
        asset_id: i64,
        src: &Path,
        mime_type: &str,
    ) -> (PathBuf, Option<u64>) {
        let original_out = self.placeholder_path(asset_id);
        let out = if original_out.exists() && placeholder_is_unusable(&original_out) {
            self.repaired_placeholder_path(asset_id)
        } else {
            original_out
        };
        if out.exists() && !placeholder_is_unusable(&out) {
            return (out, None);
        }
        let is_video = mime_type.starts_with("video/");
        let mut phash_out: Option<u64> = None;
        let ok = if mime_type.starts_with("image/") {
            atomic_generate(&out, |tmp| {
                let (ok, ph) = imaging::write_thumb_with_phash(src, tmp, PLACEHOLDER_SIZE);
                phash_out = ph;
                ok
            })
        } else if is_video {
            let _permit = imaging::acquire();
            atomic_generate(&out, |tmp| {
                video::extract_frame(src, 0, tmp, PLACEHOLDER_SIZE)
            })
        } else {
            false
        };
        if !ok {
            tracing::warn!(
                operation = "thumbnail",
                asset_id,
                variant = "placeholder",
                source = %src.display(),
                mime_type,
                reason = "decode_or_write_failed",
                "缩略图生成失败，降级为通用占位图"
            );
            let fallback_ok = atomic_generate(&out, |tmp| {
                Self::write_generic(tmp, is_video);
                true
            });
            if !fallback_ok {
                tracing::error!(
                    operation = "thumbnail",
                    asset_id,
                    variant = "placeholder",
                    source = %src.display(),
                    "通用占位图写入失败"
                );
            }
            return (out, None);
        }
        (out, phash_out)
    }

    /// 取占位图并修复历史的通用灰色缓存。
    ///
    /// 读取素材行只持有短数据库锁；实际解码和文件写入都在锁外完成。
    /// 当旧占位图不可用时，`extract_placeholder` 会写入带修复代次的新路径，
    /// 从而避免浏览器继续复用旧的灰色 URL。
    pub fn get_or_create_placeholder(
        &self,
        db: &std::sync::Arc<std::sync::Mutex<Connection>>,
        asset_id: i64,
    ) -> AppResult<PathBuf> {
        let asset = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            assets::get(&conn, asset_id)?
        };
        let (path, phash) =
            self.extract_placeholder(asset_id, Path::new(&asset.file_path), &asset.mime_type);
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        assets::set_placeholder_path(&conn, asset_id, &path.to_string_lossy())?;
        if let Some(ph) = phash {
            assets::set_phash(&conn, asset_id, ph)?;
        }
        Ok(path)
    }

    // ── 高清层 ──

    /// 获取或按需生成高清缩略图；命中缓存直接返回（调用方负责回填/已回填 hd_thumbnail_path）
    /// 注意：解码全尺寸大图是重活，全局串行闸防止滚动时几十个并发解码打爆 CPU/内存
    /// 取/生成高清缩略图。DB 锁只在读行/写路径时短暂持有，
    /// 解码期间释放（否则排队生成会饿死列表/打标等一切 DB 请求）
    pub fn get_or_create_hd(
        &self,
        db: &std::sync::Arc<std::sync::Mutex<Connection>>,
        asset_id: i64,
        size: Option<u32>,
    ) -> AppResult<PathBuf> {
        // 短暂读库（排队期间可能已被前一个任务生成）
        let (asset, out) = {
            let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
            let asset = assets::get(&conn, asset_id)?;
            let size = size.unwrap_or(HD_SIZE);
            let out = if asset.mime_type.starts_with("video/") {
                self.hd_dir.join(format!("{asset_id}_cover.jpg"))
            } else {
                self.image_hd_path(asset_id, size)
            };
            (asset, out)
        }; // 此处释放 DB 锁
        if out.exists() {
            return Ok(out);
        }
        // 指导书 §9.4.1：single-flight 按 (asset, variant/size) 去重；锁内重新检查文件存在后再生成。
        // 生成期间不持有 DB 锁（decode 在锁外），只短暂读行 + 回写路径。
        let key = out.to_string_lossy().to_string();
        let size = size.unwrap_or(HD_SIZE);
        let asset_for_gen = asset.clone();
        with_single_flight(&key, || {
            let out = out.clone();
            if out.exists() {
                return Ok(out); // 其他调用方已生成
            }
            let src = Path::new(&asset_for_gen.file_path);
            let ok = if asset_for_gen.mime_type.starts_with("video/") {
                let _permit = imaging::acquire();
                let t = asset_for_gen.duration_ms.map(|d| d / 10).unwrap_or(0);
                atomic_generate(&out, |tmp| video::extract_frame(src, t, tmp, size))
            } else {
                atomic_generate(&out, |tmp| imaging::write_thumb(src, tmp, size))
            };
            if ok {
                // B05：回写 hd 路径（短锁）+ 节流触发 LRU 清理（先读 settings 短锁，再锁外清理）
                let cleanup_mb = {
                    let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
                    assets::set_hd_thumbnail_path(&conn, asset_id, &out.to_string_lossy())?;
                    // B05：每生成 100 张触发一次 LRU 清理
                    let n = HD_GEN_COUNT.fetch_add(1, Ordering::Relaxed) + 1;
                    if n.is_multiple_of(LRU_CHECK_INTERVAL) {
                        crate::db::settings::get_settings(&conn)
                            .ok()
                            .map(|s| s.thumbnail_cache_mb)
                    } else {
                        None
                    }
                }; // 释放 DB 锁
                   // B05：锁外执行 LRU 清理（纯文件系统操作，不持锁）
                if let Some(max_mb) = cleanup_mb {
                    let _ = self.cleanup_lru(max_mb);
                }
                Ok(out)
            } else {
                tracing::warn!(
                    operation = "thumbnail",
                    asset_id,
                    variant = "hd",
                    source = %src.display(),
                    size,
                    "高清缩略图生成失败，降级返回占位图"
                );
                // 高清生成失败降级返回占位图，保证前端有图可显
                Ok(self.placeholder_path(asset_id))
            }
        })
    }

    // ── 缓存管理 ──

    /// 高清层 LRU 清理（mtime 最旧优先）；占位层不清理（架构 §1.6③）
    pub fn cleanup_lru(&self, max_mb: i64) -> AppResult<()> {
        let mut entries: Vec<(PathBuf, u64, std::time::SystemTime)> = Vec::new();
        let mut total: u64 = 0;
        let mut skipped = 0u64;
        for entry in fs::read_dir(&self.hd_dir)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    skipped += 1;
                    tracing::warn!(
                        operation = "thumbnail_cache",
                        stage = "lru_scan",
                        error = %e,
                        "缩略图缓存目录项读取失败"
                    );
                    continue;
                }
            };
            let md = match entry.metadata() {
                Ok(md) => md,
                Err(e) => {
                    skipped += 1;
                    tracing::warn!(
                        operation = "thumbnail_cache",
                        stage = "lru_metadata",
                        path = %entry.path().display(),
                        error = %e,
                        "缩略图缓存元数据读取失败"
                    );
                    continue;
                }
            };
            if md.is_file() {
                total += md.len();
                match md.modified() {
                    Ok(modified) => entries.push((entry.path(), md.len(), modified)),
                    Err(e) => {
                        skipped += 1;
                        tracing::warn!(
                            operation = "thumbnail_cache",
                            stage = "lru_mtime",
                            path = %entry.path().display(),
                            error = %e,
                            "缩略图缓存修改时间读取失败"
                        );
                    }
                }
            }
        }
        let budget = (max_mb.max(0) as u64) * 1024 * 1024;
        if total <= budget {
            return Ok(());
        }
        entries.sort_by_key(|(_, _, mtime)| *mtime);
        let mut removed = 0u64;
        let mut remove_failed = 0u64;
        for (path, len, _) in entries {
            if total <= budget {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= len;
                removed += 1;
            } else {
                remove_failed += 1;
            }
        }
        tracing::info!(
            operation = "thumbnail_cache",
            stage = "lru_cleanup",
            removed,
            remove_failed,
            skipped,
            remaining_bytes = total,
            budget_bytes = budget,
            "缩略图 LRU 清理完成"
        );
        Ok(())
    }

    /// 删除素材联动清理两层文件（架构 §1.6③）
    pub fn delete_for_asset(&self, asset_id: i64) {
        let _ = fs::remove_file(self.placeholder_path(asset_id));
        let _ = fs::remove_file(self.repaired_placeholder_path(asset_id));
        if let Ok(rd) = fs::read_dir(&self.hd_dir) {
            let prefix = format!("{asset_id}_");
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(&prefix) {
                    let _ = fs::remove_file(e.path());
                }
            }
        }
    }

    pub fn clear(&self, kind: Option<&str>) -> AppResult<()> {
        let clear_dir = |dir: &Path| -> AppResult<()> {
            if dir.exists() {
                for e in fs::read_dir(dir)?.flatten() {
                    let _ = fs::remove_file(e.path());
                }
            }
            Ok(())
        };
        match kind {
            Some("placeholder") => clear_dir(&self.placeholder_dir)?,
            Some("hd") => clear_dir(&self.hd_dir)?,
            _ => {
                clear_dir(&self.placeholder_dir)?;
                clear_dir(&self.hd_dir)?;
            }
        }
        Ok(())
    }

    // ── 内部 ──

    /// 通用类型占位图：纯色底 + 深色色块区分图片/视频
    pub(crate) fn write_generic(out: &Path, is_video: bool) {
        let (w, h) = (PLACEHOLDER_SIZE, PLACEHOLDER_SIZE);
        let mut img = image::RgbaImage::from_pixel(w, h, GENERIC_BG);
        let block = if is_video {
            GENERIC_BLOCK_VIDEO
        } else {
            GENERIC_BLOCK_IMAGE
        };
        let (bw, bh) = (w / 2, h / 2);
        let (x0, y0) = ((w - bw) / 2, (h - bh) / 2);
        for y in y0..y0 + bh {
            for x in x0..x0 + bw {
                img.put_pixel(x, y, block);
            }
        }
        if let Ok(mut file) = fs::File::create(out) {
            let _ = image::codecs::webp::WebPEncoder::new_lossless(&mut file).write_image(
                img.as_raw(),
                w,
                h,
                image::ExtendedColorType::Rgba8,
            );
        }
    }
}

/// 旧占位图不存在、损坏，或是 `write_generic` 生成的 UI 灰块时，需要重新生成。
fn placeholder_is_unusable(path: &Path) -> bool {
    if !path.is_file() {
        return true;
    }
    match image::open(path) {
        Ok(img) => looks_like_generic_placeholder(&img),
        Err(_) => true,
    }
}

/// FX-13：这张解码结果是不是 `write_generic` 写的 UI 占位图？
///
/// 色板回算取材优先用 placeholder，而 `extract_placeholder` 永不失败 —— HEIC/RAW/损坏图片
/// 都会落到通用占位图（暖灰底 + 中央色块）。拿它算主色会写进一个与素材无关的
/// `dominant_hue`（实测灰白 75% + 品牌色 25%），污染按颜色检索，且因 palette_json 非空
/// 而永远不会被 `missing` scope 重算。仅按 mime 过滤挡不住这条路径。
///
/// 判据：四角是背景色 + 正中是两种色块之一。缩放/编码会在边界引入插值，
/// 但纯色区域内部不受影响，所以只采样这五点并留 ±4 容差。
pub fn looks_like_generic_placeholder(img: &image::DynamicImage) -> bool {
    use image::GenericImageView;

    let (w, h) = img.dimensions();
    if w < 8 || h < 8 {
        return false;
    }
    let near = |p: image::Rgba<u8>, q: image::Rgba<u8>| (0..3).all(|i| p[i].abs_diff(q[i]) <= 4);
    let rgba = img.to_rgba8();
    let corners = [
        rgba.get_pixel(0, 0),
        rgba.get_pixel(w - 1, 0),
        rgba.get_pixel(0, h - 1),
        rgba.get_pixel(w - 1, h - 1),
    ];
    if !corners.iter().all(|p| near(**p, GENERIC_BG)) {
        return false;
    }
    let center = *rgba.get_pixel(w / 2, h / 2);
    near(center, GENERIC_BLOCK_VIDEO) || near(center, GENERIC_BLOCK_IMAGE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// FX-13：write_generic 写出的占位图必须被认出来（无论图片还是视频变体），
    /// 而真实素材（含正好用了近似暖灰的图）不得被误判。
    #[test]
    fn generic_placeholder_is_recognized_but_real_images_are_not() {
        let dir = std::env::temp_dir().join(format!("bg_generic_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();

        for is_video in [true, false] {
            let out = dir.join(format!("generic_{is_video}.webp"));
            ThumbnailService::write_generic(&out, is_video);
            let img = image::open(&out).expect("占位图应可解码");
            assert!(
                looks_like_generic_placeholder(&img),
                "write_generic 的产物必须被认出（is_video={is_video}）"
            );
            // 回算实际拿到的是缩到 100px 的版本，缩放后同样要认出来
            assert!(
                looks_like_generic_placeholder(&img.thumbnail(100, 100)),
                "缩放后仍应被认出（is_video={is_video}）"
            );
        }

        // 纯色暖灰图：四角是背景色但正中不是色块 → 是真实素材，不得误判
        let solid =
            image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(64, 64, GENERIC_BG));
        assert!(!looks_like_generic_placeholder(&solid), "纯色图不是占位图");
        // 普通照片
        let photo = image::DynamicImage::ImageRgba8(image::RgbaImage::from_fn(64, 64, |x, y| {
            image::Rgba([(x * 3) as u8, (y * 3) as u8, 90, 255])
        }));
        assert!(!looks_like_generic_placeholder(&photo), "渐变图不是占位图");

        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn stale_generic_placeholder_is_repaired_to_a_new_path() {
        let dir = tempfile::tempdir().unwrap();
        let svc = ThumbnailService::new(dir.path()).unwrap();
        let source = dir.path().join("source.jpg");
        image::RgbImage::from_fn(320, 240, |x, y| {
            image::Rgb([x as u8, y as u8, (x.wrapping_add(y) % 255) as u8])
        })
        .save(&source)
        .unwrap();

        let old = svc.placeholder_path(42);
        ThumbnailService::write_generic(&old, false);
        let (repaired, phash) = svc.extract_placeholder(42, &source, "image/jpeg");

        assert_eq!(
            repaired.file_name().and_then(|name| name.to_str()),
            Some("42_v2.webp")
        );
        assert!(repaired.exists());
        assert!(!placeholder_is_unusable(&repaired));
        assert!(phash.is_some());
        assert!(old.exists(), "旧缓存保留作回退，不应删除原文件");
    }

    #[test]
    fn temp_path_preserves_extension_and_parent() {
        let dir = std::env::temp_dir();
        let out = dir.join("100_512.webp");
        let tmp = temp_path_for(&out, "abc123");
        assert_eq!(tmp.parent(), Some(dir.as_path()));
        assert!(tmp.extension().is_some());
        assert!(tmp.to_string_lossy().contains("abc123"));
        assert_ne!(tmp, out);
    }

    #[test]
    fn image_hd_cache_uses_new_generation_path() {
        let dir = std::env::temp_dir().join(format!("bg_hd_version_{}", std::process::id()));
        let svc = ThumbnailService::new(&dir).unwrap();
        let path = svc.image_hd_path(7, 1024);

        assert_eq!(path.file_name().unwrap(), "7_1024_v2.webp");
        assert!(is_current_image_hd_cache_path(&path));
        assert!(!is_current_image_hd_cache_path(Path::new(
            "thumbnails/hd/7_1024.webp"
        )));
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn atomic_generate_success_writes_and_renames() {
        let dir = std::env::temp_dir().join(format!("bg_atomic_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let out = dir.join("out.webp");
        // gen 在临时文件里写内容
        let ok = atomic_generate(&out, |tmp| {
            fs::write(tmp, b"hello").unwrap();
            true
        });
        assert!(ok);
        assert!(out.exists());
        assert_eq!(fs::read(&out).unwrap(), b"hello");
        // 无残留 .tmp
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn atomic_generate_failure_leaves_no_file() {
        let dir = std::env::temp_dir().join(format!("bg_atomic_fail_{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let out = dir.join("fail.jpg");
        let ok = atomic_generate(&out, |tmp| {
            fs::write(tmp, b"partial").unwrap();
            false // 模拟生成失败
        });
        assert!(!ok);
        assert!(!out.exists());
        // 失败后清理临时文件
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 0);
        fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn single_flight_serializes_same_key() {
        // 同一 key：并发 3 个调用，只有 1 个能进入临界区（其余等待），用原子计数器验证
        let entered = Arc::new(AtomicU64::new(0));
        let in_crit = Arc::new(AtomicBool::new(false));
        let max_seen = Arc::new(AtomicU64::new(0));
        let mut handles = Vec::new();
        for _ in 0..3 {
            let entered = Arc::clone(&entered);
            let in_crit = Arc::clone(&in_crit);
            let max_seen = Arc::clone(&max_seen);
            handles.push(std::thread::spawn(move || {
                let _ = with_single_flight("a:512", || {
                    entered.fetch_add(1, Ordering::SeqCst);
                    if in_crit.swap(true, Ordering::SeqCst) {
                        // 已在临界区（不应发生）
                        max_seen.fetch_add(1, Ordering::SeqCst);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(30));
                    in_crit.store(false, Ordering::SeqCst);
                    Ok(std::path::PathBuf::from("x"))
                });
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        // 临界区无并发进入
        assert_eq!(max_seen.load(Ordering::SeqCst), 0);
        assert_eq!(entered.load(Ordering::SeqCst), 3);
    }
}
