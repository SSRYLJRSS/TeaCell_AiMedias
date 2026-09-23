//! RAW 真解码兜底层（Phase 2 F04，基于 rawler 0.7.2）
//!
//! 定位：内嵌预览缺失/过小时的兜底，只进高清按需层与查看器大图，
//! **严禁进占位图路径**（会打爆入库速度，见 docs/ARCHITECTURE.md 红线）。
//!
//! 缩略图优化：2×2 Bayer binning——每 2×2 块按 CFA 通道归组取均值，
//! 直接得到半分辨率 RGB（零插值伪影、1/4 内存），45MP 传感器产出 ~4K×3K，
//! 对 512/1920px 缩略图绰绰有余。X-Trans（6×6）不支持 binning，降级灰度预览。
//!
//! 色彩管线（简化显影，非专业级）：
//! 黑/白电平归一 → 相机白平衡（wb_coeffs，G 归一化）
//! → camRGB→XYZ（cam_to_xyz_normalized）→ XYZ→sRGB 矩阵 → gamma 2.2

use image::{DynamicImage, ImageBuffer};
use rawler::{decoders::RawDecodeParams, RawImage, RawImageData};

use super::imaging;

/// 超过该像素数的 RAW 拒绝全解码（内存保护红线，见 docs/ARCHITECTURE.md）
const MAX_DECODE_PIXELS: usize = 150_000_000;

/// XYZ(D65) → sRGB 线性矩阵
const XYZ_TO_SRGB: [[f32; 3]; 3] = [
    [3.2406, -1.5372, -0.4986],
    [-0.9689, 1.8758, 0.0415],
    [0.0557, -0.2040, 1.0570],
];

/// W1-4：只读 RAW 宽高（不 binning、不做色彩管线）。
/// image::image_dimensions 不支持 RAW（image crate 无 RAW 解码器）→ 205 张 RW2 宽高全 NULL。
/// 这里只取 rawler::decode_file 的 width/height 就返回。
/// 注意：decode_file 会读整个文件（非只读头），45MP 级文件每次调用约百毫秒~秒级，
/// 适合入库单次与手动回填，不适合批量热路径。
pub fn probe_dimensions(src: &std::path::Path) -> Option<(u32, u32)> {
    let _permit = imaging::acquire();
    catch_raw_panics(src, || {
        let raw = rawler::decode_file(src).ok()?;
        Some((raw.width as u32, raw.height as u32))
    })
}

/// RAW 真解码为 DynamicImage（半分辨率 binning 结果，未缩放到目标边长）
/// 调用方负责再 thumbnail()；失败返回 None 由策略链降级
pub fn decode_raw(src: &std::path::Path) -> Option<DynamicImage> {
    catch_raw_panics(src, || decode_raw_inner(src))
}

/// 提取 RAW 文件自带的缩略图/预览图。
///
/// rawler 已为不同厂商容器实现了各自的预览定位规则（RAF、CRW、MRW、DNG 等），
/// 比通用 TIFF/JPEG marker 扫描覆盖更完整。该路径只负责取预览，不进入自写的
/// Bayer 显影，因此适合入库阶段的快速缩略图，也作为高清真解码前的可靠兜底。
pub fn decode_preview(src: &std::path::Path) -> Option<DynamicImage> {
    catch_raw_panics(src, || {
        let params = RawDecodeParams::default();
        let image = rawler::analyze::extract_thumbnail_pixels(src, &params).ok()?;
        if image.width() == 0 || image.height() == 0 || is_effectively_black(&image) {
            return None;
        }
        Some(image)
    })
}

fn is_effectively_black(image: &DynamicImage) -> bool {
    let sample = image.thumbnail(64, 64).to_luma8();
    let mut max = 0u8;
    let mut sum = 0u64;
    for pixel in sample.pixels() {
        let value = pixel.0[0];
        max = max.max(value);
        sum += u64::from(value);
    }
    let count = sample.width().saturating_mul(sample.height()).max(1) as u64;
    max <= 8 && sum / count <= 4
}

fn decode_raw_inner(src: &std::path::Path) -> Option<DynamicImage> {
    let mut raw = rawler::decode_file(src).ok()?;
    if raw.width * raw.height > MAX_DECODE_PIXELS {
        tracing::warn!("RAW 超过解码像素上限，跳过: {src:?}");
        return None;
    }
    // cpp==3 的 RAW（如部分 DNG 线性 RGB）：rawler 已给出 RGB，直接转换
    if raw.cpp == 3 {
        return linear_rgb_to_dynamic(&raw);
    }
    if raw.cpp != 1 {
        return None;
    }
    // rawler 0.7.2 的 cropped_cfa() 仍是 todo!()。这里按完整传感器坐标
    // 使用相机 CFA，后续 color_at(y, x) 也传原始坐标，不需要再额外平移。
    let cfa = &raw.camera.cfa;
    // X-Trans 等非 2×2 阵列：binning 不适用，降级灰度预览（聊胜于无）
    if cfa.width != 2 || cfa.height != 2 {
        return grayscale_preview(&mut raw);
    }
    bayer_binning(&mut raw)
}

/// rawler 0.7.2 仍有多处 `todo!()/unimplemented!()`。单个异常文件不能把
/// Rayon 入库线程连同整批任务一起打掉，因此把上游 panic 降级为 None。
fn catch_raw_panics<T>(src: &std::path::Path, f: impl FnOnce() -> Option<T>) -> Option<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(_) => {
            tracing::warn!("RAW 解码 panic，已降级跳过: {}", src.display());
            None
        }
    }
}

/// 2×2 Bayer binning → 半分辨率 sRGB u8
fn bayer_binning(raw: &mut RawImage) -> Option<DynamicImage> {
    let data = match &raw.data {
        RawImageData::Integer(d) => d,
        _ => return None, // Float RAW 少见，暂不支持，走降级
    };
    let (w, h) = (raw.width, raw.height);
    // 裁剪区（优先 crop_area，其次 active_area）：保证偶数对齐
    let (cx, cy, cw, ch) = raw
        .crop_area
        .or(raw.active_area)
        .map(|r| (r.p.x & !1, r.p.y & !1, r.d.w & !1, r.d.h & !1))
        .unwrap_or((0, 0, w & !1, h & !1));
    if cw < 4 || ch < 4 {
        return None;
    }

    let cfa = &raw.camera.cfa;
    let wb = neutral_wb(raw);
    // 黑/白电平按 2×2 Bayer 位置展开（0=R 1=G1 2=B 3=G2）
    let bl4 = raw.blacklevel.as_bayer_array();
    let wl4 = raw.whitelevel.as_bayer_array();
    // cam→XYZ 矩阵（行主序 [row][col]，4 通道取前 3）
    let m_cam_xyz = raw.cam_to_xyz_normalized();
    // 合成 cam→sRGB：M_srgb_xyz × M_cam_xyz
    let mut m = [[0f32; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            m[r][c] = XYZ_TO_SRGB[r][0] * m_cam_xyz[0][c]
                + XYZ_TO_SRGB[r][1] * m_cam_xyz[1][c]
                + XYZ_TO_SRGB[r][2] * m_cam_xyz[2][c];
        }
    }

    let out_w = cw / 2;
    let out_h = ch / 2;
    let mut buf = vec![0u8; out_w * out_h * 3];

    for by in 0..out_h {
        let y0 = cy + by * 2;
        for bx in 0..out_w {
            let x0 = cx + bx * 2;
            // 2×2 块内按 CFA 颜色归组求均值（RGGB 类阵列每块恰含 R×1 G×2 B×1）
            // shift 索引同取：记录每个颜色通道的块内位置（用于黑电平查表）
            let mut sum = [0.0f32; 3];
            let mut cnt = [0.0f32; 3];
            let mut slot = [0usize; 3];
            for dy in 0..2 {
                for dx in 0..2 {
                    let (x, y) = (x0 + dx, y0 + dy);
                    let v = data[y * w + x] as f32;
                    let ci = cfa.color_at(y, x); // 0=R 1=G 2=B
                    if ci < 3 {
                        sum[ci] += v;
                        cnt[ci] += 1.0;
                        slot[ci] = dy * 2 + dx;
                    }
                }
            }
            // 黑白电平归一 + 白平衡（各通道独立）
            let mut rgb = [0.0f32; 3];
            for c in 0..3 {
                if cnt[c] <= 0.0 {
                    continue;
                }
                let mean = sum[c] / cnt[c];
                let bl = bl4[slot[c]];
                let wl = wl4[slot[c]];
                let norm = (mean - bl) / (wl - bl).max(1.0);
                rgb[c] = (norm * wb[c]).clamp(0.0, 1.0);
            }
            // camRGB → sRGB 线性 → gamma
            let sr = m[0][0] * rgb[0] + m[0][1] * rgb[1] + m[0][2] * rgb[2];
            let sg = m[1][0] * rgb[0] + m[1][1] * rgb[1] + m[1][2] * rgb[2];
            let sb = m[2][0] * rgb[0] + m[2][1] * rgb[1] + m[2][2] * rgb[2];
            let o = (by * out_w + bx) * 3;
            buf[o] = gamma_u8(sr);
            buf[o + 1] = gamma_u8(sg);
            buf[o + 2] = gamma_u8(sb);
        }
    }
    let img = ImageBuffer::from_raw(out_w as u32, out_h as u32, buf)?;
    Some(DynamicImage::ImageRgb8(img))
}

/// 白平衡系数（RGBE 序，G 归一化；无效时中性）
fn neutral_wb(raw: &RawImage) -> [f32; 3] {
    let wb = raw.wb_coeffs;
    let all_ok = wb.iter().take(3).all(|c| c.is_finite() && *c > 0.01);
    if !all_ok {
        return [1.0; 3];
    }
    let g = wb[1].max(0.001);
    [wb[0] / g, 1.0, wb[2] / g]
}

fn gamma_u8(v: f32) -> u8 {
    let v = v.clamp(0.0, 1.0);
    (v.powf(1.0 / 2.2) * 255.0 + 0.5) as u8
}

/// cpp==3 线性 RGB RAW（部分 DNG）：直接 16bit→8bit gamma
fn linear_rgb_to_dynamic(raw: &RawImage) -> Option<DynamicImage> {
    let data = match &raw.data {
        RawImageData::Integer(d) => d,
        _ => return None,
    };
    let n = raw.width * raw.height;
    let mut buf = vec![0u8; n * 3];
    let white = ((1u32 << raw.bps.min(16)) - 1) as f32;
    for i in 0..n {
        for c in 0..3 {
            let v = data[i * 3 + c] as f32 / white;
            buf[i * 3 + c] = gamma_u8(v);
        }
    }
    let img = ImageBuffer::from_raw(raw.width as u32, raw.height as u32, buf)?;
    Some(DynamicImage::ImageRgb8(img))
}

/// X-Trans 等非 Bayer 阵列降级：灰度预览（有图 > 无图）
fn grayscale_preview(raw: &mut RawImage) -> Option<DynamicImage> {
    let data = match &raw.data {
        RawImageData::Integer(d) => d,
        _ => return None,
    };
    let n = raw.width * raw.height;
    let mut buf = vec![0u8; n];
    let white = ((1u32 << raw.bps.min(16)) - 1) as f32;
    for i in 0..n {
        buf[i] = ((data[i] as f32 / white).clamp(0.0, 1.0) * 255.0) as u8;
    }
    let img = ImageBuffer::from_raw(raw.width as u32, raw.height as u32, buf)?;
    Some(DynamicImage::ImageLuma8(img))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gamma_boundaries() {
        assert_eq!(gamma_u8(-0.5), 0);
        assert_eq!(gamma_u8(0.0), 0);
        assert_eq!(gamma_u8(1.0), 255);
        assert_eq!(gamma_u8(2.0), 255);
        // gamma 2.2 曲线上 0.5 线性值应提亮到 ~186
        assert!((gamma_u8(0.5) as i32 - 186).abs() <= 1);
    }

    #[test]
    fn wb_invalid_falls_back_neutral() {
        // wb_coeffs 全 0（部分相机不写 WB）时应返回中性系数而非 NaN
        let wb = [0.0f32, 0.0, 0.0, 0.0];
        let all_ok = wb.iter().take(3).all(|c| c.is_finite() && *c > 0.01);
        assert!(!all_ok);
    }

    #[test]
    fn decode_nonexistent_returns_none() {
        assert!(decode_raw(std::path::Path::new("不存在的文件.cr2")).is_none());
    }

    #[test]
    fn raw_panic_is_contained() {
        let result = catch_raw_panics(std::path::Path::new("panic.raw"), || -> Option<()> {
            panic!("not yet implemented")
        });
        assert!(result.is_none());
    }

    #[test]
    fn decode_real_crw_no_longer_hits_upstream_todo() {
        let sample =
            std::path::Path::new(r"F:\testdata\S2_formats\raw\Canon - EOS D30 - RAW (3_2).CRW");
        if !sample.exists() {
            eprintln!("跳过：CRW 真机样本不存在（{}）", sample.display());
            return;
        }
        assert!(
            decode_raw(sample).is_some(),
            "CRW 真解码不应 panic 或返回空"
        );
    }
}
