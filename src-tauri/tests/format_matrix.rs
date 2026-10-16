//! 格式 × 层级矩阵测试（Phase 2 F06）
//!
//! 合成样本覆盖可编码格式（jpg/png/webp/bmp/tga/tif）在占位层（320）与
//! 高清层（1024）的出图断言；垃圾/损坏样本验证各层优雅降级不 panic。
//! 真实 RAW/HEIC 样本无法合成，走 perf_probe 手动探针 + 老板素材走查
//! （fixtures 大文件 gitignore，见 docs/TEST_STRATEGY.md 与 docs/PERFORMANCE.md）。

use std::path::PathBuf;

use image::{GenericImageView, ImageBuffer, Rgb};

use teacell_ai_media_manager_lib::services::imaging;
use teacell_ai_media_manager_lib::utils::mime;

/// 每个用例独立目录（测试默认并行跑，共享目录会被互相 remove_dir_all 踩坏）
fn fixture_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("bagertea_fmt_{}_{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 640×480 渐变测试图（有细节，能暴露解码花屏问题）
fn sample_image() -> image::DynamicImage {
    let img = ImageBuffer::from_fn(640, 480, |x, y| {
        Rgb([((x * 255) / 639) as u8, ((y * 255) / 479) as u8, 128u8])
    });
    image::DynamicImage::ImageRgb8(img)
}

/// 可编码格式清单（image crate 有编码器的全部格式，与 F01 白名单对齐）
const ENCODABLE: &[(&str, &str)] = &[
    ("jpg", "image/jpeg"),
    ("png", "image/png"),
    ("webp", "image/webp"),
    ("bmp", "image/bmp"),
    ("tga", "image/tga"),
    ("tif", "image/tiff"),
];

#[test]
fn whitelist_covers_all_formats() {
    // 可编码格式 + HEIC 全部入库放行
    for (ext, expected_mime) in ENCODABLE
        .iter()
        .chain(&[("heic", "image/heic"), ("heif", "image/heif")])
    {
        assert_eq!(
            mime::asset_type_from_ext(ext),
            Some("image"),
            "扩展名 {ext} 应放行"
        );
        assert_eq!(mime::mime_from_ext(ext).as_deref(), Some(*expected_mime));
    }
    // RAW 系判定与白名单同源：25 个扩展名全中，常见格式不误伤
    for ext in [
        "raw", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "dng", "raf", "orf", "rw2",
        "pef", "srw", "x3f", "mrw", "iiq", "3fr", "fff", "kdc", "dcr", "mos", "mef", "erf",
    ] {
        assert!(mime::is_raw_ext(ext), "{ext} 应判为 RAW");
        assert_eq!(mime::asset_type_from_ext(ext), Some("image"));
    }
    assert!(!mime::is_raw_ext("jpg"));
    assert!(!mime::is_raw_ext("tiff"));
    // 未知扩展名不放行（防黑图入库）
    assert_eq!(mime::asset_type_from_ext("xyz"), None);
    assert_eq!(mime::asset_type_from_ext("avif"), None);
}

#[test]
fn decodable_formats_placeholder_and_hd() {
    let dir = fixture_dir("decode");
    let img = sample_image();
    for (ext, _) in ENCODABLE {
        let p = dir.join(format!("sample.{ext}"));
        img.save(&p)
            .unwrap_or_else(|e| panic!("{ext} 编码失败: {e}"));

        // 占位层：秒开路径，出图且不超过目标边长
        let ph =
            imaging::decode_thumb(&p, 320).unwrap_or_else(|| panic!("{ext} 占位层（320）应出图"));
        assert!(
            ph.width() <= 320 && ph.height() <= 320,
            "{ext} 占位层尺寸越界"
        );

        // 高清层：同样出图
        let hd =
            imaging::decode_thumb(&p, 1024).unwrap_or_else(|| panic!("{ext} 高清层（1024）应出图"));
        assert!(
            hd.width() <= 1024 && hd.height() <= 1024,
            "{ext} 高清层尺寸越界"
        );
        // 原图小于目标边时 thumbnail 会放大到目标边，只验宽高比不变
        let ratio_ok = (hd.width() as f32 / hd.height() as f32 - 640.0 / 480.0).abs() < 0.02;
        assert!(ratio_ok, "{ext} 高清层宽高比失真: {:?}", hd.dimensions());
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn placeholder_layer_rejects_garbage_raw() {
    // 占位层红线：RAW 垃圾数据在 320px 路径必须快速返回 None，
    // 绝不触发真解码（special_decode 仅 max_px>320 启用）
    let dir = fixture_dir("ph_garbage");
    let p = dir.join("garbage.nef");
    std::fs::write(&p, b"this is not a raw file").unwrap();
    assert!(imaging::decode_thumb(&p, 320).is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn hd_layer_garbage_degrades_gracefully() {
    // 高清层对坏文件：走完真解码兜底后优雅返回 None，不 panic
    let dir = fixture_dir("hd_garbage");
    for (name, bytes) in [
        ("garbage.nef", &b"fake raw bytes"[..]),
        ("garbage.heic", &b"fake heic bytes"[..]),
        (
            "garbage.cr3",
            &[
                0, 0, 0, 12, b'f', b't', b'y', b'p', b'c', b'r', b'x', b' ', 1, 2, 3,
            ][..],
        ),
    ] {
        let p = dir.join(name);
        std::fs::write(&p, bytes).unwrap();
        assert!(
            imaging::decode_thumb(&p, 1024).is_none(),
            "{name} 高清层应降级为 None 而非 panic"
        );
    }
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn heic_and_raw_decoder_guards() {
    use teacell_ai_media_manager_lib::services::{heic_decode, raw_decode};
    let dir = fixture_dir("guards");
    let garbage = dir.join("g.heic");
    std::fs::write(&garbage, vec![0u8; 4096]).unwrap();
    assert!(heic_decode::decode_heic(&garbage).is_none());
    assert!(raw_decode::decode_raw(&garbage).is_none());
    assert!(heic_decode::decode_heic(std::path::Path::new("不存在.heic")).is_none());
    std::fs::remove_dir_all(&dir).ok();
}

#[test]
fn truncated_jpeg_graceful() {
    // 截断 JPEG：任何一层都不得 panic（出不出图交给解码器判定）
    let dir = fixture_dir("truncated");
    let full = dir.join("full.jpg");
    sample_image().save(&full).unwrap();
    let bytes = std::fs::read(&full).unwrap();
    let cut = dir.join("cut.jpg");
    std::fs::write(&cut, &bytes[..bytes.len() / 3]).unwrap();
    let _ = imaging::decode_thumb(&cut, 320);
    let _ = imaging::decode_thumb(&cut, 1024);
    std::fs::remove_dir_all(&dir).ok();
}

/// W1-4：RAW 宽高探测 —— image crate 不支持 RW2（image_dimensions 必败），
/// probe_dimensions 走 rawler 应能拿到宽高。用真机样本（f:/all 的松下 S5II RAW）；
/// 样本不存在时跳过（CI/其它机器无样本），避免误报。
#[test]
fn raw_probe_dimensions_reads_rw2() {
    use teacell_ai_media_manager_lib::services::raw_decode;
    let sample = std::path::Path::new("f:/all/_1091396.RW2");
    if !sample.exists() {
        eprintln!("跳过：真机 RW2 样本不存在（{sample:?}）");
        return;
    }
    // 前置确认：image crate 确实不支持 RW2（这是 W1-4 存在的前提）
    assert!(
        image::image_dimensions(sample).is_err(),
        "前提失效：image crate 已支持 RW2？请复核 probe_dimensions 是否还需要"
    );
    let dims = raw_decode::probe_dimensions(sample).expect("rawler 应能读出 RW2 宽高");
    // S5II 最大 6000x4000（45MP 全画幅），宽高都应为正且在合理范围
    assert!(dims.0 > 1000 && dims.0 <= 12000, "宽度异常: {dims:?}");
    assert!(dims.1 > 1000 && dims.1 <= 12000, "高度异常: {dims:?}");
}

/// W1-4：probe_dimensions 对垃圾文件/不存在文件返回 None（不 panic）。
#[test]
fn raw_probe_dimensions_guards() {
    use teacell_ai_media_manager_lib::services::raw_decode;
    let dir = fixture_dir("probe-dims");
    let garbage = dir.join("g.rw2");
    std::fs::write(&garbage, vec![0u8; 4096]).unwrap();
    assert!(raw_decode::probe_dimensions(&garbage).is_none());
    assert!(raw_decode::probe_dimensions(std::path::Path::new("不存在.rw2")).is_none());
    std::fs::remove_dir_all(&dir).ok();
}
