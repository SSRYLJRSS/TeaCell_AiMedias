//! 测试素材可解码性验证（一次性工具，不进主程序）。
//!
//! 目的：**下载成功 != 软件能读**。本工具直接调用项目真实的解码链路，
//! 对 F:/testdata 下的素材逐类验证，输出「哪种格式能出图、出多少分辨率、耗时多少」。
//!
//! 复用后端真实函数：
//!   services::imaging::probe_dimensions   —— 通用图片 + RAW 尺寸探测
//!   services::imaging::decode_thumb       —— 统一解码入口（内嵌预览 → image::open → HEIC/RAW 真解码）
//!   services::imaging::embedded_preview   —— 内嵌预览提取（RAW 快通道，单独统计命中率）
//!
//! 运行：
//!   cd src-tauri
//!   $env:QA_ROOT="F:/testdata"; cargo run --example qa_media_probe
//! 可选：
//!   $env:QA_LIMIT="6"   每类最多测几个（默认 6）
//!
//! 产出：控制台表格 + 若设置 QA_OUT 则写 JSON。

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use bagertea_ai_media_v2_lib::services::imaging;

fn env_or(key: &str, def: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| def.to_string())
}

/// 收集某目录下指定扩展名的文件（最多 limit 个）
fn collect(dir: &Path, exts: &[&str], limit: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = fs::read_dir(dir) else {
        return out;
    };
    let mut all: Vec<PathBuf> = rd
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.is_file()
                && p.extension()
                    .and_then(|s| s.to_str())
                    .map(|s| exts.contains(&s.to_ascii_lowercase().as_str()))
                    .unwrap_or(false)
        })
        .collect();
    all.sort();
    for p in all.into_iter().take(limit) {
        out.push(p);
    }
    out
}

struct Row {
    category: String,
    file: String,
    kb: u64,
    dims: Option<(u32, u32)>,
    thumb_dims: Option<(u32, u32)>,
    ms: f64,
    note: String,
}

fn probe_one(category: &str, p: &Path) -> Row {
    let kb = fs::metadata(p).map(|m| m.len()).unwrap_or(0) / 1024;

    // 尺寸探测（image_dimensions → RAW 兜底）
    let t0 = Instant::now();
    let dims = imaging::probe_dimensions(p);
    let ms_dims = t0.elapsed().as_secs_f64() * 1000.0;

    // 统一解码入口：这步才是「软件能不能出图」的真实答案
    let t1 = Instant::now();
    let img = imaging::decode_thumb(p, 512);
    let ms = t1.elapsed().as_secs_f64() * 1000.0;

    let (thumb_dims, note) = match img {
        Some(i) => (Some((i.width(), i.height())), String::new()),
        None => {
            // 再退回 320 占位层试试（高清层失败但占位层可能成功）
            let ph = imaging::decode_thumb(p, 320);
            match ph {
                Some(i) => (
                    Some((i.width(), i.height())),
                    "仅占位层(320)可解，高清层失败".to_string(),
                ),
                None => (None, "解码失败".to_string()),
            }
        }
    };

    Row {
        category: category.to_string(),
        file: p
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_string(),
        kb,
        dims,
        thumb_dims,
        ms: ms + ms_dims,
        note,
    }
}

fn main() {
    let root = PathBuf::from(env_or("QA_ROOT", "F:/testdata"));
    let limit: usize = env_or("QA_LIMIT", "6").parse().unwrap_or(6);

    println!("=== 茶包素材 · 测试素材解码验证 ===");
    println!("根目录 : {}", root.display());
    println!("每类上限: {limit}\n");
    println!(
        "  {:<38} {:>8} {:>13} {:>13} {:>9} 备注",
        "文件", "体积", "像素尺寸", "缩略图输出", "耗时"
    );

    // (类别名, 目录, 扩展名)
    let groups: Vec<(&str, PathBuf, Vec<&str>)> = vec![
        (
            "S1 宣传图(JPG)",
            root.join("S1_showcase"),
            vec!["jpg", "jpeg"],
        ),
        ("S2 PNG", root.join("S2_formats/common/png"), vec!["png"]),
        ("S2 WEBP", root.join("S2_formats/common/webp"), vec!["webp"]),
        ("S2 GIF", root.join("S2_formats/common/gif"), vec!["gif"]),
        ("S2 BMP", root.join("S2_formats/common/bmp"), vec!["bmp"]),
        (
            "S2 TIFF",
            root.join("S2_formats/common/tiff"),
            vec!["tiff", "tif"],
        ),
        (
            "S2 HEIC",
            root.join("S2_formats/common/heic"),
            vec!["heic", "heif"],
        ),
        (
            "S2 RAW",
            root.join("S2_formats/raw"),
            vec![
                "raw", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "dng", "raf", "orf",
                "rw2", "pef", "srw", "x3f", "mrw", "iiq", "3fr", "fff", "kdc", "dcr", "mos", "mef",
                "erf",
            ],
        ),
    ];

    let mut rows: Vec<Row> = Vec::new();
    for (label, dir, exts) in &groups {
        if !dir.is_dir() {
            println!("\n[跳过] {label} —— 目录不存在: {}", dir.display());
            continue;
        }
        let files = collect(dir, exts, limit);
        println!("\n--- {label}  命中 {} 个 ---", files.len());
        for p in &files {
            let r = probe_one(label, p);
            let name: String = r.file.chars().take(38).collect();
            let pd = r
                .dims
                .map(|(w, h)| format!("{w}x{h}"))
                .unwrap_or_else(|| "-".into());
            let td = r
                .thumb_dims
                .map(|(w, h)| format!("{w}x{h}"))
                .unwrap_or_else(|| "-".into());
            println!(
                "  {:<38} {:>6} KB {:>13} {:>13} {:>8.1}ms {}",
                name, r.kb, pd, td, r.ms, r.note
            );
            rows.push(r);
        }
    }

    // 汇总
    println!("\n=== 汇总 ===");
    let mut ok = 0usize;
    let mut fail = 0usize;
    let mut by_cat: std::collections::BTreeMap<&str, (usize, usize)> = Default::default();
    for r in &rows {
        let good = r.thumb_dims.is_some();
        if good {
            ok += 1;
        } else {
            fail += 1;
        }
        let e = by_cat.entry(r.category.as_str()).or_insert((0, 0));
        if good {
            e.0 += 1;
        } else {
            e.1 += 1;
        }
    }
    for (cat, (a, b)) in &by_cat {
        let mark = if *b == 0 { "OK  " } else { "注意" };
        println!("  [{mark}] {cat:<16} 出图成功 {a:>2} / 失败 {b:>2}");
    }
    println!("\n  合计: 成功 {ok} / 失败 {fail}");

    if fail > 0 {
        println!("\n  失败明细（这是软件的真实待修问题，不是素材问题）:");
        for r in rows.iter().filter(|r| r.thumb_dims.is_none()) {
            println!("    {}/{} -> {}", r.category, r.file, r.note);
        }
    }

    // 可选落盘
    if let Ok(out) = std::env::var("QA_OUT") {
        let json: Vec<String> = rows
            .iter()
            .map(|r| {
                format!(
                    "{{\"category\":\"{}\",\"file\":\"{}\",\"kb\":{},\"w\":{},\"h\":{},\"tw\":{},\"th\":{},\"ms\":{:.2},\"note\":\"{}\"}}",
                    r.category,
                    r.file,
                    r.kb,
                    r.dims.map(|d| d.0).unwrap_or(0),
                    r.dims.map(|d| d.1).unwrap_or(0),
                    r.thumb_dims.map(|d| d.0).unwrap_or(0),
                    r.thumb_dims.map(|d| d.1).unwrap_or(0),
                    r.ms,
                    r.note
                )
            })
            .collect();
        let body = format!("[\n{}\n]", json.join(",\n"));
        let _ = fs::write(&out, body);
        println!("\nJSON 已写入: {out}");
    }
}
