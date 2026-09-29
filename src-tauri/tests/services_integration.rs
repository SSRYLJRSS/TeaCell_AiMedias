//! T03 验收测试：入库管线 / 双层缩略图 / 导出 / 删除联动清理
//! 运行：cargo test

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use bagertea_ai_media_v2_lib::db::assets::AssetFilter;
use bagertea_ai_media_v2_lib::db::{self, assets, export};
use bagertea_ai_media_v2_lib::error::AppResult;
use bagertea_ai_media_v2_lib::services::{export_local, importer, thumbnail::ThumbnailService};
use bagertea_ai_media_v2_lib::state::Database;

fn default_import_options(temp_dir: &std::path::Path) -> AppResult<importer::ImportOptions> {
    let library_root = temp_dir.join("library");
    std::fs::create_dir_all(&library_root)?;
    Ok(importer::ImportOptions {
        library_root: Some(library_root.to_string_lossy().into_owned()),
        ..Default::default()
    })
}

/// 生成 N 张测试 JPEG（image crate 直接出图）
fn make_jpegs(dir: &std::path::Path, n: usize) {
    for i in 0..n {
        let img = image::RgbImage::from_fn(800, 600, |x, y| {
            image::Rgb([((x + i as u32) % 256) as u8, (y % 256) as u8, 150])
        });
        image::DynamicImage::ImageRgb8(img)
            .save_with_format(
                dir.join(format!("photo{i:03}.jpg")),
                image::ImageFormat::Jpeg,
            )
            .expect("生成测试图失败");
    }
}

// ① 入库 100 张图片：全部成功 + 每张立即有占位图 + 重复导入识别
#[test]
fn import_100_images_with_placeholders() -> AppResult<()> {
    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src)?;
    make_jpegs(&src, 100);

    let dbm = std::sync::Arc::new(Database::new(db::init_memory()?));
    let thumbs = ThumbnailService::new(&tmp.path().join("data"))?;
    let cancel = AtomicBool::new(false);

    let r = importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &default_import_options(tmp.path())?,
        &cancel,
        |_| {},
    )?;
    assert_eq!(r.imported, 100);
    assert_eq!(r.failed, 0, "errors: {:?}", r.errors);

    // 每张立即有占位图（入库后无空白）
    let page = assets::list(
        &dbm.lock().unwrap(),
        &AssetFilter {
            limit: 200,
            ..Default::default()
        },
    )?;
    assert_eq!(page.total, 100);
    for a in &page.items {
        let p = a.placeholder_path.as_ref().expect("缺占位图路径");
        assert!(std::path::Path::new(p).exists(), "占位图文件不存在: {p}");
        assert_eq!(a.width, Some(800));
        assert_eq!(a.height, Some(600));
    }

    // 重复导入 → duplicates
    let r2 = importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &default_import_options(tmp.path())?,
        &cancel,
        |_| {},
    )?;
    assert_eq!(r2.duplicates, 100);
    assert_eq!(r2.imported, 0);
    Ok(())
}

// ①a 复用确认阶段已经完成的后端预检清单继续入库。
#[test]
fn import_reuses_preflight_candidate_plan() -> AppResult<()> {
    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src)?;
    make_jpegs(&src, 1);

    let dbm = std::sync::Arc::new(Database::new(db::init_memory()?));
    let thumbs = ThumbnailService::new(&tmp.path().join("data"))?;
    let cancel = AtomicBool::new(false);
    let paths = [src.join("photo000.jpg").to_string_lossy().into_owned()];
    let plan = importer::inspect_paths(&paths);
    assert_eq!(plan.items.len(), 1);

    let result = importer::import_preflight_plan_with_task_id(
        &dbm,
        &thumbs,
        plan,
        &default_import_options(tmp.path())?,
        &cancel,
        "preflight-test".into(),
        |_| {},
    )?;

    assert_eq!(result.imported, 1);
    assert_eq!(result.failed, 0, "errors: {:?}", result.errors);
    assert_eq!(
        assets::list(&dbm.lock().unwrap(), &AssetFilter::default())?.total,
        1
    );
    Ok(())
}

// ② 高清缩略图：按需生成 + 二次命中缓存
#[test]
fn hd_thumbnail_generate_and_cache() -> AppResult<()> {
    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src)?;
    make_jpegs(&src, 1);

    let dbm = std::sync::Arc::new(Database::new(db::init_memory()?));
    let thumbs = ThumbnailService::new(&tmp.path().join("data"))?;
    let cancel = AtomicBool::new(false);
    importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &default_import_options(tmp.path())?,
        &cancel,
        |_| {},
    )?;

    let page = assets::list(&dbm.lock().unwrap(), &AssetFilter::default())?;
    let id = page.items[0].id;

    let p1 = thumbs.get_or_create_hd(&dbm, id, Some(512))?;
    assert!(p1.exists());
    // 已回填 hd_thumbnail_path
    assert!(assets::get(&dbm.lock().unwrap(), id)?
        .hd_thumbnail_path
        .is_some());
    // 二次调用命中缓存（同一路径，文件未重建）
    let mtime = std::fs::metadata(&p1)?.modified()?;
    let p2 = thumbs.get_or_create_hd(&dbm, id, Some(512))?;
    assert_eq!(p1, p2);
    assert_eq!(std::fs::metadata(&p2)?.modified()?, mtime);
    Ok(())
}

// ③ 本地导出：复制模式文件完整 + 任务状态流转
#[test]
fn export_copy_integrity() -> AppResult<()> {
    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src)?;
    make_jpegs(&src, 5);

    let dbm = std::sync::Arc::new(Database::new(db::init_memory()?));
    let thumbs = ThumbnailService::new(&tmp.path().join("data"))?;
    let cancel = AtomicBool::new(false);
    importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &default_import_options(tmp.path())?,
        &cancel,
        |_| {},
    )?;

    let page = assets::list(&dbm.lock().unwrap(), &AssetFilter::default())?;
    let ids: Vec<i64> = page.items.iter().map(|a| a.id).collect();
    let dest = tmp.path().join("out");
    let task = export::create_task(
        &dbm.lock().unwrap(),
        "local",
        ids.len() as i64,
        Some(&dest.to_string_lossy()),
        None,
    )?;

    export_local::export_local(
        &dbm,
        task.id,
        &ids,
        &dest.to_string_lossy(),
        "copy",
        "flat",
        &Arc::new(AtomicBool::new(false)),
        |_| {},
    )?;

    let t = export::get_task(&dbm.lock().unwrap(), task.id)?;
    assert_eq!(t.status, "done");
    assert_eq!(t.done, 5);
    for a in &page.items {
        let copied = dest.join(&a.file_name);
        assert!(copied.exists());
        // 文件完整：尺寸一致
        let src_size = std::fs::metadata(&a.file_path)?.len();
        assert_eq!(std::fs::metadata(&copied)?.len(), src_size);
    }
    Ok(())
}

// ④ 删除素材联动清理两层缩略图
#[test]
fn delete_cleans_thumbnails() -> AppResult<()> {
    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src)?;
    make_jpegs(&src, 2);

    let dbm = std::sync::Arc::new(Database::new(db::init_memory()?));
    let data_dir = tmp.path().join("data");
    let thumbs = ThumbnailService::new(&data_dir)?;
    let cancel = AtomicBool::new(false);
    importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &default_import_options(tmp.path())?,
        &cancel,
        |_| {},
    )?;

    let page = assets::list(&dbm.lock().unwrap(), &AssetFilter::default())?;
    let id = page.items[0].id;
    let hd = thumbs.get_or_create_hd(&dbm, id, Some(512))?;
    let ph = thumbs.placeholder_path(id);
    assert!(hd.exists() && ph.exists());

    assets::delete(&dbm.lock().unwrap(), &[id])?;
    thumbs.delete_for_asset(id);
    assert!(!ph.exists(), "占位图未清理");
    assert!(!hd.exists(), "高清图未清理");
    Ok(())
}

// ⑤ 总库/分库托管入库（R-32）：复制进 总库/分库/ + 批量改名 + 非法分库名快速失败
#[test]
fn managed_library_import() -> AppResult<()> {
    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src)?;
    make_jpegs(&src, 3);

    let dbm = std::sync::Arc::new(Database::new(db::init_memory()?));
    let thumbs = ThumbnailService::new(&tmp.path().join("data"))?;
    let cancel = AtomicBool::new(false);
    let root = tmp.path().join("library");
    std::fs::create_dir_all(&root)?;
    let opts = importer::ImportOptions {
        library_root: Some(root.to_string_lossy().into_owned()),
        collection: Some("旅行".into()),
        rename_pattern: Some("{分库}_{序号:3}".into()),
    };
    let r = importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &opts,
        &cancel,
        |_| {},
    )?;
    assert_eq!(r.imported, 3);
    // 复制到 总库/分库/ 且按 分库名_序号 改名
    let dest = root.join("旅行");
    assert!(dest.join("旅行_001.jpg").exists());
    assert!(dest.join("旅行_003.jpg").exists());
    // 库中路径指向托管副本
    let page = assets::list(&dbm.lock().unwrap(), &AssetFilter::default())?;
    assert!(page.items.iter().all(|a| a.file_path.contains("旅行")));
    // 非法分库名（路径穿越）整批拒绝
    let bad = importer::ImportOptions {
        collection: Some("../escape".into()),
        ..opts
    };
    let r2 = importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &bad,
        &cancel,
        |_| {},
    );
    assert!(r2.is_err());
    Ok(())
}

// ⑥ B04：导出 move 模式后库记录 file_path/file_name 同步更新
#[test]
fn b04_export_move_updates_db_file_path() -> AppResult<()> {
    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src)?;
    make_jpegs(&src, 1);

    let dbm = std::sync::Arc::new(Database::new(db::init_memory()?));
    let thumbs = ThumbnailService::new(&tmp.path().join("data"))?;
    let cancel = AtomicBool::new(false);
    importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &default_import_options(tmp.path())?,
        &cancel,
        |_| {},
    )?;

    let page = assets::list(&dbm.lock().unwrap(), &AssetFilter::default())?;
    let id = page.items[0].id;
    let old_path = page.items[0].file_path.clone();
    let old_name = page.items[0].file_name.clone();
    assert!(std::path::Path::new(&old_path).exists(), "原文件应存在");

    // 导出 move 到 dest 目录
    let dest = tmp.path().join("out");
    let task = export::create_task(
        &dbm.lock().unwrap(),
        "local",
        1,
        Some(&dest.to_string_lossy()),
        None,
    )?;
    export_local::export_local(
        &dbm,
        task.id,
        &[id],
        &dest.to_string_lossy(),
        "move",
        "flat",
        &Arc::new(AtomicBool::new(false)),
        |_| {},
    )?;

    // 任务完成
    let t = export::get_task(&dbm.lock().unwrap(), task.id)?;
    assert_eq!(t.status, "done", "move 导出应成功: error={:?}", t.error);
    assert_eq!(t.done, 1);

    // B04：file_path 已更新指向新路径
    let asset = assets::get(&dbm.lock().unwrap(), id)?;
    assert_ne!(
        asset.file_path, old_path,
        "file_path 应已更新（旧={}, 新={})",
        old_path, asset.file_path
    );
    assert!(
        asset.file_path.contains("out"),
        "file_path 应指向导出目录: {}",
        asset.file_path
    );
    assert_eq!(asset.file_name, old_name, "无同名冲突时 file_name 应不变");
    // 目标文件存在
    assert!(
        std::path::Path::new(&asset.file_path).exists(),
        "目标文件应存在: {}",
        asset.file_path
    );
    // 源文件已被移动（不存在）
    assert!(
        !std::path::Path::new(&old_path).exists(),
        "源文件应已被移动（不存在）"
    );
    Ok(())
}

// ⑦ B04：导出 move 到同名冲突目录 → file_name 加 (1) 后缀 + 库记录同步
#[test]
fn b04_export_move_same_name_suffix_updates_db() -> AppResult<()> {
    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src)?;
    make_jpegs(&src, 1); // photo000.jpg

    let dbm = std::sync::Arc::new(Database::new(db::init_memory()?));
    let thumbs = ThumbnailService::new(&tmp.path().join("data"))?;
    let cancel = AtomicBool::new(false);
    importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &default_import_options(tmp.path())?,
        &cancel,
        |_| {},
    )?;

    let page = assets::list(&dbm.lock().unwrap(), &AssetFilter::default())?;
    let id = page.items[0].id;
    let orig_name = page.items[0].file_name.clone(); // photo000.jpg

    // 预置同名文件，迫使 unique_dest 加 (1) 后缀
    let dest = tmp.path().join("out");
    std::fs::create_dir_all(&dest)?;
    std::fs::copy(src.join(&orig_name), dest.join(&orig_name))?;

    let task = export::create_task(
        &dbm.lock().unwrap(),
        "local",
        1,
        Some(&dest.to_string_lossy()),
        None,
    )?;
    export_local::export_local(
        &dbm,
        task.id,
        &[id],
        &dest.to_string_lossy(),
        "move",
        "flat",
        &Arc::new(AtomicBool::new(false)),
        |_| {},
    )?;

    let t = export::get_task(&dbm.lock().unwrap(), task.id)?;
    assert_eq!(t.status, "done", "move 应成功: {:?}", t.error);

    // B04：file_name 应带 (1) 后缀
    let asset = assets::get(&dbm.lock().unwrap(), id)?;
    let stem = std::path::Path::new(&orig_name)
        .file_stem()
        .unwrap()
        .to_string_lossy();
    let ext = std::path::Path::new(&orig_name)
        .extension()
        .unwrap()
        .to_string_lossy();
    let expected = format!("{stem}(1).{ext}");
    assert_eq!(
        asset.file_name, expected,
        "file_name 应加 (1) 后缀: 期望={expected}, 实际={}",
        asset.file_name
    );
    assert!(
        std::path::Path::new(&asset.file_path).exists(),
        "新路径文件应存在"
    );
    Ok(())
}

// ⑧ B01：导入取消（cancel=true 从开始）→ 0 导入 + 取消提示 + 库无记录
#[test]
fn b01_import_cancel_zero_imported() -> AppResult<()> {
    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src)?;
    make_jpegs(&src, 10);

    let dbm = std::sync::Arc::new(Database::new(db::init_memory()?));
    let thumbs = ThumbnailService::new(&tmp.path().join("data"))?;
    let cancel = AtomicBool::new(true); // 立即取消

    let r = importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &default_import_options(tmp.path())?,
        &cancel,
        |_| {},
    )
    .expect_err("入库检查期间取消应返回明确的取消状态");
    assert_eq!(r.code(), "CANCELLED");

    // 取消后不复制文件、不写库；随后可安全重新导入。
    let page = assets::list(&dbm.lock().unwrap(), &AssetFilter::default())?;
    assert_eq!(page.total, 0, "取消后库应无记录");

    // 同批文件可重新导入（取消不污染状态）
    let cancel2 = AtomicBool::new(false);
    let r2 = importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &default_import_options(tmp.path())?,
        &cancel2,
        |_| {},
    )?;
    assert_eq!(r2.imported, 10, "取消后重新导入应全部成功");
    assert_eq!(r2.failed, 0);
    Ok(())
}

// ⑨ B06b：导出同名冲突超限报错（unique_dest 返回 Err 而非覆盖）
#[test]
fn b06b_export_same_name_exhaustion_errors() -> AppResult<()> {
    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src)?;
    make_jpegs(&src, 1); // photo000.jpg

    let dbm = std::sync::Arc::new(Database::new(db::init_memory()?));
    let thumbs = ThumbnailService::new(&tmp.path().join("data"))?;
    let cancel = AtomicBool::new(false);
    importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &default_import_options(tmp.path())?,
        &cancel,
        |_| {},
    )?;

    let page = assets::list(&dbm.lock().unwrap(), &AssetFilter::default())?;
    let id = page.items[0].id;
    let orig_name = page.items[0].file_name.clone();

    // 预置 1000 个同名占位（原始名 + (1)..(999)），unique_dest 循环 1..1000 耗尽
    let dest = tmp.path().join("out");
    std::fs::create_dir_all(&dest)?;
    let stem = std::path::Path::new(&orig_name)
        .file_stem()
        .unwrap()
        .to_string_lossy();
    let ext = std::path::Path::new(&orig_name)
        .extension()
        .unwrap()
        .to_string_lossy();
    // 用空文件占位（unique_dest 只检查 .exists()，不读内容）
    let _ = std::fs::File::create(dest.join(&orig_name))?;
    for i in 1..1000 {
        let _ = std::fs::File::create(dest.join(format!("{stem}({i}).{ext}")))?;
    }

    let task = export::create_task(
        &dbm.lock().unwrap(),
        "local",
        1,
        Some(&dest.to_string_lossy()),
        None,
    )?;
    // 导出 copy 到同名耗尽的目录 → unique_dest 应返回 Err
    let r = export_local::export_local(
        &dbm,
        task.id,
        &[id],
        &dest.to_string_lossy(),
        "copy",
        "flat",
        &Arc::new(AtomicBool::new(false)),
        |_| {},
    );

    // B06b 核心验证：应返回 Err（不静默覆盖已有文件）
    assert!(r.is_err(), "同名冲突耗尽应报错而非覆盖: {:?}", r);
    if let Err(e) = &r {
        let msg = format!("{e}");
        assert!(
            msg.contains("同名文件过多"),
            "错误信息应含'同名文件过多': {msg}"
        );
    }

    // BUG-QA-1 修复验证：unique_dest 失败时 r=Err(e) 被 if let Err(e) = r 捕获，
    // finish_task("failed") 正确执行，任务状态应为 "failed"
    let t = export::get_task(&dbm.lock().unwrap(), task.id)?;
    assert_eq!(
        t.status, "failed",
        "unique_dest 失败应触发 finish_task(\"failed\")，任务状态应为 failed"
    );
    Ok(())
}

// 阶段 1：入库进度事件按 queued→scanning→hashing→processing→previewing→done 顺序上报，
// 后端只发阶段进度（含 taskId），其中 hashing/processing/previewing 相对次序正确。
#[test]
fn import_phase_progress_sequence() -> AppResult<()> {
    let tmp = tempfile::tempdir()?;
    let src = tmp.path().join("src");
    std::fs::create_dir_all(&src)?;
    make_jpegs(&src, 3);

    let dbm = std::sync::Arc::new(Database::new(db::init_memory()?));
    let thumbs = ThumbnailService::new(&tmp.path().join("data"))?;
    let cancel = AtomicBool::new(false);

    let phases = std::sync::Mutex::new(Vec::<String>::new());
    let progress = |p: importer::ImportProgress| {
        phases.lock().unwrap().push(p.phase.clone());
    };
    let r = importer::import_paths(
        &dbm,
        &thumbs,
        &[src.to_string_lossy().into_owned()],
        &default_import_options(tmp.path())?,
        &cancel,
        progress,
    )?;
    assert_eq!(r.imported, 3);

    let phases = phases.into_inner().unwrap();
    assert_eq!(phases.first().map(String::as_str), Some("queued"));
    assert_eq!(phases.last().map(String::as_str), Some("done"));
    for phase in ["scanning", "hashing", "processing", "previewing"] {
        assert!(phases.iter().any(|p| p == phase), "缺少阶段事件：{phase}");
    }
    // 关键相对次序：hashing < processing < previewing < done
    let idx = |s: &str| phases.iter().position(|x| x == s).expect("阶段应存在");
    assert!(idx("hashing") < idx("processing"));
    assert!(idx("processing") < idx("previewing"));
    assert!(idx("previewing") < idx("done"));
    Ok(())
}
