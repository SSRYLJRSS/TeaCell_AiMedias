//! 性能探针：用老板真实文件测图像引擎各路径耗时
//! 运行：cargo test --test perf_probe -- --ignored --nocapture

use std::time::Instant;

use teacell_ai_media_manager_lib::services::imaging;

/// R2-5：真实样本目录不再硬编码机器路径 —— 一律经环境变量注入；
/// 未设置时落到临时目录（不存在 → 各探针自检后优雅跳过）。
fn sample_dir() -> std::path::PathBuf {
    std::env::var_os("IMG_SAMPLE_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("chabaosucai_samples"))
}

#[test]
#[ignore = "手动性能探针"]
fn probe_real_files() {
    let cases = [
        (sample_dir().join("_1091370.JPG"), "相机 JPG"),
        (sample_dir().join("_1091370.RW2"), "松下 RW2"),
    ];
    for (path, label) in cases {
        let p = path.as_path();
        if !p.exists() {
            println!("{label}: 文件不存在，跳过");
            continue;
        }

        let t = Instant::now();
        let emb = imaging::embedded_preview(p);
        println!(
            "{label} 内嵌提取: {:?} → {}",
            t.elapsed(),
            emb.as_ref()
                .map(|b| format!("{}KB", b.len() / 1024))
                .unwrap_or("无".into())
        );

        let t = Instant::now();
        let thumb = imaging::decode_thumb(p, 320);
        println!(
            "{label} decode_thumb(320): {:?} → {:?}",
            t.elapsed(),
            thumb.as_ref().map(image::GenericImageView::dimensions)
        );

        let t = Instant::now();
        let hd = imaging::decode_thumb(p, 1280);
        println!(
            "{label} decode_thumb(1280): {:?} → {:?}",
            t.elapsed(),
            hd.as_ref().map(image::GenericImageView::dimensions)
        );

        let t = Instant::now();
        let full = image::open(p).ok();
        println!(
            "{label} 全解码(对照): {:?} → {:?}\n",
            t.elapsed(),
            full.as_ref().map(image::GenericImageView::dimensions)
        );
    }
}

#[test]
#[ignore = "手动诊断"]
fn probe_exif_error() {
    let p = sample_dir().join("_1091370.JPG");
    let f = std::fs::File::open(p).unwrap();
    match exif::Reader::new().read_from_container(&mut std::io::BufReader::new(f)) {
        Ok(ex) => {
            for tag in [
                exif::Tag::JPEGInterchangeFormat,
                exif::Tag::JPEGInterchangeFormatLength,
            ] {
                println!(
                    "THUMB  {:?}: {:?}",
                    tag,
                    ex.get_field(tag, exif::In::THUMBNAIL).map(|f| &f.value)
                );
                println!(
                    "PRIM   {:?}: {:?}",
                    tag,
                    ex.get_field(tag, exif::In::PRIMARY).map(|f| &f.value)
                );
            }
        }
        Err(e) => println!("kamadak 解析失败: {e}"),
    }
}

/// 阶段2 §5.2：RAW 样本基准——逐文件记录 format/file_size/resolution/embedded_preview_found/
/// embedded_preview_ms/placeholder_ms/hd_preview_ms/failure_reason。
/// 真实样本到位后跑：设环境变量 RAW_SAMPLES_DIR 指向含 CR3/NEF/ARW/RAF/RW2/DNG 的目录。
#[test]
#[ignore = "手动性能探针（需真实 RAW 样本目录）"]
fn probe_raw_benchmark() {
    let dir = std::path::PathBuf::from(
        // R2-5：样本目录经 RAW_SAMPLES_DIR 注入；未设置 → 空目录自检跳过
        std::env::var("RAW_SAMPLES_DIR").unwrap_or_default(),
    );
    if !dir.exists() {
        println!("RAW_BENCH 样本目录不存在: {dir:?}（设 RAW_SAMPLES_DIR 指向真实 RAW 目录后再跑）");
        return;
    }
    let raws = [
        "cr3", "nef", "arw", "raf", "rw2", "dng", "cr2", "orf", "pef", "srw", "x3f",
    ];
    let mut entries: Vec<_> = std::fs::read_dir(&dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    entries.sort();
    let mut rows = Vec::new();
    for p in entries {
        let ext = p
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or_default()
            .to_lowercase();
        if !raws.contains(&ext.as_str()) {
            continue;
        }
        let file_size = p.metadata().map(|m| m.len()).unwrap_or(0);
        let format = ext.clone();

        let t = Instant::now();
        let emb = imaging::embedded_preview(&p);
        let embedded_preview_ms = t.elapsed().as_millis() as u64;
        let embedded_preview_found = emb.is_some();

        let t = Instant::now();
        let ph = imaging::decode_thumb(&p, 320);
        let placeholder_ms = t.elapsed().as_millis() as u64;

        let t = Instant::now();
        let hd = imaging::decode_thumb(&p, 1280);
        let hd_preview_ms = t.elapsed().as_millis() as u64;

        let resolution = hd
            .as_ref()
            .or(ph.as_ref())
            .map(|i| {
                let (w, h) = image::GenericImageView::dimensions(i);
                format!("{w}x{h}")
            })
            .unwrap_or_default();
        let failure_reason = if hd.is_none() && ph.is_none() {
            "双黑图（须归因）".to_string()
        } else {
            String::new()
        };
        println!(
            "RAW_BENCH |{format}|{file_size}|{resolution}|emb={embedded_preview_found}|{embedded_preview_ms}ms|ph={placeholder_ms}ms|hd={hd_preview_ms}ms|{failure_reason}"
        );
        rows.push((
            format,
            file_size,
            resolution,
            embedded_preview_found,
            embedded_preview_ms,
            placeholder_ms,
            hd_preview_ms,
            failure_reason,
        ));
    }
    let found = rows.iter().filter(|r| r.3).count();
    println!(
        "RAW_BENCH 汇总: {} 样本，内嵌预览命中 {}，占比 {:.1}%",
        rows.len(),
        found,
        if rows.is_empty() {
            0.0
        } else {
            found as f64 / rows.len() as f64 * 100.0
        }
    );
}

/// F06：100 张混合格式占位层解码吞吐（合成样本，模拟入库占位图阶段）
#[test]
#[ignore = "手动性能探针"]
fn probe_mixed_decode_throughput() {
    use image::{ImageBuffer, Rgb};
    let dir = std::env::temp_dir().join(format!("bagertea_perf_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let img = image::DynamicImage::ImageRgb8(ImageBuffer::from_fn(2400, 1600, |x, y| {
        Rgb([(x % 256) as u8, (y % 256) as u8, 128u8])
    }));
    let exts = ["jpg", "png", "webp", "bmp", "tga", "tif"];
    let mut files = Vec::new();
    for i in 0..100 {
        let ext = exts[i % exts.len()];
        let p = dir.join(format!("mix_{i}.{ext}"));
        img.save(&p).unwrap();
        files.push(p);
    }

    let t = Instant::now();
    let mut ok = 0usize;
    for p in &files {
        if imaging::decode_thumb(p, 320).is_some() {
            ok += 1;
        }
    }
    let total = t.elapsed();
    println!(
        "混合占位层解码: {ok}/100 出图，总耗时 {:?}，均 {:?}/张",
        total,
        total / 100
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// F06：真实素材库 RAW 走查（默认 F:\pictures\20260726，不存在则提示）
/// 逐文件：内嵌提取 + 占位层 + 高清层耗时，汇总后验证零黑图
#[test]
#[ignore = "手动性能探针（需真实样本）"]
fn probe_raw_library_walk() {
    let dir = sample_dir();
    if !dir.exists() {
        println!("样本目录不存在: {dir:?}，请老板提供 RW2/CR3/NEF/ARW/HEIC 后再跑");
        return;
    }
    let mut n_ok = 0usize;
    let mut n_fail = 0usize;
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    entries.sort();
    for p in entries {
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or_default();
        if teacell_ai_media_manager_lib::utils::mime::asset_type_from_ext(ext) != Some("image") {
            continue;
        }
        let t = Instant::now();
        let ph = imaging::decode_thumb(&p, 320);
        let t_ph = t.elapsed();
        let t = Instant::now();
        let hd = imaging::decode_thumb(&p, 1280);
        let t_hd = t.elapsed();
        match (&ph, &hd) {
            (Some(_), _) => n_ok += 1,
            (None, Some(_)) => n_ok += 1,
            (None, None) => n_fail += 1,
        }
        println!(
            "{} 占位 {:?}({}) 高清 {:?}({})",
            p.file_name().unwrap_or_default().to_string_lossy(),
            t_ph,
            if ph.is_some() { "✓" } else { "×" },
            t_hd,
            if hd.is_some() { "✓" } else { "×" }
        );
    }
    println!("走查汇总: {n_ok} 出图 / {n_fail} 黑图（黑图须逐一归因）");
}

// ═══════════════ W7-3 性能探针（指导书 §W7-3） ═══════════════

fn mark_all_managed(conn: &rusqlite::Connection) {
    conn.execute(
        "UPDATE libraries
            SET health='ready',root_path='C:/fixture-root',root_identity='fixture-root',
                library_epoch=lower(hex(randomblob(16)))
          WHERE singleton=1 AND (health!='ready' OR root_identity IS NULL)",
        [],
    )
    .unwrap();
    let library = teacell_ai_media_manager_lib::db::libraries::current(conn)
        .unwrap()
        .unwrap();
    conn.execute(
        "UPDATE managed_asset_locations
            SET status='verified',reason='verified',library_id=?1,library_epoch=?2,
                relative_path=printf('asset-%d.jpg',asset_id),
                file_identity='fixture-file',content_sha256=?3,
                ownership_receipt='fixture-receipt'",
        rusqlite::params![library.library_id, library.library_epoch, "a".repeat(64)],
    )
    .unwrap();
}

/// 深分页：3 万素材 + 复杂布尔表达式的深翻页耗时（① S6：COUNT + LIMIT/OFFSET 成本随页深增长）
#[test]
fn probe_deep_pagination() {
    use teacell_ai_media_manager_lib::db::{self, assets};
    let conn = db::init_memory().unwrap();
    // 批量事务插入 3 万条
    let t = std::time::Instant::now();
    {
        let tx = conn.unchecked_transaction().unwrap();
        for i in 0..30_000 {
            assets::insert(
                &tx,
                &format!("d:/p/{i:05}.jpg"),
                &format!("{i:05}.jpg"),
                "jpg",
                1024,
                "image/jpeg",
                1700000000000 + i * 1000,
            )
            .unwrap();
            if i % 5000 == 0 {
                println!("插入 {i}…");
            }
        }
        tx.commit().unwrap();
    }
    mark_all_managed(&conn);
    println!("插入 3 万素材: {:?}", t.elapsed());

    let filter = assets::AssetFilter {
        limit: 60,
        offset: 0,
        ..Default::default()
    };
    for depth in [0, 10_000, 20_000, 29_940] {
        let mut f = filter.clone();
        f.offset = depth;
        let t = std::time::Instant::now();
        let page = assets::list(&conn, &f).unwrap();
        println!(
            "深翻页 offset={depth}: {:?} → {} 条 / total {}",
            t.elapsed(),
            page.items.len(),
            page.total
        );
    }
}

/// phash 相似扫描：3 万行分桶 + 汉明比较耗时（验证「毫秒级」断言）
#[test]
fn probe_phash_scan_30k() {
    use teacell_ai_media_manager_lib::db::{self, assets, dedup};
    let conn = db::init_memory().unwrap();
    let t = std::time::Instant::now();
    {
        let tx = conn.unchecked_transaction().unwrap();
        // 伪随机 phash：同批次种子制造相近哈希（模拟同画面），其余随机
        let mut seed = 0x9E3779B97F4A7C15u64;
        for i in 0..30_000 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            let phash = if i % 500 == 0 {
                0xABCD_0000_0000_0000 | (seed & 0xFF)
            } else {
                seed
            };
            assets::insert(
                &tx,
                &format!("d:/p/{i:05}.jpg"),
                &format!("{i:05}.jpg"),
                "jpg",
                1024,
                "image/jpeg",
                1700000000000,
            )
            .unwrap();
            assets::set_phash(&tx, i as i64 + 1, phash).unwrap();
        }
        tx.commit().unwrap();
    }
    mark_all_managed(&conn);
    println!("插入 3 万行 + phash: {:?}", t.elapsed());
    let t = std::time::Instant::now();
    let groups = dedup::scan_similar_groups(&conn, 8, false, &[]).unwrap();
    println!(
        "scan_similar_groups(3万行, 阈值8): {:?} → {} 组",
        t.elapsed(),
        groups.len()
    );
}

/// RAW 宽高回填：真机 205 张 RW2 的量级感知（文件不存在则跳过）
#[test]
#[ignore = "手动性能探针"]
fn probe_raw_dimension_walk() {
    let dir = sample_dir();
    if !dir.is_dir() {
        println!("RAW 目录不存在，跳过");
        return;
    }
    let entries: Vec<_> = std::fs::read_dir(dir)
        .map(|rd| rd.filter_map(|e| e.ok()).map(|e| e.path()).collect())
        .unwrap_or_default();
    let raws: Vec<_> = entries
        .iter()
        .filter(|p| {
            p.extension()
                .and_then(|e| e.to_str())
                .is_some_and(|e| e.eq_ignore_ascii_case("rw2"))
        })
        .collect();
    println!("发现 {} 张 RW2，逐张 probe_dimensions…", raws.len());
    let t = std::time::Instant::now();
    let mut ok = 0;
    for p in &raws {
        if let Some((w, h)) =
            teacell_ai_media_manager_lib::services::raw_decode::probe_dimensions(p)
        {
            ok += 1;
            let _ = (w, h);
        }
    }
    println!(
        "probe_dimensions 全部完成: {:?}（成功 {ok}/{}）",
        t.elapsed(),
        raws.len()
    );
}

/// 提示词 token 量级：W5a 后 system + user 实际字符数（近似 token ≈ 字符数，中文 1 字 ≈ 1 token）
#[test]
fn probe_prompt_size() {
    use teacell_ai_media_manager_lib::db::{self, tag_facets};
    let conn = db::init_memory().unwrap();
    let facets = tag_facets::build_prompt_context(&conn, "all").unwrap();
    // 用中等分面数模拟真实场景（含用户自建分面时）
    let facets = if facets.len() >= 8 {
        facets
    } else {
        for i in 0..(8 - facets.len()) {
            tag_facets::create(
                &conn,
                &format!("user_facet_{i}"),
                &format!("用户分面{i}"),
                "测试描述",
                "multi",
                Some(5),
                "all",
            )
            .unwrap();
        }
        tag_facets::build_prompt_context(&conn, "all").unwrap()
    };
    let system =
        teacell_ai_media_manager_lib::services::super_search_ai::build_system_prompt(&facets);
    // user 段在 request_intent 内联拼接，这里复刻（含分面说明段；词典与查询句按真实量级估算）
    let mut user = String::from(
        "标签词典（规范名 | aliases: 可搜索别名）
- 示例标签 1
- 示例标签 2

分面说明
",
    );
    for f in &facets {
        user.push_str(&format!(
            "- {}(key={}) selection={} max={}: {}
",
            f.display_name,
            f.key,
            f.selection_mode,
            f.max_items.unwrap_or(3),
            f.description
        ));
    }
    user.push_str(
        "
用户查询：<query>海边日落 2025</query>
请输出解析结果。",
    );
    println!("分面数: {}", facets.len());
    println!("system 字符数: {}（≈token 量级）", system.chars().count());
    println!("user 字符数: {}（≈token 量级）", user.chars().count());
    println!(
        "合计: {} 字符",
        system.chars().count() + user.chars().count()
    );
}
