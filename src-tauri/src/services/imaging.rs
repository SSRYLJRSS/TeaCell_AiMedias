//! 全局统一图像引擎（PRD v2.6）：全应用唯一的图片解码/缩略图实现
//! preview.rs（待入库）与 thumbnail.rs（已入库）都是它的瘦壳。
//!
//! 内嵌预览策略链（开源看图软件标准做法，对标 ExifTool/FastRawViewer）：
//!   1. EXIF IFD1 ThumbnailImage（kamadak-exif；相机 JPEG/TIFF 微秒级）
//!   2. 手写 TIFF 遍历：任意 IFD 的 0x0201/0x0202 + Panasonic JpgFromRaw(0x2E)（RW2/DNG/CR2…）
//!   3. CR3 ISOBMFF box 遍历（meta/iprp/ipco 里的 JPEG item；Phase 2 F03）
//!   4. FFD8..FFD9 标记扫描（取最大 JPEG 块，一切 RAW 兜底）
//!
//! 解码路径：
//!   内嵌图（尺寸达标直接用）→ jpeg-decoder DCT 缩放（1/8~1/1，比全解码快数倍）
//!   → image::open 全解码兜底（PNG/GIF/WebP/BMP）
//! 并发：全局 4 许可信号量（手写 Condvar），替代两处串行 Mutex，吞吐 ×4

use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Condvar, Mutex};

use image::{DynamicImage, GenericImageView, ImageEncoder};

/// AI 视觉请求的稳定输入契约。
///
/// 这两个值属于请求层契约，不应由 UI 缩略图尺寸或用户是否打开过高清预览影响。
pub const AI_IMAGE_MAX_PX: u32 = 1024;
pub const AI_IMAGE_FALLBACK_MAX_PX: u32 = 768;
pub const AI_JPEG_QUALITY: u8 = 85;

/// 已标准化的 AI 图片载荷。所有协议（Ollama/OpenAI/Anthropic）共用同一份 JPEG。
#[derive(Debug, Clone)]
pub struct AiImagePayload {
    pub bytes: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub max_px: u32,
}

/// 解码素材并编码为稳定的 RGB JPEG，避免直接把 UI 高清 WebP 送进模型。
///
/// `decode_thumb` 已统一负责普通图片、HEIC 和 RAW 的分派；这里再统一做透明背景、
/// 尺寸和编码，确保请求 MIME 与实际字节一致。失败返回 `None`，由 AI 服务层给出
/// 面向用户的“无法解码素材”错误。
pub fn encode_ai_image(src: &Path, max_px: u32) -> Option<AiImagePayload> {
    let max_px = max_px.max(1);
    let image = decode_thumb(src, max_px)?;
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    if width == 0 || height == 0 {
        return None;
    }

    // JPEG 没有 alpha 通道。使用缩略图服务的中性背景，避免透明 PNG 被黑底编码。
    const BACKGROUND: [u8; 3] = [233, 233, 231];
    let mut rgb = image::RgbImage::new(width, height);
    for (x, y, pixel) in rgba.enumerate_pixels() {
        let [r, g, b, a] = pixel.0;
        let alpha = u16::from(a);
        let inv = u16::from(255_u8.saturating_sub(a));
        let blend = |foreground: u8, background: u8| -> u8 {
            ((u16::from(foreground) * alpha + u16::from(background) * inv + 127) / 255) as u8
        };
        rgb.put_pixel(
            x,
            y,
            image::Rgb([
                blend(r, BACKGROUND[0]),
                blend(g, BACKGROUND[1]),
                blend(b, BACKGROUND[2]),
            ]),
        );
    }

    let mut bytes = Vec::new();
    let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, AI_JPEG_QUALITY);
    encoder
        .write_image(rgb.as_raw(), width, height, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(AiImagePayload {
        bytes,
        width,
        height,
        max_px,
    })
}

/// 读取常规图片或 RAW 文件的尺寸。RAW 由 rawler 兜底，调用方应在数据库锁外调用。
pub fn probe_dimensions(src: &Path) -> Option<(u32, u32)> {
    if let Ok(dims) = image::image_dimensions(src) {
        return Some(dims);
    }
    if is_heic_like(src) {
        let _permit = acquire();
        return super::heic_decode::probe_dimensions(src);
    }
    super::raw_decode::probe_dimensions(src).or_else(|| guessed_dimensions(src))
}

/// 非标准扩展名的 TIFF 容器（如 3FR/IIQ）按内容识别，只读头拿预览尺寸。
fn guessed_dimensions(src: &Path) -> Option<(u32, u32)> {
    image::ImageReader::open(src)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_dimensions()
        .ok()
}

// ---------------------------------------------------------------------------
// 并发许可：最多 4 个解码并行（解码是 CPU 密集活，再多只会互相抢）
// ---------------------------------------------------------------------------

static DECODE_SEM: (Mutex<usize>, Condvar) = (Mutex::new(4), Condvar::new());
const MAX_PERMITS: usize = 4;

pub struct DecodePermit;

impl Drop for DecodePermit {
    fn drop(&mut self) {
        let (m, c) = &DECODE_SEM;
        if let Ok(mut n) = m.lock() {
            *n += 1;
            debug_assert!(*n <= MAX_PERMITS);
        }
        c.notify_one();
    }
}

pub fn acquire() -> DecodePermit {
    let (m, c) = &DECODE_SEM;
    let mut n = m.lock().unwrap_or_else(|e| e.into_inner());
    while *n == 0 {
        n = c.wait(n).unwrap_or_else(|e| e.into_inner());
    }
    *n -= 1;
    DecodePermit
}

// ---------------------------------------------------------------------------
// 内嵌预览提取（策略链）
// ---------------------------------------------------------------------------

/// 从文件里按偏移+长度抠 JPEG：宽容裁剪（前 64 字节内找 SOI、末尾找 EOI）
/// 某些相机内嵌图带填充字节，严格校验会误杀
fn cut_jpeg(src: &Path, offset: u64, len: u64) -> Option<Vec<u8>> {
    if len == 0 || len > 64 * 1024 * 1024 {
        return None;
    }
    let mut f = std::fs::File::open(src).ok()?;
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf).ok()?;
    // 头部 64 字节内定位 SOI
    let soi = buf[..buf.len().min(64)]
        .windows(2)
        .position(|w| w[0] == 0xFF && w[1] == 0xD8)?;
    // 末尾定位最后一个 EOI
    let eoi = buf.windows(2).rposition(|w| w[0] == 0xFF && w[1] == 0xD9)?;
    if eoi <= soi {
        return None;
    }
    Some(buf[soi..eoi + 2].to_vec())
}

/// 定位 TIFF 头：(基准偏移, 是否大端)
/// - TIFF/RAW 容器：base=0（RW2 magic 0x55，标准 0x2A）
/// - JPEG 容器：扫 APP 段找 FFE1+"Exif\0\0"，TIFF 头在其后；IFD 里的偏移全部相对该基准
fn locate_tiff_base(src: &Path) -> Option<(u64, bool)> {
    let mut f = std::fs::File::open(src).ok()?;
    let mut head = [0u8; 4];
    f.read_exact(&mut head).ok()?;
    match &head[0..2] {
        b"II" => return Some((0, false)),
        b"MM" => return Some((0, true)),
        _ => {}
    }
    // JPEG：遍历 marker 段找 Exif APP1
    if head[0] != 0xFF || head[1] != 0xD8 {
        return None;
    }
    f.seek(SeekFrom::Start(2)).ok()?;
    let mut buf = vec![0u8; 256 * 1024];
    let n = f.read(&mut buf).ok()?;
    buf.truncate(n);
    let mut i = 0;
    while i + 4 < buf.len() {
        if buf[i] != 0xFF {
            i += 1;
            continue;
        }
        let marker = buf[i + 1];
        if marker == 0xD8 || marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            i += 2; // 无长度段
            continue;
        }
        if marker == 0xDA {
            break; // SOS：扫描开始，后面没有 APP 段了
        }
        let seg_len = u16::from_be_bytes([buf[i + 2], buf[i + 3]]) as usize;
        if seg_len < 2 || i + 2 + seg_len > buf.len() {
            break;
        }
        if marker == 0xE1 && seg_len >= 8 && &buf[i + 4..i + 10] == b"Exif\0\0" {
            // buf 从文件偏移 2 开始读：TIFF 基准(文件绝对) = i + 2(marker) + 4(段头) + 6(Exif\0\0)
            let base = (i + 12) as u64;
            let be = match &buf[i + 10..i + 12] {
                b"II" => false,
                b"MM" => true,
                _ => return None,
            };
            return Some((base, be));
        }
        i += 2 + seg_len;
    }
    None
}

/// 统一 TIFF 遍历（策略 1+2 合体）：沿 IFD 链找
/// 标准 0x0201/0x0202（JPEGInterchangeFormat）与 Panasonic 0x2E（JpgFromRaw，count 即长度）
fn tiff_embedded_jpeg(src: &Path) -> Option<Vec<u8>> {
    let (base, be) = locate_tiff_base(src)?;
    let mut f = std::fs::File::open(src).ok()?;
    let u16 = |b: &[u8]| -> u16 {
        if be {
            u16::from_be_bytes([b[0], b[1]])
        } else {
            u16::from_le_bytes([b[0], b[1]])
        }
    };
    let u32 = |b: &[u8]| -> u32 {
        if be {
            u32::from_be_bytes([b[0], b[1], b[2], b[3]])
        } else {
            u32::from_le_bytes([b[0], b[1], b[2], b[3]])
        }
    };

    let mut magic_buf = [0u8; 8];
    f.seek(SeekFrom::Start(base)).ok()?;
    f.read_exact(&mut magic_buf).ok()?;
    let magic = u16(&magic_buf[2..4]);
    if magic != 42 && magic != 85 {
        return None;
    }
    let mut ifd_off = u32(&magic_buf[4..8]) as u64;

    // 沿 IFD 链走（IFD0 → IFD1 → …），最多 8 层防坏文件死循环
    for _ in 0..8 {
        if ifd_off == 0 {
            return None;
        }
        f.seek(SeekFrom::Start(base + ifd_off)).ok()?;
        let mut cnt_buf = [0u8; 2];
        f.read_exact(&mut cnt_buf).ok()?;
        let n = u16(&cnt_buf) as usize;
        if n > 512 {
            return None;
        }
        let mut entries = vec![0u8; n * 12];
        f.read_exact(&mut entries).ok()?;

        let mut jpeg_off: Option<u64> = None;
        let mut jpeg_len: Option<u64> = None;
        let mut pana_off: Option<u64> = None;
        let mut pana_len: Option<u64> = None;

        for i in 0..n {
            let e = &entries[i * 12..i * 12 + 12];
            let tag = u16(&e[0..2]);
            let typ = u16(&e[2..4]);
            let count = u32(&e[4..8]) as u64;
            let val = u32(&e[8..12]) as u64;
            match tag {
                0x0201 => jpeg_off = Some(val),
                0x0202 => jpeg_len = Some(val),
                // Panasonic JpgFromRaw：UNDEF(7)，count 即数据长度，value 为偏移
                0x002E if typ == 7 && count > 1024 => {
                    pana_off = Some(val);
                    pana_len = Some(count);
                }
                _ => {}
            }
        }

        // 优先标准 JPEGInterchangeFormat，其次 Panasonic（偏移相对 TIFF 基准）
        if let (Some(o), Some(l)) = (jpeg_off, jpeg_len) {
            if let Some(j) = cut_jpeg(src, base + o, l) {
                return Some(j);
            }
        }
        if let (Some(o), Some(l)) = (pana_off, pana_len) {
            if let Some(j) = cut_jpeg(src, base + o, l) {
                return Some(j);
            }
        }

        // 下一个 IFD
        let mut next = [0u8; 4];
        f.read_exact(&mut next).ok()?;
        ifd_off = u32(&next) as u64;
    }
    None
}

/// marker_scan_jpeg 单次最多扫描的字节数（有界上限，指导书 §5.4）。
/// 禁止 `std::fs::read` 整文件读入：超大 RAW（可达数 GB）会被整体载入内存拖垮。
/// 超过上限即封顶，只扫描前部（内嵌预览通常位于文件头附近）；找不到则返回 None，走通用占位图。
const MARKER_SCAN_MAX_BYTES: usize = 64 * 1024 * 1024;
/// 分块读取块大小（固定块，跨块边界由合并后的连续 buffer 处理）
const MARKER_SCAN_BLOCK: usize = 256 * 1024;

/// 策略 3：FFD8..FFD9 标记扫描（取最大 JPEG 块；一切格式的最后兜底）
/// 有界、分块读取：绝不无界整读；遇到超过上限的大 RAW 只扫描前部。
fn marker_scan_jpeg(src: &Path) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(src).ok()?;
    let file_len = f.metadata().ok()?.len();
    if file_len < 4 {
        return None;
    }
    let scan_end = (file_len as usize).min(MARKER_SCAN_MAX_BYTES);
    // 分块累积到 scan_end（有界），内存开销受 MARKER_SCAN_MAX_BYTES 约束
    let mut data = Vec::with_capacity(scan_end.min(MARKER_SCAN_BLOCK));
    let mut pos = 0usize;
    while pos < scan_end {
        let want = MARKER_SCAN_BLOCK.min(scan_end - pos);
        let mut block = vec![0u8; want];
        if f.read_exact(&mut block).is_err() {
            break;
        }
        data.extend_from_slice(&block);
        pos += want;
    }
    if data.len() < 4 {
        return None;
    }
    let mut best: Option<(usize, usize)> = None; // (start, len)
    let mut i = 0;
    while i + 1 < data.len() {
        if data[i] == 0xFF && data[i + 1] == 0xD8 {
            // 找匹配的 EOI
            let mut j = i + 2;
            while j + 1 < data.len() {
                if data[j] == 0xFF && data[j + 1] == 0xD9 {
                    let len = j + 2 - i;
                    if len > 4096 && best.map(|(_, bl)| len > bl).unwrap_or(true) {
                        best = Some((i, len));
                    }
                    break;
                }
                j += 1;
            }
            i = j.max(i + 2);
        } else {
            i += 1;
        }
    }
    let (start, len) = best?;
    Some(data[start..start + len].to_vec())
}

/// CR3 内嵌预览（Phase 2 F03）：CR3 是 ISOBMFF 容器（非 TIFF），
/// TIFF 遍历够不着。沿 box 树走 meta→iprp→ipco，取最大的 JPEG item。
fn cr3_embedded_jpeg(src: &Path) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(src).ok()?;
    let mut head = [0u8; 8];
    f.read_exact(&mut head).ok()?;
    // CR3 特征：ftyp box 且 major brand 为 crx
    if &head[4..8] != b"ftyp" {
        return None;
    }
    let file_len = f.metadata().ok()?.len();
    f.seek(SeekFrom::Start(0)).ok()?;
    walk_isobmff(&mut f, 0, file_len, 0)
}

/// ISOBMFF box 遍历：只递归容器 box（moov/meta/iprp/ipco/iprp），
/// 大体积 box（mdat 等）直接按声明长度 seek 跳过，绝不逐字节扫
fn walk_isobmff(f: &mut std::fs::File, start: u64, end: u64, depth: u32) -> Option<Vec<u8>> {
    if depth > 8 {
        return None;
    }
    let mut off = start;
    let mut best: Option<Vec<u8>> = None;
    while off + 8 <= end {
        f.seek(SeekFrom::Start(off)).ok()?;
        let mut hdr = [0u8; 8];
        if f.read_exact(&mut hdr).is_err() {
            break;
        }
        let mut size = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]) as u64;
        let typ = &hdr[4..8];
        let mut content_off = off + 8;
        if size == 1 {
            // 64bit largesize
            let mut lg = [0u8; 8];
            if f.read_exact(&mut lg).is_err() {
                break;
            }
            size = u64::from_be_bytes(lg);
            content_off = off + 16;
        } else if size == 0 {
            size = end - off; // 延伸到文件尾
        }
        if size < 8 || off + size > end {
            break;
        }
        let next = off + size;
        match typ {
            b"moov" | b"iprp" | b"ipco" => {
                if let Some(j) = walk_isobmff(f, content_off, next, depth + 1) {
                    if best.as_ref().map(|b| j.len() > b.len()).unwrap_or(true) {
                        best = Some(j);
                    }
                }
            }
            b"meta" => {
                // FullBox：4 字节 version/flags 后才到子 box
                if let Some(j) = walk_isobmff(f, content_off + 4, next, depth + 1) {
                    if best.as_ref().map(|b| j.len() > b.len()).unwrap_or(true) {
                        best = Some(j);
                    }
                }
            }
            _ => {
                // 叶子 box：可能是 JPEG 内嵌图（CR3 的 ipco item），
                // 只读小于 64MB 的，前几字节验 SOI 再交给 cut_jpeg 宽容裁剪
                let content_len = next - content_off;
                if content_len > 1024 && content_len < 64 * 1024 * 1024 {
                    f.seek(SeekFrom::Start(content_off)).ok()?;
                    let mut peek = [0u8; 64];
                    if f.read_exact(&mut peek).is_ok() && peek[..2] == [0xFF, 0xD8] {
                        if let Some(j) = cut_jpeg_from_offset(f, content_off, content_len) {
                            if best.as_ref().map(|b| j.len() > b.len()).unwrap_or(true) {
                                best = Some(j);
                            }
                        }
                    }
                }
            }
        }
        off = next;
    }
    best
}

/// 从指定偏移读整段并抠 JPEG（复用 cut_jpeg 的宽容裁剪逻辑）
fn cut_jpeg_from_offset(f: &mut std::fs::File, offset: u64, len: u64) -> Option<Vec<u8>> {
    f.seek(SeekFrom::Start(offset)).ok()?;
    let mut buf = vec![0u8; len as usize];
    f.read_exact(&mut buf).ok()?;
    let soi = buf[..buf.len().min(64)]
        .windows(2)
        .position(|w| w[0] == 0xFF && w[1] == 0xD8)?;
    let eoi = buf.windows(2).rposition(|w| w[0] == 0xFF && w[1] == 0xD9)?;
    if eoi <= soi {
        return None;
    }
    Some(buf[soi..eoi + 2].to_vec())
}

/// 内嵌预览策略链：TIFF 遍历（含 JPEG 容器定位）→ CR3 ISOBMFF → 标记扫描
/// 标记扫描对 .jpg 文件禁用（整个文件就是 JPEG，会误抓主图）
pub fn embedded_preview(src: &Path) -> Option<Vec<u8>> {
    tiff_embedded_jpeg(src)
        .or_else(|| cr3_embedded_jpeg(src))
        .or_else(|| {
            if is_jpeg_like(src) || has_native_decoder(src) {
                None
            } else {
                marker_scan_jpeg(src)
            }
        })
}

// ---------------------------------------------------------------------------
// 解码
// ---------------------------------------------------------------------------

fn is_jpeg_like(src: &Path) -> bool {
    src.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "jpg" | "jpeg"))
        .unwrap_or(false)
}

fn is_heic_like(src: &Path) -> bool {
    src.extension()
        .and_then(|e| e.to_str())
        .map(|e| matches!(e.to_ascii_lowercase().as_str(), "heic" | "heif"))
        .unwrap_or(false)
}

/// 这些格式由 image crate 原生解码，不应再扫描最多 64MB 的 JPEG marker。
fn has_native_decoder(src: &Path) -> bool {
    src.extension()
        .and_then(|e| e.to_str())
        .map(|e| {
            matches!(
                e.to_ascii_lowercase().as_str(),
                "png" | "webp" | "gif" | "bmp" | "tga" | "tif" | "tiff"
            )
        })
        .unwrap_or(false)
}

/// 统一解码入口：目标最长边 max_px
/// 内嵌图达标（≥max_px）直接用；不达标再按格式选最快的解码路
/// **内部已取全局并发闸**，调用方不要再 `acquire()`（同线程双持会把 4 并发压成 2，FX-08）。
pub fn decode_thumb(src: &Path, max_px: u32) -> Option<DynamicImage> {
    let _permit = acquire();

    // 1. 内嵌预览（微秒~毫秒级）。即使尺寸小于目标，也保留为可靠兜底；
    //    部分 RAW 的全解码会返回纯黑，不能让黑图覆盖真实预览。
    let embedded = embedded_preview(src).and_then(|jpeg| image::load_from_memory(&jpeg).ok());
    if let Some(img) = &embedded {
        if img.dimensions().0.max(img.dimensions().1) >= max_px {
            return Some(img.thumbnail(max_px, max_px));
        }
    }

    // 2. rawler 的格式专用预览提取。它覆盖 RAF/CRW/MRW/DNG 等容器各自的
    // 预览定位规则，作为手写通用扫描失败后的轻量兜底；小尺寸占位层也允许走
    // 这条路径，但仍不会进入完整 RAW 显影。
    let raw_preview = if is_raw_like(src) {
        super::raw_decode::decode_preview(src)
    } else {
        None
    };
    if let Some(img) = &raw_preview {
        if img.dimensions().0.max(img.dimensions().1) >= max_px {
            return Some(img.thumbnail(max_px, max_px));
        }
    }

    // 3. 全解码 + 缩放（image 0.24 默认 zune-jpeg；dev 下已配 O3 override，
    //    24MP 全解码 ~370ms——实测比 jpeg-decoder 的 DCT 缩放路径还快，故精简掉后者）
    let raw_like = is_raw_like(src);
    let decoded = image::open(src)
        .ok()
        .filter(|img| !(raw_like && is_effectively_black(img)))
        .or_else(|| {
            if max_px > 320 {
                guessed_decode(src)
            } else {
                None
            }
        })
        .or_else(|| {
            // 4. 真解码兜底（Phase 2 F02/F04）：仅高清按需层；占位层（≤320px）禁用，
            //    避免 HEVC/RAW 全解码拖垮入库速度（docs/ARCHITECTURE.md 红线）
            if max_px > 320 {
                special_decode(src)
            } else {
                None
            }
        });
    if let Some(img) = decoded {
        if raw_like && is_effectively_black(&img) {
            tracing::warn!("RAW 解码结果近似纯黑，降级到内嵌预览或占位图: {src:?}");
            return raw_preview.or(embedded);
        }
        return Some(img.thumbnail(max_px, max_px));
    }
    raw_preview.or(embedded)
}

/// `image::open` 依赖扩展名；3FR/IIQ 等 TIFF 容器按文件头识别后解码其预览。
fn guessed_decode(src: &Path) -> Option<DynamicImage> {
    let reader = image::ImageReader::open(src)
        .ok()?
        .with_guessed_format()
        .ok()?;
    if reader.format() != Some(image::ImageFormat::Tiff) {
        return None;
    }
    let mut reader = reader;
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(20_000);
    limits.max_image_height = Some(20_000);
    limits.max_alloc = Some(256 * 1024 * 1024);
    reader.limits(limits);
    reader.decode().ok()
}

/// 特殊格式真解码分派：HEIC/HEIF → libheif；RAW 系 → rawler
fn special_decode(src: &Path) -> Option<DynamicImage> {
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default();
    let img = match ext.as_str() {
        "heic" | "heif" => super::heic_decode::decode_heic(src),
        _ => super::raw_decode::decode_raw(src),
    }?;
    if is_raw_like(src) && is_effectively_black(&img) {
        tracing::warn!("RAW 全解码结果近似纯黑，降级到内嵌预览或占位图: {src:?}");
        return None;
    }
    Some(img)
}

fn is_raw_like(src: &Path) -> bool {
    src.extension()
        .and_then(|e| e.to_str())
        .map(crate::utils::mime::is_raw_ext)
        .unwrap_or(false)
}

/// RAW 全解码偶发返回黑帧（上游黑电平/裁剪处理缺陷）。只拦截接近纯黑且无动态范围的
/// 结果，避免让可靠的内嵌预览或通用占位图被黑卡覆盖。
fn is_effectively_black(img: &DynamicImage) -> bool {
    let sample = img.thumbnail(64, 64).to_luma8();
    let mut min = u8::MAX;
    let mut max = u8::MIN;
    let mut sum = 0u64;
    for px in sample.pixels() {
        let v = px.0[0];
        min = min.min(v);
        max = max.max(v);
        sum += u64::from(v);
    }
    let mean = sum as f64 / sample.pixels().len().max(1) as f64;
    max <= 8 && max.saturating_sub(min) <= 4 && mean <= 4.0
}

/// 解码并写 webp 缩略图到 out
pub fn write_thumb(src: &Path, out: &Path, max_px: u32) -> bool {
    match decode_thumb(src, max_px) {
        Some(img) => img.save_with_format(out, image::ImageFormat::WebP).is_ok(),
        None => false,
    }
}

/// W5d（§W5d）：解码并写缩略图，顺带返回 dHash（入库搭车 —— decode_thumb 内部
/// 必然产出已解码像素，从这里算 phash 是零额外解码；None = 未解码出图像/视频）。
pub fn write_thumb_with_phash(src: &Path, out: &Path, max_px: u32) -> (bool, Option<u64>) {
    match decode_thumb(src, max_px) {
        Some(img) => {
            let phash = super::perceptual::dhash(&img);
            let ok = img.save_with_format(out, image::ImageFormat::WebP).is_ok();
            (ok, Some(phash))
        }
        None => (false, None),
    }
}

/// W5d（§W5d）：解码出已下采样图像 + dHash（回填命令用；只调一次 decode_thumb，
/// 绝不额外解码原文件 —— decode_thumb 的返回就是已解码像素）。
pub fn decode_thumb_phash(src: &Path, max_px: u32) -> Option<(image::DynamicImage, Option<u64>)> {
    let img = decode_thumb(src, max_px)?;
    let phash = super::perceptual::dhash(&img);
    Some((img, Some(phash)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ai_image_is_normalized_to_jpeg_and_max_edge() {
        let dir = std::env::temp_dir().join(format!("bagertea_ai_image_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let src = dir.join("large.png");
        let image = image::RgbaImage::from_fn(2400, 1200, |x, y| {
            if x < 1200 {
                image::Rgba([220, 40, 30, 255])
            } else {
                image::Rgba([40, 80, 220, (y % 255) as u8])
            }
        });
        image.save(&src).unwrap();

        let payload = encode_ai_image(&src, AI_IMAGE_MAX_PX).expect("PNG 应可被 AI 解码");
        assert_eq!(payload.max_px, AI_IMAGE_MAX_PX);
        assert_eq!(payload.width, 1024);
        assert_eq!(payload.height, 512);
        assert_eq!(&payload.bytes[..2], &[0xff, 0xd8], "输出必须是 JPEG");
        let decoded = image::load_from_memory(&payload.bytes).unwrap();
        assert_eq!(decoded.dimensions(), (1024, 512));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn semaphore_allows_up_to_four() {
        let p1 = acquire();
        let p2 = acquire();
        let p3 = acquire();
        let p4 = acquire();
        // 第 5 个会阻塞——另起线程验证释放后能拿到
        let h = std::thread::spawn(|| {
            acquire();
        });
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert!(!h.is_finished());
        drop(p1);
        h.join().unwrap();
        drop(p2);
        drop(p3);
        drop(p4);
    }

    #[test]
    fn marker_scan_picks_largest_jpeg() {
        let dir = std::env::temp_dir().join(format!("bagertea_marker_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("fake.raw");
        let mut data = vec![0u8; 100];
        // 小 JPEG
        data.extend_from_slice(&[0xFF, 0xD8, 1, 2, 3, 0xFF, 0xD9]);
        data.extend_from_slice(&[0u8; 50]);
        // 大 JPEG
        data.extend_from_slice(&[0xFF, 0xD8]);
        data.extend_from_slice(&vec![7u8; 5000]);
        data.extend_from_slice(&[0xFF, 0xD9]);
        std::fs::write(&f, &data).unwrap();
        let j = marker_scan_jpeg(&f).unwrap();
        assert_eq!(j.len(), 5004);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn marker_scan_finds_jpeg_spanning_blocks() {
        let dir = std::env::temp_dir().join(format!("bagertea_marker_blk_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("huge.raw");
        // JPEG 内容超过一个 block（MARKER_SCAN_BLOCK），验证分块读取仍能完整提取跨块 JPEG
        let mut data = vec![0u8; 100];
        data.extend_from_slice(&[0xFF, 0xD8]);
        data.extend_from_slice(&vec![7u8; 300 * 1024]);
        data.extend_from_slice(&[0xFF, 0xD9]);
        std::fs::write(&f, &data).unwrap();
        let j = marker_scan_jpeg(&f).unwrap();
        assert!(
            j.len() > 300 * 1024,
            "应提取到跨块完整 JPEG，实际 {}",
            j.len()
        );
        assert_eq!(&j[..2], &[0xFF, 0xD8]);
        assert_eq!(&j[j.len() - 2..], &[0xFF, 0xD9]);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn cut_jpeg_validates_markers() {
        let dir = std::env::temp_dir().join(format!("bagertea_cut_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("x.bin");
        std::fs::write(&f, [0xFF, 0xD8, 9, 9, 0xFF, 0xD9]).unwrap();
        assert!(cut_jpeg(&f, 0, 6).is_some());
        assert!(cut_jpeg(&f, 1, 4).is_none()); // 缺 SOI
        assert!(cut_jpeg(&f, 0, 0).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 合成 CR3：ftyp + meta(FullBox)→iprp→ipco→JPEG 叶子 box + 大 mdat（验证 seek 跳过）
    fn be32(v: u32) -> [u8; 4] {
        v.to_be_bytes()
    }

    #[test]
    fn cr3_walk_finds_embedded_jpeg() {
        let dir = std::env::temp_dir().join(format!("bagertea_cr3_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("fake.cr3");

        // 假 JPEG（>1KB 才进候选）
        let mut jpeg = vec![0xFFu8, 0xD8];
        jpeg.extend(vec![7u8; 2000]);
        jpeg.extend_from_slice(&[0xFF, 0xD9]);

        // ipco 叶子 box（非 JPEG item）+ JPEG item box
        let mut ipco_content = Vec::new();
        ipco_content.extend_from_slice(&be32(16));
        ipco_content.extend_from_slice(b"ispe");
        ipco_content.extend_from_slice(&[0u8; 8]);
        ipco_content.extend_from_slice(&be32((jpeg.len() + 8) as u32));
        ipco_content.extend_from_slice(b"avc1");
        ipco_content.extend_from_slice(&jpeg);
        let ipco_box: Vec<u8> = [
            &be32((ipco_content.len() + 8) as u32)[..],
            b"ipco",
            &ipco_content,
        ]
        .concat();
        let iprp_box: Vec<u8> =
            [&be32((ipco_box.len() + 8) as u32)[..], b"iprp", &ipco_box].concat();
        // meta = FullBox：4 字节 version/flags
        let meta_content: Vec<u8> = [&[0u8; 4][..], &iprp_box].concat();
        let meta_box: Vec<u8> = [
            &be32((meta_content.len() + 8) as u32)[..],
            b"meta",
            &meta_content,
        ]
        .concat();
        // 假 mdat（RAW 数据所在，必须被 seek 跳过而非读入）
        let mdat_box: Vec<u8> = [&be32(16)[..], b"mdat", &[0xAB; 8]].concat();
        // ftyp：size 声明必须与实际字节数一致（4+4+4=12），否则遍历错位
        let ftyp_box: Vec<u8> = [&be32(12)[..], b"ftyp", b"crx "].concat();

        let file: Vec<u8> = [&ftyp_box[..], &meta_box, &mdat_box].concat();
        std::fs::write(&f, &file).unwrap();

        let got = cr3_embedded_jpeg(&f).unwrap();
        assert_eq!(got.len(), jpeg.len());
        assert_eq!(&got[..2], &[0xFF, 0xD8]);
        // 非 ftyp 开头（普通 JPEG 文件）不该进 CR3 分支
        let f2 = dir.join("plain.jpg");
        std::fs::write(&f2, &jpeg).unwrap();
        assert!(cr3_embedded_jpeg(&f2).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn native_formats_skip_marker_scan() {
        let dir = std::env::temp_dir().join(format!("bagertea_native_skip_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let png = dir.join("fake.png");
        let raw = dir.join("fake.raw");
        let mut data = vec![0x89, b'P', b'N', b'G'];
        data.extend_from_slice(&[0u8; 100]);
        data.extend_from_slice(&[0xFF, 0xD8]);
        data.extend_from_slice(&vec![7u8; 5000]);
        data.extend_from_slice(&[0xFF, 0xD9]);
        std::fs::write(&png, &data).unwrap();
        std::fs::write(&raw, &data).unwrap();

        assert!(
            embedded_preview(&png).is_none(),
            "PNG 由原生解码器处理，不应扫描伪 JPEG"
        );
        assert!(
            embedded_preview(&raw).is_some(),
            "未知/RAW 容器仍应保留 marker 兜底"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn guessed_tiff_preview_supports_3fr() {
        let sample = std::path::Path::new(
            r"F:\testdata\S2_formats\raw\Hasselblad - CFV-50c - 16bit (4_3).3FR",
        );
        if !sample.exists() {
            eprintln!("跳过：3FR 真机样本不存在（{}）", sample.display());
            return;
        }
        assert!(
            probe_dimensions(sample).is_some(),
            "3FR 应能从 TIFF 内容识别宽高"
        );
        assert!(
            decode_thumb(sample, 512).is_some(),
            "3FR 应能从 TIFF 内容解出预览"
        );
    }

    #[test]
    fn effectively_black_detects_empty_raw_output() {
        let black = DynamicImage::new_luma8(64, 64);
        assert!(is_effectively_black(&black));

        let mut visible = DynamicImage::new_luma8(64, 64).to_luma8();
        visible.put_pixel(32, 32, image::Luma([80]));
        assert!(!is_effectively_black(&DynamicImage::ImageLuma8(visible)));
    }

    #[test]
    fn black_raw_decode_falls_back_to_real_embedded_preview() {
        let sample = std::path::Path::new(r"F:\all\test4\test4_20260915_333.ARW");
        if !sample.exists() {
            eprintln!("跳过：ARW 真机样本不存在（{}）", sample.display());
            return;
        }
        let img = decode_thumb(sample, 1024).expect("应回退到可用的内嵌预览");
        let rgb = img.to_rgb8();
        let max = rgb.pixels().flat_map(|p| p.0).max().unwrap_or(0);
        assert!(max > 16, "回退结果不应是纯黑占位");
    }

    #[test]
    fn known_black_raw_samples_never_emit_black_thumbnails() {
        let samples = [
            r"F:\all\test4\test4_20260915_305.DCR",
            r"F:\all\test4\test4_20260915_312.NRW",
            r"F:\all\test4\test4_20260915_317.ORF",
        ];
        for sample in samples {
            let path = std::path::Path::new(sample);
            if !path.exists() {
                eprintln!("跳过：RAW 真机样本不存在（{}）", path.display());
                continue;
            }
            if let Some(img) = decode_thumb(path, 1024) {
                assert!(
                    !is_effectively_black(&img),
                    "已知黑帧样本不应输出全黑缩略图: {}",
                    path.display()
                );
            }
        }
    }
}
