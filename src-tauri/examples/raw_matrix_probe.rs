//! RAW 全格式解码验证（一次性工具，不进主程序）。
//!
//! 为什么必须单独写这个：
//!   rawler 0.7.2 的 `RawImage::cropped_cfa()` 是上游未实现的 `todo!()`。
//!   `todo!()` 抛的是 **panic 而非 Result::Err**，调用方的 `.ok()?` 完全拦不住，
//!   一旦触发就是进程级 abort —— 在 Tauri 里等于用户导入一个 DNG 整个软件消失。
//!
//!   所以本工具用 `catch_unwind` 把每个文件的解码包起来，让 panic 隔离在单文件内，
//!   从而能把 25 种 RAW 扩展名**全部跑完**，输出准确的「哪种格式会炸」清单。
//!
//! 运行：
//!   cd src-tauri
//!   $env:QA_RAW="F:/testdata/S2_formats/raw"; cargo run --example raw_matrix_probe
//! 可选：
//!   $env:QA_OUT="F:/testdata/_raw_matrix.json"

use std::fs;
use std::panic;
use std::path::{Path, PathBuf};
use std::time::Instant;

use bagertea_ai_media_v2_lib::services::imaging;

fn env_or(key: &str, def: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| def.to_string())
}

#[derive(Clone, Copy, PartialEq)]
enum Verdict {
    /// 尺寸探测 + 解码都成功
    FullOk,
    /// 尺寸能出，但解码返回 None（走降级，不崩）
    DimOnly,
    /// panic（进程级风险）
    Panic,
    /// 干净失败：返回 None，不崩
    CleanFail,
}

impl Verdict {
    fn tag(&self) -> &'static str {
        match self {
            Verdict::FullOk => "PASS   ",
            Verdict::DimOnly => "降级   ",
            Verdict::Panic => "**PANIC**",
            Verdict::CleanFail => "失败   ",
        }
    }
}

struct Row {
    ext: String,
    file: String,
    kb: u64,
    dims: Option<(u32, u32)>,
    decoded: Option<(u32, u32)>,
    ms: f64,
    verdict: Verdict,
    panic_msg: String,
}

/// 在独立 panic 边界内跑一个 RAW 文件的完整链路
fn probe(path: &Path, ext: &str) -> Row {
    let kb = fs::metadata(path).map(|m| m.len()).unwrap_or(0) / 1024;
    let file = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("?")
        .to_string();
    let p = path.to_path_buf();

    // 静默 panic 输出，避免刷屏（我们自己捕获并记录）
    let prev = panic::take_hook();
    panic::set_hook(Box::new(|_| {}));
    let t0 = Instant::now();
    let result = panic::catch_unwind(move || {
        let dims = imaging::probe_dimensions(&p);
        let img = imaging::decode_thumb(&p, 512);
        (dims, img.map(|i| (i.width(), i.height())))
    });
    panic::set_hook(prev);
    let ms = t0.elapsed().as_secs_f64() * 1000.0;

    match result {
        Ok((dims, decoded)) => {
            let verdict = if decoded.is_some() {
                Verdict::FullOk
            } else if dims.is_some() {
                Verdict::DimOnly
            } else {
                Verdict::CleanFail
            };
            Row {
                ext: ext.to_string(),
                file,
                kb,
                dims,
                decoded,
                ms,
                verdict,
                panic_msg: String::new(),
            }
        }
        Err(e) => {
            let msg = if let Some(s) = e.downcast_ref::<&str>() {
                (*s).to_string()
            } else if let Some(s) = e.downcast_ref::<String>() {
                s.clone()
            } else {
                "未知 panic".to_string()
            };
            Row {
                ext: ext.to_string(),
                file,
                kb,
                dims: None,
                decoded: None,
                ms,
                verdict: Verdict::Panic,
                panic_msg: msg,
            }
        }
    }
}

fn main() {
    let dir = PathBuf::from(env_or("QA_RAW", "F:/testdata/S2_formats/raw"));
    println!("=== RAW 全格式解码验证（panic 隔离）===");
    println!("目录: {}\n", dir.display());

    if !dir.is_dir() {
        println!("目录不存在，退出");
        return;
    }

    // 收集全部 RAW 文件
    let mut files: Vec<PathBuf> = fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .map(|e| e.path())
                .filter(|p| {
                    p.is_file()
                        && !p.to_string_lossy().ends_with(".part")
                        && !p.to_string_lossy().ends_with("_manifest.json")
                })
                .collect()
        })
        .unwrap_or_default();
    files.sort();

    println!("发现 {} 个 RAW 文件\n", files.len());
    println!(
        "  {:<6} {:<42} {:>8} {:>12} {:>12} {:>9}  判定",
        "扩展名", "文件", "体积", "像素尺寸", "解码输出", "耗时"
    );

    let mut rows = Vec::new();
    for p in &files {
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("?")
            .to_ascii_lowercase();
        let r = probe(p, &ext);
        let name: String = r.file.chars().take(42).collect();
        let pd = r
            .dims
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_else(|| "-".into());
        let dd = r
            .decoded
            .map(|(w, h)| format!("{w}x{h}"))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {:<6} {:<42} {:>6}KB {:>12} {:>12} {:>7.0}ms  {}",
            r.ext,
            name,
            r.kb,
            pd,
            dd,
            r.ms,
            r.verdict.tag()
        );
        if !r.panic_msg.is_empty() {
            println!("         ↳ panic: {}", r.panic_msg);
        }
        rows.push(r);
    }

    // 按扩展名聚合
    println!("\n=== 按扩展名汇总 ===");
    let mut order: Vec<String> = Vec::new();
    let mut agg: std::collections::BTreeMap<String, (usize, usize, usize, usize)> =
        Default::default();
    for r in &rows {
        if !order.contains(&r.ext) {
            order.push(r.ext.clone());
        }
        let e = agg.entry(r.ext.clone()).or_insert((0, 0, 0, 0));
        match r.verdict {
            Verdict::FullOk => e.0 += 1,
            Verdict::DimOnly => e.1 += 1,
            Verdict::Panic => e.2 += 1,
            Verdict::CleanFail => e.3 += 1,
        }
    }
    order.sort();
    let mut panic_total = 0;
    println!(
        "  {:<6} {:>6} {:>8} {:>10} {:>8}",
        "扩展名", "PASS", "仅尺寸", "PANIC", "干净失败"
    );
    for ext in &order {
        let (a, b, c, d) = agg[ext];
        panic_total += c;
        let mark = if c > 0 { " ⚠" } else { "" };
        println!("  {:<6} {:>6} {:>8} {:>10} {:>8}{}", ext, a, b, c, d, mark);
    }

    let pass = rows.iter().filter(|r| r.verdict == Verdict::FullOk).count();
    let dim = rows
        .iter()
        .filter(|r| r.verdict == Verdict::DimOnly)
        .count();
    let cf = rows
        .iter()
        .filter(|r| r.verdict == Verdict::CleanFail)
        .count();

    println!("\n=== 结论 ===");
    println!("  总文件        : {}", rows.len());
    println!("  完整解码 PASS : {pass}");
    println!("  仅尺寸(降级)  : {dim}   —— 不崩，走占位图/内嵌预览降级，属可接受");
    println!("  干净失败      : {cf}   —— 返回 None，不崩，属可接受");
    println!("  **PANIC**     : {panic_total}   —— 进程级风险，必须修");

    if panic_total > 0 {
        let exts: Vec<&str> = order
            .iter()
            .filter(|e| agg[*e].2 > 0)
            .map(|s| s.as_str())
            .collect();
        println!("\n  ⚠ 触发 panic 的扩展名: {}", exts.join(", "));
        println!("  根因: rawler 0.7.2 RawImage::cropped_cfa() 为上游 todo!()，");
        println!("        而 src/services/raw_decode.rs:52 无条件调用它。");
        println!("  影响: 用户导入这些格式的 RAW → 整个应用进程 abort（非降级、非报错）。");
    }

    if let Ok(out) = std::env::var("QA_OUT") {
        let json: Vec<String> = rows
            .iter()
            .map(|r| {
                let v = match r.verdict {
                    Verdict::FullOk => "PASS",
                    Verdict::DimOnly => "DIM_ONLY",
                    Verdict::Panic => "PANIC",
                    Verdict::CleanFail => "CLEAN_FAIL",
                };
                format!(
                    "{{\"ext\":\"{}\",\"file\":\"{}\",\"kb\":{},\"w\":{},\"h\":{},\"dw\":{},\"dh\":{},\"ms\":{:.1},\"verdict\":\"{}\",\"panic\":\"{}\"}}",
                    r.ext,
                    r.file.replace('\\', "\\\\").replace('"', "\\\""),
                    r.kb,
                    r.dims.map(|d| d.0).unwrap_or(0),
                    r.dims.map(|d| d.1).unwrap_or(0),
                    r.decoded.map(|d| d.0).unwrap_or(0),
                    r.decoded.map(|d| d.1).unwrap_or(0),
                    r.ms,
                    v,
                    r.panic_msg.replace('\\', "\\\\").replace('"', "\\\"")
                )
            })
            .collect();
        let _ = fs::write(&out, format!("[\n{}\n]", json.join(",\n")));
        println!("\n  JSON: {out}");
    }
}
