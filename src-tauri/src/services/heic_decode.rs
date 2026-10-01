//! HEIC/HEIF 解码兜底层（Phase 2 F02，基于 heif-rs → 预编译静态 libheif+libde265）
//!
//! 定位：image crate 无 HEIF 解码器，iPhone/安卓 HEIC 在此真解码。
//! 与 raw_decode 同红线：只进高清按需层，占位层（≤320px）优先走内嵌预览，
//! HEVC 全解码比内嵌 JPEG 提取慢一到两个量级，绝不能进入库占位路径。
//!
//! 依赖链：heif-rs（Apache-2.0）静态链接 libheif（LGPL-3.0）+ libde265（LGPL-3.0）；
//! GPL 组合分发与对应源码要求见 docs/ARCHITECTURE.md。

use image::{DynamicImage, ImageDecoder};

/// 超过该文件大小的 HEIC 拒绝解码（内存保护：heif::decode 需整文件读入）
const MAX_HEIC_BYTES: u64 = 128 * 1024 * 1024;

/// HEIC/HEIF 真解码为 DynamicImage；失败返回 None 由策略链降级
pub fn decode_heic(src: &std::path::Path) -> Option<DynamicImage> {
    let meta = std::fs::metadata(src).ok()?;
    if meta.len() > MAX_HEIC_BYTES {
        tracing::warn!("HEIC 超过解码大小上限，跳过: {src:?}");
        return None;
    }
    let bytes = std::fs::read(src).ok()?;
    match heif::decode(&bytes) {
        Ok(img) => Some(img),
        Err(e) => {
            tracing::debug!("HEIC 解码失败 {src:?}: {e}");
            None
        }
    }
}

/// 只解析 HEIF 容器头和主图属性，不解 HEVC 像素。
pub fn probe_dimensions(src: &std::path::Path) -> Option<(u32, u32)> {
    let meta = std::fs::metadata(src).ok()?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_HEIC_BYTES {
        return None;
    }
    let file = std::fs::File::open(src).ok()?;
    let decoder = heif::HeifDecoder::new(file).ok()?;
    Some(decoder.dimensions())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_nonexistent_returns_none() {
        assert!(decode_heic(std::path::Path::new("不存在的文件.heic")).is_none());
    }

    #[test]
    fn decode_garbage_returns_none() {
        let dir = std::env::temp_dir().join(format!("bagertea_heic_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("fake.heic");
        std::fs::write(&f, b"this is not a heic file at all").unwrap();
        assert!(decode_heic(&f).is_none());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn probe_real_heic_reads_dimensions() {
        let sample = std::path::Path::new(r"F:\testdata\S2_formats\common\heic");
        let Ok(entries) = std::fs::read_dir(sample) else {
            eprintln!("跳过：HEIC 真机样本目录不存在（{}）", sample.display());
            return;
        };
        let file = entries.filter_map(Result::ok).map(|e| e.path()).find(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .map(|e| matches!(e.to_ascii_lowercase().as_str(), "heic" | "heif"))
                .unwrap_or(false)
        });
        let Some(file) = file else {
            eprintln!("跳过：HEIC 真机样本为空");
            return;
        };
        let dims = probe_dimensions(&file).expect("HEIC 容器头应能读出宽高");
        assert!(dims.0 > 0 && dims.1 > 0);
    }
}
