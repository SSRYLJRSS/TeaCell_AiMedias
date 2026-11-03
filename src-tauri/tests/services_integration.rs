//! T03 验收测试：入库管线 / 双层缩略图 / 导出 / 删除联动清理
//! 运行：cargo test

use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use teacell_ai_media_manager_lib::db::assets::AssetFilter;
use teacell_ai_media_manager_lib::db::{self, assets, export, export_items};
use teacell_ai_media_manager_lib::error::AppResult;
use teacell_ai_media_manager_lib::services::{export_local, importer, thumbnail::ThumbnailService};
use teacell_ai_media_manager_lib::state::Database;

/// A4-01 统一入口：只复制、逐文件结果。mode 参数已从服务层移除，
/// 「移动到库外」在命令层被明确拒绝，不可能再走到这里。
fn run_export(
    dbm: &Arc<Database>,
    task_id: i64,
    ids: &[i64],
    dest: &std::path::Path,
    layout: &str,
) -> AppResult<export_local::ExportSummary> {
    let cancel = Arc::new(AtomicBool::new(false));
    let dest_dir = dest.to_string_lossy().into_owned();
    export_local::export_local(
        dbm,
        export_local::ExportRun {
            task_id,
            asset_ids: ids,
            dest_dir: &dest_dir,
            layout,
            cancel: &cancel,
        },
        |_| {},
        &mut |_| Ok(()),
    )
}
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
    let dest_string = dest.to_string_lossy().into_owned();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let task = export::create_task(
        &dbm.lock().unwrap(),
        "local",
        ids.len() as i64,
        Some(&dest.to_string_lossy()),
        None,
    )?;

    export_local::export_local(
        &dbm,
        export_local::ExportRun {
            task_id: task.id,
            asset_ids: &ids,
            dest_dir: &dest_string,
            layout: "flat",
            cancel: &cancel_flag,
        },
        |_| {},
        &mut |_| Ok(()),
    )?;

    let t = export::get_task(&dbm.lock().unwrap(), task.id)?;
    assert_eq!(t.status, "done");
    assert_eq!(t.done, 5);
    // A4-01：逐文件结果必须全部落成 copied，且源文件与库内记录不变
    let items = export_items::items(&dbm.lock().unwrap(), task.id)?;
    assert_eq!(items.len(), 5);
    assert!(items
        .iter()
        .all(|item| item.state == export_items::STATE_COPIED));
    for a in &page.items {
        let copied = dest.join(&a.file_name);
        assert!(copied.exists());
        // 文件完整：尺寸一致
        let src_size = std::fs::metadata(&a.file_path)?.len();
        assert_eq!(std::fs::metadata(&copied)?.len(), src_size);
        // 复制导出不移动：原件、原路径仍在
        assert!(
            std::path::Path::new(&a.file_path).exists(),
            "复制导出后库内原文件必须仍在: {}",
            a.file_path
        );
        let current = assets::get(&dbm.lock().unwrap(), a.id)?;
        assert_eq!(current.file_path, a.file_path, "复制导出不得改动库内路径");
        assert_eq!(current.file_name, a.file_name, "复制导出不得改动文件名");
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

// ⑥ A4-01：导出统一为复制——库内原记录、原路径、原文件全部保留
#[test]
fn a4_01_export_keeps_library_record_and_source() -> AppResult<()> {
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
    let old_tags: Vec<String> = page.items[0].tags.iter().map(|t| t.name.clone()).collect();
    assert!(std::path::Path::new(&old_path).exists(), "原文件应存在");

    let dest = tmp.path().join("out");
    let task = export::create_task(
        &dbm.lock().unwrap(),
        "local",
        1,
        Some(&dest.to_string_lossy()),
        None,
    )?;
    let summary = run_export(&dbm, task.id, &[id], &dest, "flat")?;
    assert_eq!(summary.copied, 1);
    assert_eq!(summary.failed, 0);
    assert!(!summary.cancelled);

    // 目标已落盘
    assert!(dest.join(&old_name).exists(), "目标文件应存在");
    // 库内记录与原件原封不动
    let asset = assets::get(&dbm.lock().unwrap(), id)?;
    assert_eq!(asset.file_path, old_path, "复制导出不得改动库内路径");
    assert_eq!(asset.file_name, old_name, "复制导出不得改动文件名");
    assert_eq!(
        asset
            .tags
            .iter()
            .map(|t| t.name.clone())
            .collect::<Vec<_>>(),
        old_tags,
        "复制导出不得改动标签"
    );
    assert!(
        std::path::Path::new(&old_path).exists(),
        "复制导出后源文件必须仍在: {old_path}"
    );
    Ok(())
}

// ⑦ A4-01：目标目录已有同名文件 → 新目标自动加 (1)，库内记录不变
#[test]
fn a4_01_export_same_name_suffix_keeps_record() -> AppResult<()> {
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
    let orig_path = page.items[0].file_path.clone();

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
    let summary = run_export(&dbm, task.id, &[id], &dest, "flat")?;
    assert_eq!(summary.copied, 1);

    // 原有同名目标未被覆盖，新文件带 (1) 后缀
    let stem = std::path::Path::new(&orig_name)
        .file_stem()
        .unwrap()
        .to_string_lossy();
    let ext = std::path::Path::new(&orig_name)
        .extension()
        .unwrap()
        .to_string_lossy();
    let expected = format!("{stem}(1).{ext}");
    assert!(dest.join(&expected).exists(), "应写到 {expected}");
    // 库内记录仍指向原文件
    let asset = assets::get(&dbm.lock().unwrap(), id)?;
    assert_eq!(asset.file_name, orig_name, "库内文件名不得被导出改写");
    assert_eq!(asset.file_path, orig_path, "库内路径不得被导出改写");
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

// ⑨ B06b / A4-01：导出同名冲突超限 → 逐文件失败（不覆盖、不静默），任务落 failed
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
    // A4-01：单文件失败不再中断整个任务，而是落一条逐文件失败结果
    let summary = run_export(&dbm, task.id, &[id], &dest, "flat")?;
    assert_eq!(summary.copied, 0, "不应有任何文件被写出");
    assert_eq!(summary.failed, 1);

    let items = export_items::items(&dbm.lock().unwrap(), task.id)?;
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].state, export_items::STATE_FAILED);
    assert_eq!(
        items[0].error_code.as_deref(),
        Some(export_items::CODE_TARGET_EXISTS),
        "同名耗尽应归因为目标已存在"
    );
    let message = items[0].error_message.clone().unwrap_or_default();
    assert!(
        message.contains("同名文件过多"),
        "错误信息应含'同名文件过多': {message}"
    );
    // 原有同名占位未被覆盖，库内记录不变
    assert_eq!(
        std::fs::metadata(dest.join(&orig_name))?.len(),
        0,
        "占位文件不得被覆盖写入"
    );
    let asset = assets::get(&dbm.lock().unwrap(), id)?;
    assert_eq!(asset.file_name, orig_name);
    assert_eq!(asset.file_path, page.items[0].file_path);

    // 全部失败 → 任务状态 failed（不是 done、也不是静默成功）
    let t = export::get_task(&dbm.lock().unwrap(), task.id)?;
    assert_eq!(t.status, "failed");
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
