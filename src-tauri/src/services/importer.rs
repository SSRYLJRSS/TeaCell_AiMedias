//! 入库管线（架构 §4.1）：
//! 扫描（目录递归）→ 路径/hash 去重 → 元数据（图片读头尺寸；视频 ffprobe 尽力而为）
//! → 事务写库（每 500 条 commit，FTS 触发器同步）→ 占位缩略图 → 进度回调。
//! 进度通过回调上报，command 层包成 app.emit 事件（服务与 IPC 解耦，可单测）。
//!
//! B01 重构：阶段②拆为②a（锁外 rayon 并行处理：复制+元数据提取）+ ②b（短锁批量写库）。
//! 慢 IO（fs::copy / ffprobe / EXIF）不再持库锁，导入期间 list/get 等查询不阻塞。

use rayon::prelude::*;
use rusqlite::Connection;
use serde::Serialize;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use walkdir::WalkDir;

use super::thumbnail::ThumbnailService;
use super::{dedup, exif_meta, imaging, thumbnail, video};
use crate::db::assets::{self, ImportResult};
use crate::error::{AppError, AppResult};
use crate::state::Database;
use crate::utils::{mime, path};

/// 入库阶段（阶段 1 契约，见《入库标签与素材库改造开发指导书》§4.4）：
/// 后端只发阶段进度；前端按权重计算整体展示进度。不新增 committing 阶段（写库在 processing 内部）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportPhase {
    Queued,
    Scanning,
    Checking,
    Hashing,
    Processing,
    Previewing,
    Review,
    Cancelled,
    Failed,
    Done,
}

impl ImportPhase {
    pub fn as_str(&self) -> &'static str {
        match self {
            ImportPhase::Queued => "queued",
            ImportPhase::Scanning => "scanning",
            ImportPhase::Checking => "checking",
            ImportPhase::Hashing => "hashing",
            ImportPhase::Processing => "processing",
            ImportPhase::Previewing => "previewing",
            ImportPhase::Review => "review",
            ImportPhase::Cancelled => "cancelled",
            ImportPhase::Failed => "failed",
            ImportPhase::Done => "done",
        }
    }
}

/// 入库进度事件。task_id 用于防止旧任务事件污染新任务（前端监听 import://progress）。
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportProgress {
    pub task_id: String,
    /// queued|scanning|hashing|processing|previewing|done
    pub phase: String,
    pub phase_current: i64,
    /// 未知时前端必须显示不确定进度（不伪造百分比）
    pub phase_total: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub imported: i64,
    pub duplicates: i64,
    pub failed: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ImportProgress {
    fn new(task_id: &str, phase: ImportPhase) -> Self {
        ImportProgress {
            task_id: task_id.to_string(),
            phase: phase.as_str().to_string(),
            phase_current: 0,
            phase_total: None,
            file: None,
            imported: 0,
            duplicates: 0,
            failed: 0,
            message: None,
        }
    }
}

/// 任务 ID 生成：时间戳 + 进程内自增序号，保证同一次运行内唯一、可区分新旧任务。
fn next_task_id() -> String {
    static TASK_SEQ: AtomicI64 = AtomicI64::new(0);
    let seq = TASK_SEQ.fetch_add(1, Ordering::Relaxed);
    format!("import-{}-{}", chrono::Utc::now().timestamp_millis(), seq)
}

/// 展开输入路径为候选文件列表（目录递归 + 类型过滤）。
/// on_scan 每发现一个候选文件上报一次当前计数（供 scanning 阶段进度展示）。
/// 无法读取的路径进入 warnings，不再由 flatten() 静默丢弃。
fn collect_files_with_cancel<F: Fn(i64) + Sync>(
    paths: &[String],
    cancel: &AtomicBool,
    on_scan: F,
) -> AppResult<(Vec<PathBuf>, Vec<String>)> {
    let mut out = Vec::new();
    let mut warnings = Vec::new();
    for p in paths {
        if cancel.load(Ordering::Relaxed) {
            return Err(AppError::cancelled("文件检查已取消"));
        }
        let pb = PathBuf::from(p);
        if pb.is_dir() {
            for entry in WalkDir::new(&pb).follow_links(false) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(AppError::cancelled("文件检查已取消"));
                }
                match entry {
                    Ok(e) => {
                        if e.file_type().is_file() && is_supported(e.path()) {
                            if path::encode_native_path(e.path()).is_err() {
                                warnings.push(format!(
                                    "{}: 文件名不是有效 UTF-8，首版暂不支持无损入库",
                                    e.path().display()
                                ));
                                continue;
                            }
                            out.push(e.path().to_path_buf());
                            on_scan(out.len() as i64);
                        }
                    }
                    Err(e) => {
                        let path = e
                            .path()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| pb.display().to_string());
                        warnings.push(format!("{path}: {e}"));
                    }
                }
            }
        } else if pb.is_file() && is_supported(&pb) {
            out.push(pb);
            on_scan(out.len() as i64);
        } else {
            warnings.push(format!("{}: 路径不存在或不受支持", pb.display()));
        }
    }
    Ok((out, warnings))
}

#[cfg(test)]
fn collect_files(paths: &[String], on_scan: impl Fn(i64) + Sync) -> (Vec<PathBuf>, Vec<String>) {
    let cancel = AtomicBool::new(false);
    collect_files_with_cancel(paths, &cancel, on_scan).expect("未取消的文件扫描不应返回取消错误")
}

fn is_supported(p: &Path) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .and_then(mime::asset_type_from_ext)
        .is_some()
}

/// 入库选项（R-32 总库/分库托管）
#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// 总库根目录；空 = 原位索引模式
    pub library_root: Option<String>,
    /// 分库名称（总库下新建子文件夹）
    pub collection: Option<String>,
    /// 批量改名模板：{分库} {原名} {日期} {序号} / {序号:N} 补零；None = 不改名
    pub rename_pattern: Option<String>,
}

/// 渲染改名模板（不含扩展名）；非法字符转下划线，空结果回退原名
fn render_name(
    template: &str,
    collection: &str,
    orig_stem: &str,
    mtime_ms: i64,
    seq: usize,
) -> String {
    let date = {
        let secs = mtime_ms / 1000;
        // 本地时区日期（符合用户直觉）
        let dt = chrono::DateTime::from_timestamp(secs, 0)
            .map(|d| d.with_timezone(&chrono::Local))
            .unwrap_or_default();
        dt.format("%Y%m%d").to_string()
    };
    let mut out = template.to_string();
    // {序号:N} 补零格式先于 {序号} 处理（中文占位符多字节，按完整 token 截取）
    while let Some(start) = out.find("{序号:") {
        let rest = out[start..].to_string();
        let Some(rel_end) = rest.find('}') else { break };
        let inner = &rest["{序号:".len()..rel_end];
        let width: usize = inner.parse().unwrap_or(3);
        out = out.replacen(&rest[..rel_end + 1], &format!("{seq:0width$}"), 1);
    }
    out = out
        .replace("{分库}", collection)
        .replace("{原名}", orig_stem)
        .replace("{日期}", &date)
        .replace("{序号}", &seq.to_string());
    // 文件名非法字符净化（含路径分隔符，杜绝穿越）
    let sanitized: String = out
        .chars()
        .map(|c| if "<>:\"/\\|?*".contains(c) { '_' } else { c })
        .collect();
    let sanitized = sanitized
        .replace("..", "_")
        .trim()
        .trim_matches('.')
        .to_string();
    if sanitized.is_empty() {
        orig_stem.to_string()
    } else {
        sanitized
    }
}

/// 校验分库名：禁止路径穿越/分隔符（PRD R-32）
fn validate_collection(name: &str) -> AppResult<()> {
    let n = name.trim();
    if n.is_empty() || n.contains("..") || n.contains('/') || n.contains('\\') || n.contains(':') {
        return Err(AppError::msg("非法分库名称"));
    }
    Ok(())
}

/// 原子创建复制：create_new 保证目标文件此前不存在。
/// 并行 stage 同名文件时各线程依次尝试候选名，谁先创建成功谁占用，杜绝互相覆盖
fn copy_create_new(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut src = std::fs::File::open(from)?;
    let mut dst = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(to)?;
    std::io::copy(&mut src, &mut dst)?;
    Ok(())
}

/// 托管模式：复制源文件到 总库/分库/（可选批量改名），返回入库用路径
/// B06a：同名冲突循环耗尽（999）时报错而非静默覆盖已有文件
fn stage_file(file: &Path, opts: &ImportOptions, seq: usize) -> AppResult<PathBuf> {
    let root = opts.library_root.as_deref().unwrap_or_default().trim();
    if root.is_empty() {
        return Ok(file.to_path_buf());
    }
    let collection = opts.collection.as_deref().unwrap_or_default().trim();
    if !collection.is_empty() {
        validate_collection(collection)?;
    }
    let dest_dir = if collection.is_empty() {
        PathBuf::from(root)
    } else {
        PathBuf::from(root).join(collection)
    };
    // 目标路径将写入 SQLite/回传前端，不接受不可无损表示的路径。
    path::encode_native_path(&dest_dir)?;
    std::fs::create_dir_all(&dest_dir)?;

    let ext = file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or_default();
    let base_name = if let Some(tpl) = &opts.rename_pattern {
        let orig_stem = file
            .file_stem()
            .and_then(|stem| stem.to_str())
            .ok_or_else(|| AppError::unsupported("原文件名不是有效 UTF-8，无法安全改名"))?;
        let mtime = file
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        format!(
            "{}.{ext}",
            render_name(tpl, collection, orig_stem, mtime, seq)
        )
    } else {
        file.file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| AppError::unsupported("原文件名不是有效 UTF-8，无法安全入库"))?
            .to_owned()
    };
    // 同名冲突加 (n) 后缀：create_new 原子创建，②a 并行 stage 的多个同名文件
    // 各自依次尝试候选名，先创建成功者占用——不再依赖 exists() 检查后复制（有竞态，
    // 两线程会同时通过检查互相覆盖 → 数据丢失）
    let stem = Path::new(&base_name)
        .file_stem()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned();
    for i in 0..1000 {
        let dest = if i == 0 {
            dest_dir.join(&base_name)
        } else {
            dest_dir.join(format!("{stem}({i}).{ext}"))
        };
        match copy_create_new(file, &dest) {
            Ok(()) => return Ok(dest),
            // 该名字已被（并行任务或磁盘已有文件）占用，尝试下一个候选
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    // B06a：同名冲突耗尽时报错，不再回退覆盖已有文件
    Err(AppError::msg(format!("同名文件过多: {base_name}")))
}

// ── B01：元数据提取与回写（拆自原 import_one） ──

/// 锁外提取的元数据（image_dimensions + EXIF + ffprobe）
#[derive(Default)]
struct AssetMeta {
    width: Option<i64>,
    height: Option<i64>,
    duration_ms: Option<i64>,
    video_codec: Option<String>,
    audio_codec: Option<String>,
    // 指导书 §7.3/§7.4：视频结构化字段（V12 迁移新增列）
    container_format: Option<String>,
    video_profile: Option<String>,
    pixel_format: Option<String>,
    frame_rate: Option<f64>,
    rotation: Option<i64>,
    media_metadata_json: Option<String>,
    // GPS 定位（带符号十进制度，北纬东经为正）与视频拍摄时间（V18 列）
    latitude: Option<f64>,
    longitude: Option<f64>,
    /// 仅视频：ffprobe creation_time → epoch 毫秒（图片走 exif.taken_at）
    taken_at: Option<i64>,
    exif: Option<exif_meta::ExifData>,
}

/// B01：从已复制文件提取元数据（锁外，②a 阶段调用）
fn extract_meta(file: &Path, mime_type: &str) -> Option<AssetMeta> {
    let mut meta = AssetMeta::default();
    let mut has_any = false;
    if mime_type.starts_with("image/") {
        if let Ok((w, h)) = image::image_dimensions(file) {
            meta.width = Some(w as i64);
            meta.height = Some(h as i64);
            has_any = true;
        } else if crate::utils::mime::is_raw_ext(
            file.extension().and_then(|e| e.to_str()).unwrap_or(""),
        ) {
            // W1-4：image crate 不支持 RAW（RW2 等）→ rawler 只读宽高。
            // 205 张 RAW 宽高 NULL 会拖垮分辨率分面/搜索/信息面板。
            if let Some((w, h)) = crate::services::imaging::probe_dimensions(file) {
                meta.width = Some(w as i64);
                meta.height = Some(h as i64);
                has_any = true;
            }
        }
        let ex = exif_meta::extract(file);
        if ex.camera.is_some()
            || ex.taken_at.is_some()
            || ex.aperture.is_some()
            || ex.latitude.is_some()
        {
            meta.exif = Some(ex);
            has_any = true;
        }
    } else if mime_type.starts_with("video/") {
        if let Ok(vm) = video::probe(file) {
            meta.width = vm.width;
            meta.height = vm.height;
            meta.duration_ms = vm.duration_ms;
            meta.video_codec = vm.video_codec;
            meta.audio_codec = vm.audio_codec;
            meta.container_format = vm.container_format;
            meta.video_profile = vm.video_profile;
            meta.pixel_format = vm.pixel_format;
            meta.frame_rate = vm.frame_rate;
            meta.rotation = vm.rotation;
            meta.media_metadata_json = vm.raw_json;
            // GPS 定位与拍摄时间（format.tags 解析，尽力而为）
            meta.latitude = vm.latitude;
            meta.longitude = vm.longitude;
            meta.taken_at = vm.taken_at;
            has_any = true;
        }
    }
    if has_any {
        Some(meta)
    } else {
        None
    }
}

/// B01：元数据回写库（②b 阶段，事务内调用）
fn write_meta(
    conn: &Connection,
    id: i64,
    meta: &Option<AssetMeta>,
    mime_type: &str,
) -> AppResult<()> {
    let Some(m) = meta else {
        return Ok(());
    };
    if mime_type.starts_with("image/") {
        if let (Some(w), Some(h)) = (m.width, m.height) {
            conn.execute(
                "UPDATE assets SET width = ?1, height = ?2 WHERE id = ?3",
                rusqlite::params![w, h, id],
            )?;
        }
        if let Some(ex) = &m.exif {
            assets::set_exif(
                conn,
                id,
                &assets::ExifPatch {
                    camera: ex.camera.as_deref(),
                    lens: ex.lens.as_deref(),
                    iso: ex.iso,
                    aperture: ex.aperture,
                    shutter: ex.shutter.as_deref(),
                    focal: ex.focal,
                    taken_at: ex.taken_at,
                    latitude: ex.latitude,
                    longitude: ex.longitude,
                },
            )?;
        }
    } else if mime_type.starts_with("video/") {
        conn.execute(
            "UPDATE assets SET width=?1, height=?2, duration_ms=?3, video_codec=?4, audio_codec=?5,
                            container_format=?6, video_profile=?7, pixel_format=?8, frame_rate=?9,
                            rotation=?10, media_metadata_json=?11,
                            latitude=COALESCE(latitude, ?12), longitude=COALESCE(longitude, ?13), taken_at=COALESCE(taken_at, ?14)
             WHERE id=?15",
            rusqlite::params![
                m.width,
                m.height,
                m.duration_ms,
                m.video_codec,
                m.audio_codec,
                m.container_format,
                m.video_profile,
                m.pixel_format,
                m.frame_rate,
                m.rotation,
                m.media_metadata_json,
                m.latitude,
                m.longitude,
                m.taken_at,
                id
            ],
        )?;
    }
    Ok(())
}

// ── B01：②a 并行处理结果 ──

/// 锁外处理完成的文件（已复制 + 已提取元数据）
struct Processed {
    staged: PathBuf,
    hash: String,
    mime_type: String,
    norm: String,
    file_name: String,
    ext: String,
    file_size: i64,
    modified_at: i64,
    meta: Option<AssetMeta>,
}

/// 单文件处理结果：New 带完整元数据（大）、Duplicate/Failed 轻量。
/// large_enum_variant：设计上 Failed(String)/Duplicate 高频创建，Box 化反而多一次分配；
/// New 分支承载 90% 使用路径。集中豁免。
#[allow(clippy::large_enum_variant)]
enum ProcResult {
    New(Processed),
    Duplicate,
    Failed(String),
    Cancelled,
}

enum HashOutcome {
    Ready(String),
    Failed(String),
    Cancelled,
}

/// B01：单文件写库（insert + set_hash + write_meta），事务内调用
fn write_one(conn: &Connection, p: &Processed) -> AppResult<i64> {
    let id = assets::insert(
        conn,
        &p.norm,
        &p.file_name,
        &p.ext,
        p.file_size,
        &p.mime_type,
        p.modified_at,
    )?;
    assets::set_hash(conn, id, &p.hash)?;
    write_meta(conn, id, &p.meta, &p.mime_type)?;
    Ok(id)
}

/// 托管模式下，写库失败要清理已复制副本，避免磁盘文件和数据库记录长期不一致。
/// 原位索引模式的 src 就是用户原文件，调用方必须保证不会进入这里。
fn cleanup_staged_file(path: &Path, task_id: &str) {
    match std::fs::remove_file(path) {
        Ok(()) => tracing::warn!(
            operation = "import",
            task_id = %task_id,
            stage = "cleanup",
            file = %path.display(),
            "导入失败后已清理托管副本"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => tracing::warn!(
            operation = "import",
            task_id = %task_id,
            stage = "cleanup",
            file = %path.display(),
            error = %e,
            "导入失败后清理托管副本失败"
        ),
    }
}

/// 发送 done 阶段事件（含最终 imported/duplicates/failed 统计）。
fn emit_done<F: Fn(ImportProgress)>(
    task_id: &str,
    result: &ImportResult,
    message: Option<String>,
    progress: &F,
) {
    let mut done = ImportProgress::new(task_id, ImportPhase::Done);
    done.phase_current = result.imported;
    done.phase_total = Some(result.imported);
    done.imported = result.imported;
    done.duplicates = result.duplicates;
    done.failed = result.failed;
    done.message = message;
    progress(done);
}

/// 三段式入库管线（B01 重构）：
/// ① 并行算 hash（IO/CPU 密集，不占库锁）
/// ②a 锁外并行处理（rayon）：precheck 短锁 → 托管复制 → 元数据提取
/// ②b 短锁批量写库（单事务，纯 INSERT/UPDATE，毫秒级）
/// ③ 并行生成占位图（B14：循环内检查 cancel），完成后统一回写路径
pub fn import_paths<F: Fn(ImportProgress) + Sync>(
    db: &Database,
    thumbs: &ThumbnailService,
    paths: &[String],
    opts: &ImportOptions,
    cancel: &AtomicBool,
    progress: F,
) -> AppResult<ImportResult> {
    import_paths_with_task_id(db, thumbs, paths, opts, cancel, next_task_id(), progress)
}

/// 导入统一要求用户已配置、存在且可写的托管总库位置；探测文件是随机临时文件并立即清理。
pub fn validate_library_root(root: &str) -> AppResult<PathBuf> {
    let raw = root.trim();
    if raw.is_empty() {
        return Err(AppError::coded(
            crate::error::AppErrorCode::LibraryRootRequired,
            "请先在设置中配置并保存总库位置",
        ));
    }
    let path = PathBuf::from(raw);
    let metadata = std::fs::metadata(&path).map_err(|_| {
        AppError::coded(
            crate::error::AppErrorCode::InvalidLibraryRoot,
            "总库位置不存在或无法访问，请在设置中重新选择可用目录",
        )
    })?;
    if !metadata.is_dir() {
        return Err(AppError::coded(
            crate::error::AppErrorCode::InvalidLibraryRoot,
            "总库位置必须是一个已存在的目录",
        ));
    }
    static PROBE_SEQ: AtomicI64 = AtomicI64::new(0);
    let probe = path.join(format!(
        ".bagertea-write-check-{}-{}",
        std::process::id(),
        PROBE_SEQ.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .map_err(|_| {
            AppError::coded(
                crate::error::AppErrorCode::InvalidLibraryRoot,
                "总库目录不可写，请检查目录权限或磁盘状态",
            )
        })?;
    let write_result = file.write_all(b"probe");
    drop(file);
    let cleanup_result = std::fs::remove_file(&probe);
    if write_result.is_err() || cleanup_result.is_err() {
        return Err(AppError::coded(
            crate::error::AppErrorCode::InvalidLibraryRoot,
            "无法验证总库目录写入权限或清理临时检查文件，请检查磁盘状态",
        ));
    }
    Ok(path)
}

/// 使用已有的检查任务 ID 继续入库，让检查、前置校验和入库阶段共用左侧进度栏。
pub fn import_paths_with_task_id<F: Fn(ImportProgress) + Sync>(
    db: &Database,
    thumbs: &ThumbnailService,
    paths: &[String],
    opts: &ImportOptions,
    cancel: &AtomicBool,
    task_id: String,
    progress: F,
) -> AppResult<ImportResult> {
    import_paths_inner(
        db,
        thumbs,
        ImportInput::Paths(paths),
        opts,
        cancel,
        task_id,
        progress,
    )
}

/// 正式入库前由 command 完成了后端预检时，直接复用其候选清单，避免再次展开目录/文件列表。
pub fn import_preflight_plan_with_task_id<F: Fn(ImportProgress) + Sync>(
    db: &Database,
    thumbs: &ThumbnailService,
    plan: ImportPlan,
    opts: &ImportOptions,
    cancel: &AtomicBool,
    task_id: String,
    progress: F,
) -> AppResult<ImportResult> {
    import_paths_inner(
        db,
        thumbs,
        ImportInput::PreflightPlan(plan),
        opts,
        cancel,
        task_id,
        progress,
    )
}

enum ImportInput<'a> {
    Paths(&'a [String]),
    PreflightPlan(ImportPlan),
}

fn import_paths_inner<F: Fn(ImportProgress) + Sync>(
    db: &Database,
    thumbs: &ThumbnailService,
    input: ImportInput<'_>,
    opts: &ImportOptions,
    cancel: &AtomicBool,
    task_id: String,
    progress: F,
) -> AppResult<ImportResult> {
    let library_root = opts.library_root.as_deref().ok_or_else(|| {
        AppError::coded(
            crate::error::AppErrorCode::LibraryRootRequired,
            "请先在设置中配置并保存总库位置",
        )
    })?;
    validate_library_root(library_root)?;
    // 快速失败：分库名非法直接拒绝整个批次（PRD R-32 禁路径穿越）
    if let Some(c) = &opts.collection {
        if !c.trim().is_empty() {
            validate_collection(c)?;
        }
    }
    // 阶段 1 契约：后端只发阶段进度，task_id 隔离新旧任务事件。
    let started_at = std::time::Instant::now();
    tracing::info!(
        operation = "import",
        task_id = %task_id,
        path_count = match &input {
            ImportInput::Paths(paths) => paths.len(),
            ImportInput::PreflightPlan(plan) => plan.items.len(),
        },
        managed = opts.library_root.is_some(),
        "导入任务开始"
    );
    // queued：任务已接受
    let mut queued = ImportProgress::new(&task_id, ImportPhase::Queued);
    queued.message = Some("准备入库".into());
    progress(queued);
    // scanning：目录递归收集候选文件（phaseTotal 未知 → 前端显示不确定进度）
    let scan_started_at = std::time::Instant::now();
    let reused_preflight = matches!(&input, ImportInput::PreflightPlan(_));
    let (files, scan_warnings) = match input {
        ImportInput::PreflightPlan(plan) => {
            let files: Vec<PathBuf> = plan
                .items
                .into_iter()
                .map(|item| PathBuf::from(item.path))
                .collect();
            tracing::debug!(
                operation = "import",
                task_id = %task_id,
                candidate_count = files.len(),
                "复用后端预检候选清单，跳过重复路径收集"
            );
            (files, plan.warnings)
        }
        ImportInput::Paths(paths) => collect_files_with_cancel(paths, cancel, |n| {
            let mut sc = ImportProgress::new(&task_id, ImportPhase::Scanning);
            sc.phase_current = n;
            sc.phase_total = None;
            sc.message = Some("正在扫描目录".into());
            progress(sc);
        })?,
    };
    tracing::info!(
        operation = "import",
        task_id = %task_id,
        stage = if reused_preflight { "candidate_reuse" } else { "candidate_scan" },
        candidate_count = files.len(),
        warning_count = scan_warnings.len(),
        duration_ms = scan_started_at.elapsed().as_millis() as u64,
        "入库候选清单准备完成"
    );
    for warning in &scan_warnings {
        tracing::warn!(
            operation = "import",
            task_id = %task_id,
            stage = "scan",
            error = %warning,
            "导入扫描警告"
        );
    }
    let total = files.len() as i64;
    let mut result = ImportResult {
        imported: 0,
        failed: 0,
        duplicates: 0,
        errors: Vec::new(),
        warnings: scan_warnings,
    };
    if files.is_empty() {
        tracing::info!(
            operation = "import",
            task_id = %task_id,
            warnings = result.warnings.len(),
            duration_ms = started_at.elapsed().as_millis() as u64,
            "导入结束：未发现可入库文件"
        );
        let mut done = ImportProgress::new(&task_id, ImportPhase::Done);
        done.message = Some("未发现可入库文件".into());
        progress(done);
        return Ok(result);
    }

    // ① 并行 hash（hashing 阶段；取消在②③阶段间仍生效，此处每文件上报进度）
    let hash_done = AtomicI64::new(0);
    let hash_started_at = std::time::Instant::now();
    let hashes: Vec<HashOutcome> = files
        .par_iter()
        .map(|f| {
            if cancel.load(Ordering::Relaxed) {
                return HashOutcome::Cancelled;
            }
            let hash = match crate::utils::hash::sha256_16(f) {
                Ok(hash) => HashOutcome::Ready(hash),
                Err(e) => HashOutcome::Failed(format!("{}: {e}", f.display())),
            };
            let n = hash_done.fetch_add(1, Ordering::Relaxed) + 1;
            let mut he = ImportProgress::new(&task_id, ImportPhase::Hashing);
            he.phase_current = n;
            he.phase_total = Some(total);
            he.file = Some(
                f.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
            progress(he);
            hash
        })
        .collect();
    tracing::info!(
        operation = "import",
        task_id = %task_id,
        stage = "hash",
        candidate_count = total,
        duration_ms = hash_started_at.elapsed().as_millis() as u64,
        "入库文件哈希完成"
    );

    // ②a：锁外并行处理（复制 + 元数据提取），precheck 用短锁单次查询
    let proc_done = AtomicI64::new(0);
    let process_started_at = std::time::Instant::now();
    let processed: Vec<ProcResult> = files
        .par_iter()
        .enumerate()
        .map(|(idx, file)| {
            // B01：cancel 在②a 生效
            if cancel.load(Ordering::Relaxed) {
                return ProcResult::Cancelled;
            }
            // processing 阶段进度（处理是慢阶段，②b 写库在 processing 内部，不单独计阶段）
            let n = proc_done.fetch_add(1, Ordering::Relaxed) + 1;
            let mut pe = ImportProgress::new(&task_id, ImportPhase::Processing);
            pe.phase_current = n;
            pe.phase_total = Some(total);
            pe.file = Some(
                file.file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned(),
            );
            progress(pe);
            let hash = match &hashes[idx] {
                HashOutcome::Ready(hash) => hash.clone(),
                HashOutcome::Failed(error) => {
                    tracing::warn!(
                        operation = "import",
                        task_id = %task_id,
                        stage = "hash",
                        file = %file.display(),
                        error = %error,
                        "导入文件哈希失败"
                    );
                    return ProcResult::Failed(error.clone());
                }
                HashOutcome::Cancelled => return ProcResult::Cancelled,
            };
            // precheck 用短锁（单次查询，微秒级）；TOCTOU 由 UNIQUE 约束兜底
            // 注意：precheck 返回 true=新文件，false=重复
            let is_new = {
                let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"));
                match conn {
                    Ok(c) => match precheck(&c, file, &hash) {
                        Ok(is_new) => is_new,
                        Err(e) => {
                            tracing::warn!(
                                operation = "import",
                                task_id = %task_id,
                                stage = "precheck",
                                file = %file.display(),
                                error = %e,
                                "导入预检查失败"
                            );
                            return ProcResult::Failed(format!("{}: {e}", file.display()));
                        }
                    },
                    Err(_) => {
                        tracing::error!(
                            operation = "import",
                            task_id = %task_id,
                            stage = "precheck",
                            file = %file.display(),
                            "导入预检查数据库锁中毒"
                        );
                        return ProcResult::Failed("数据库锁中毒".into());
                    }
                }
            };
            if !is_new {
                return ProcResult::Duplicate;
            }
            // 三端复核 X-18：非 UTF-8 源文件名不静默有损入库。首版明确拒绝并保留原文件
            //（导入只复制不移动，原文件本就不动），后续再做无损 OsString 协议。
            // 在暂存复制前判定，避免产生无法正确记录路径的孤儿副本。
            if let Err(e) = path::encode_native_path(file) {
                tracing::warn!(
                    operation = "import",
                    task_id = %task_id,
                    stage = "encode_name",
                    file = %file.display(),
                    error = %e,
                    "跳过：文件名非 UTF-8，首版暂不支持无损入库"
                );
                return ProcResult::Failed(format!("{}: {e}", file.display()));
            }
            // 锁外：托管复制
            let staged = match stage_file(file, opts, idx + 1) {
                Ok(p) => p,
                Err(e) => {
                    tracing::warn!(
                        operation = "import",
                        task_id = %task_id,
                        stage = "stage",
                        file = %file.display(),
                        error = %e,
                        "导入文件暂存失败"
                    );
                    return ProcResult::Failed(format!("{}: {e}", file.display()));
                }
            };
            // 锁外：计算 DB 插入所需的路径/文件信息
            let ext = staged
                .extension()
                .and_then(|e| e.to_str())
                .unwrap_or_default()
                .to_ascii_lowercase();
            let mime_type = mime::mime_from_ext(&ext).unwrap_or_default();
            // 锁外：元数据提取（image_dimensions / EXIF / ffprobe）
            let meta = extract_meta(&staged, &mime_type);
            let staged_path = match path::encode_native_path(&staged) {
                Ok(path) => path,
                Err(e) => {
                    return ProcResult::Failed(format!("{}: {e}", staged.display()));
                }
            };
            let norm = path::normalize_path(&staged_path);
            let file_name = match staged.file_name().and_then(|name| name.to_str()) {
                Some(name) => name.to_owned(),
                None => {
                    return ProcResult::Failed(format!(
                        "{}: 暂存文件名不是有效 UTF-8，无法安全入库",
                        staged.display()
                    ));
                }
            };
            let file_size = staged.metadata().map(|m| m.len() as i64).unwrap_or(0);
            let modified_at = staged
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_millis() as i64)
                .unwrap_or(0);
            ProcResult::New(Processed {
                staged,
                hash,
                mime_type,
                norm,
                file_name,
                ext,
                file_size,
                modified_at,
                meta,
            })
        })
        .collect();
    tracing::info!(
        operation = "import",
        task_id = %task_id,
        stage = "process_and_copy",
        candidate_count = total,
        duration_ms = process_started_at.elapsed().as_millis() as u64,
        "导入文件处理与托管复制阶段完成"
    );

    // 统计②a结果（区分取消与真实失败）
    for r in &processed {
        match r {
            ProcResult::Duplicate => result.duplicates += 1,
            ProcResult::Cancelled => {}
            ProcResult::Failed(msg) => {
                result.failed += 1;
                result.errors.push(msg.clone());
                tracing::warn!(
                    operation = "import",
                    task_id = %task_id,
                    stage = "process",
                    error = %msg,
                    "导入单文件处理失败"
                );
            }
            ProcResult::New(_) => {}
        }
    }

    // ②b：短锁批量写库（单事务，纯 INSERT/UPDATE，毫秒级）
    let mut pending_thumbs: Vec<(i64, PathBuf, String)> = Vec::new();
    // 托管模式下 staged 是副本，重复跳过时需清理副本避免孤儿文件；原位模式 staged 即源文件，绝不能删
    let managed = !opts
        .library_root
        .as_deref()
        .unwrap_or_default()
        .trim()
        .is_empty();
    {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        let tx = conn.unchecked_transaction()?;
        for r in &processed {
            if let ProcResult::New(p) = r {
                // 二次查重：②a 的并行 precheck 互不可见，同批同 hash 的两条会双双通过；
                // 此处串行事务内逐条「查+写」，同批后到的与跨批并发（单写连接全串行）都能拦住
                match dedup::hash_exists(&tx, &p.hash) {
                    Ok(true) => {
                        result.duplicates += 1;
                        if managed {
                            // 罕见路径：单 syscall 级清理，不破坏「锁外慢 IO」纪律
                            cleanup_staged_file(&p.staged, &task_id);
                        }
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        result.failed += 1;
                        result.errors.push(format!("{}: {e}", p.staged.display()));
                        tracing::warn!(
                            operation = "import",
                            task_id = %task_id,
                            stage = "commit",
                            file = %p.staged.display(),
                            error = %e,
                            "导入二次查重失败"
                        );
                        continue;
                    }
                }
                // B01：UNIQUE 约束兜底 TOCTOU——单条失败不中断整批
                match write_one(&tx, p) {
                    Ok(id) => {
                        pending_thumbs.push((id, p.staged.clone(), p.mime_type.clone()));
                        result.imported += 1;
                    }
                    Err(e) => {
                        result.failed += 1;
                        result.errors.push(format!("{}: {e}", p.staged.display()));
                        tracing::warn!(
                            operation = "import",
                            task_id = %task_id,
                            stage = "commit",
                            file = %p.staged.display(),
                            error = %e,
                            "导入写库失败"
                        );
                        if managed {
                            cleanup_staged_file(&p.staged, &task_id);
                        }
                    }
                }
            }
        }
        if let Err(e) = tx.commit() {
            if managed {
                for (_, file, _) in &pending_thumbs {
                    cleanup_staged_file(file, &task_id);
                }
            }
            tracing::error!(
                operation = "import",
                task_id = %task_id,
                stage = "commit",
                error = %e,
                "导入批次事务提交失败"
            );
            return Err(e.into());
        }
    } // 释放库锁：占位图生成不阻塞素材库查询

    // B15：取消后已写库记录仍保留（部分导入语义）
    if cancel.load(Ordering::Relaxed) {
        result.errors.push(format!(
            "用户取消（已导入 {} 条，重复 {} 条）",
            result.imported, result.duplicates
        ));
        tracing::info!(
            operation = "import",
            task_id = %task_id,
            result = "cancelled",
            imported = result.imported,
            duplicates = result.duplicates,
            failed = result.failed,
            warnings = result.warnings.len(),
            duration_ms = started_at.elapsed().as_millis() as u64,
            "导入任务取消"
        );
        emit_done(
            &task_id,
            &result,
            Some("已取消，保留已导入记录".into()),
            &progress,
        );
        return Ok(result);
    }

    // ③ 并行生成占位图（B14：par_iter 内检查 cancel；永不失败，失败落通用占位图）
    // W5d：占位图是已解码像素的唯一搭车点，顺带收集 dHash 回写 assets.phash。
    let thumb_total = pending_thumbs.len() as i64;
    let thumb_done = AtomicI64::new(0);
    let placeholder_started_at = std::time::Instant::now();
    let paths_out: Vec<(i64, PathBuf, Option<u64>)> = pending_thumbs
        .par_iter()
        .map(|(id, file, mime_type)| {
            // B14：取消后未开始的任务快速跳过（返回空路径，后续跳过回写）
            if cancel.load(Ordering::Relaxed) {
                return (*id, PathBuf::new(), None);
            }
            let (p, phash) = thumbs.extract_placeholder(*id, file, mime_type);
            let n = thumb_done.fetch_add(1, Ordering::Relaxed) + 1;
            let mut pe = ImportProgress::new(&task_id, ImportPhase::Previewing);
            pe.phase_current = n;
            pe.phase_total = Some(thumb_total);
            pe.file = Some(format!(
                "缩略图 · {}",
                file.file_name().unwrap_or_default().to_string_lossy()
            ));
            pe.imported = result.imported;
            pe.duplicates = result.duplicates;
            pe.failed = result.failed;
            progress(pe);
            (*id, p, phash)
        })
        .collect();
    tracing::info!(
        operation = "import",
        task_id = %task_id,
        stage = "placeholder",
        candidate_count = thumb_total,
        duration_ms = placeholder_started_at.elapsed().as_millis() as u64,
        "导入占位图阶段完成"
    );

    // 统一回写占位图路径（一个事务；跳过取消项的空路径）
    {
        let conn = db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        let tx = conn.unchecked_transaction()?;
        for (id, p, phash) in &paths_out {
            if !p.as_os_str().is_empty() {
                let placeholder_path = path::encode_native_path(p)?;
                assets::set_placeholder_path(&tx, *id, &placeholder_path)?;
                if let Some(ph) = phash {
                    assets::set_phash(&tx, *id, *ph)?;
                }
            }
        }
        tx.commit()?;
    }

    // B15：阶段③取消提示（占位图未全部生成，但库记录已写）
    if cancel.load(Ordering::Relaxed) {
        result.errors.push(format!(
            "用户取消（已导入 {} 条，部分占位图待下次浏览时补生成）",
            result.imported
        ));
        tracing::info!(
            operation = "import",
            task_id = %task_id,
            result = "cancelled",
            imported = result.imported,
            duplicates = result.duplicates,
            failed = result.failed,
            warnings = result.warnings.len(),
            duration_ms = started_at.elapsed().as_millis() as u64,
            "导入任务取消：占位图未全部生成"
        );
        emit_done(
            &task_id,
            &result,
            Some("已取消，部分占位图待补".into()),
            &progress,
        );
        return Ok(result);
    }

    tracing::info!(
        operation = "import",
        task_id = %task_id,
        result = if result.failed == 0 { "done" } else { "partial" },
        total,
        imported = result.imported,
        duplicates = result.duplicates,
        failed = result.failed,
        warnings = result.warnings.len(),
        duration_ms = started_at.elapsed().as_millis() as u64,
        "导入任务完成"
    );
    emit_done(&task_id, &result, Some("入库完成".into()), &progress);
    Ok(result)
}

/// 重复判定（hash 已由并行阶段算好）：true = 新文件
fn precheck(conn: &Connection, file: &Path, file_hash: &str) -> AppResult<bool> {
    let native_path = path::encode_native_path(file)?;
    let norm = path::normalize_path(&native_path);
    if assets::find_by_path(conn, &norm)?.is_some() {
        return Ok(false);
    }
    if dedup::hash_exists(conn, file_hash)? {
        return Ok(false);
    }
    Ok(true)
}

/// 待入库清单项（两段式入库的左侧统计，PRD v2.4）
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportPreviewStatus {
    /// 已能生成入库缩略图，普通导入路径。
    Ready,
    /// 能生成缩略图，但属于 RAW 等特殊格式，后续高清/元数据/AI 能力可能受限。
    Limited,
    /// 连入库缩略图都无法生成，禁止进入正式入库。
    Unsupported,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPlanItem {
    pub path: String,
    pub kind: String, // image|video
    pub size: i64,
    pub preview_status: ImportPreviewStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_message: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportPlan {
    pub items: Vec<ImportPlanItem>,
    pub images: i64,
    pub videos: i64,
    pub total_size: i64,
    pub warnings: Vec<String>,
}

/// 扫描路径展开为待入库清单（不落库，仅统计）
pub fn inspect_paths(paths: &[String]) -> ImportPlan {
    let cancel = AtomicBool::new(false);
    inspect_paths_with_control(paths, next_task_id(), &cancel, |_| {})
        .expect("未取消的文件检查不应失败")
}

/// 带任务进度与取消检查的清单检查；只扫描用户所选路径，不写素材库。
pub fn inspect_paths_with_control<F: Fn(ImportProgress) + Sync>(
    paths: &[String],
    task_id: String,
    cancel: &AtomicBool,
    progress: F,
) -> AppResult<ImportPlan> {
    let mut queued = ImportProgress::new(&task_id, ImportPhase::Queued);
    queued.message = Some("正在检查所选文件".into());
    progress(queued);
    let scan_started_at = std::time::Instant::now();
    let scan = collect_files_with_cancel(paths, cancel, |n| {
        let mut scanning = ImportProgress::new(&task_id, ImportPhase::Scanning);
        scanning.phase_current = n;
        scanning.message = Some("正在扫描目录".into());
        progress(scanning);
    });
    let (files, warnings) = match scan {
        Ok(result) => result,
        Err(error) => {
            let mut cancelled = ImportProgress::new(&task_id, ImportPhase::Cancelled);
            cancelled.message = Some("文件检查已取消".into());
            progress(cancelled);
            return Err(error);
        }
    };
    tracing::info!(
        operation = "inspect_import",
        task_id = %task_id,
        stage = "candidate_scan",
        candidate_count = files.len(),
        warning_count = warnings.len(),
        duration_ms = scan_started_at.elapsed().as_millis() as u64,
        "待入库候选文件扫描完成"
    );
    for warning in &warnings {
        tracing::warn!(
            operation = "inspect_import",
            stage = "scan",
            error = %warning,
            "待入库扫描警告"
        );
    }
    let mut plan = ImportPlan {
        items: Vec::new(),
        images: 0,
        videos: 0,
        total_size: 0,
        warnings,
    };
    let total = files.len() as i64;
    let preview_started_at = std::time::Instant::now();
    for (index, f) in files.into_iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            let mut cancelled = ImportProgress::new(&task_id, ImportPhase::Cancelled);
            cancelled.message = Some("文件检查已取消".into());
            progress(cancelled);
            return Err(AppError::cancelled("文件检查已取消"));
        }
        let mut checking = ImportProgress::new(&task_id, ImportPhase::Checking);
        checking.phase_current = index as i64 + 1;
        checking.phase_total = Some(total);
        checking.file = f
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        checking.message = Some("正在检查文件与缩略图".into());
        progress(checking);
        let ext = f.extension().and_then(|e| e.to_str()).unwrap_or_default();
        let kind = mime::asset_type_from_ext(ext).unwrap_or("image");
        let size = f.metadata().map(|m| m.len() as i64).unwrap_or(0);
        let path_text = match path::encode_native_path(&f) {
            Ok(path) => path,
            Err(err) => {
                plan.warnings.push(format!("{}: {err}", f.display()));
                continue;
            }
        };
        if kind == "video" {
            plan.videos += 1;
        } else {
            plan.images += 1;
        }
        plan.total_size += size;
        let (preview_status, preview_message) = if kind == "image" {
            classify_import_preview(&f, ext)
        } else {
            (ImportPreviewStatus::Ready, None)
        };
        plan.items.push(ImportPlanItem {
            path: path_text,
            kind: kind.to_string(),
            size,
            preview_status,
            preview_message,
        });
    }
    tracing::info!(
        operation = "inspect_import",
        task_id = %task_id,
        stage = "preview_classification",
        candidate_count = total,
        duration_ms = preview_started_at.elapsed().as_millis() as u64,
        "待入库预览检查完成"
    );
    if cancel.load(Ordering::Relaxed) {
        let mut cancelled = ImportProgress::new(&task_id, ImportPhase::Cancelled);
        cancelled.message = Some("文件检查已取消".into());
        progress(cancelled);
        return Err(AppError::cancelled("文件检查已取消"));
    }
    let mut review = ImportProgress::new(&task_id, ImportPhase::Review);
    review.phase_current = total;
    review.phase_total = Some(total);
    review.message = Some(format!("检查完成，待确认入库（{} 项）", plan.items.len()));
    progress(review);
    Ok(plan)
}

/// 入库前只验证“能否生成快速缩略图”，不进入完整 RAW 显影。
/// 这样导入页可以在正式入库前明确拦截损坏/当前不支持的图片。
fn classify_import_preview(file: &Path, ext: &str) -> (ImportPreviewStatus, Option<String>) {
    let previewable = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        imaging::decode_thumb(file, thumbnail::PLACEHOLDER_SIZE).is_some()
    }))
    .unwrap_or(false);
    if !previewable {
        return (
            ImportPreviewStatus::Unsupported,
            Some("无法解析入库缩略图，当前不支持导入（也可能是文件损坏）".into()),
        );
    }
    if mime::is_raw_ext(ext) {
        return (
            ImportPreviewStatus::Limited,
            Some(
                "已解析缩略图，可以导入；但该 RAW 格式的高清预览、元数据或 AI 功能可能受限".into(),
            ),
        );
    }
    (ImportPreviewStatus::Ready, None)
}

/// 在正式导入前再次验证清单，防止直接调用 IPC 绕过前端的“不支持预览”拦截。
pub fn require_previewable(plan: &ImportPlan) -> AppResult<()> {
    let blocked: Vec<&str> = plan
        .items
        .iter()
        .filter(|item| item.preview_status == ImportPreviewStatus::Unsupported)
        .map(|item| item.path.as_str())
        .collect();
    if blocked.is_empty() {
        return Ok(());
    }

    let shown = blocked
        .iter()
        .take(5)
        .copied()
        .collect::<Vec<_>>()
        .join("、");
    let suffix = if blocked.len() > 5 {
        format!(" 等 {} 个文件", blocked.len())
    } else {
        String::new()
    };
    Err(AppError::unsupported(format!(
        "有 {} 个文件无法解析入库缩略图，请先从队列剔除：{}{}",
        blocked.len(),
        shown,
        suffix
    )))
}

/// 改名模板预览（前端 RenameBuilder 用）：与入库 stage 的 render_name 同源，
/// 消除前后端双实现 drift；日期无源文件 mtime，以当前时间示意
pub fn preview_rename(template: &str, collection: &str, orig_stem: &str, seq: usize) -> String {
    let now_ms = chrono::Utc::now().timestamp_millis();
    render_name(template, collection, orig_stem, now_ms, seq)
}

#[cfg(test)]
mod tests {
    use super::{
        collect_files, inspect_paths, precheck, render_name, validate_library_root,
        ImportPreviewStatus,
    };
    use crate::utils::path;
    use std::sync::atomic::AtomicBool;

    // 2026-07-27 12:00:00 UTC
    const MTIME: i64 = 1_785_225_600_000;

    #[test]
    fn library_root_must_exist_be_a_directory_and_be_writable() {
        let root = tempfile::tempdir().unwrap();
        let validated = validate_library_root(root.path().to_string_lossy().as_ref()).unwrap();
        assert_eq!(validated, root.path());
        assert_eq!(
            std::fs::read_dir(root.path()).unwrap().count(),
            0,
            "探测文件必须清理"
        );

        let file = root.path().join("not-a-directory");
        std::fs::write(&file, b"x").unwrap();
        let error = validate_library_root(file.to_string_lossy().as_ref()).unwrap_err();
        assert_eq!(error.code(), "INVALID_LIBRARY_ROOT");
        let error = validate_library_root(" ").unwrap_err();
        assert_eq!(error.code(), "LIBRARY_ROOT_REQUIRED");
    }

    #[test]
    fn import_inspection_observes_cancellation_and_emits_terminal_phase() {
        let root = tempfile::tempdir().unwrap();
        let cancel = AtomicBool::new(true);
        let phases = std::sync::Mutex::new(Vec::new());
        let result = super::inspect_paths_with_control(
            &[root.path().to_string_lossy().into_owned()],
            "inspect-cancel".into(),
            &cancel,
            |progress| phases.lock().unwrap().push(progress.phase),
        );
        assert_eq!(result.unwrap_err().code(), "CANCELLED");
        assert_eq!(*phases.lock().unwrap(), vec!["queued", "cancelled"]);
    }

    #[test]
    fn default_template() {
        assert_eq!(
            render_name("{分库}_{序号:3}", "旅行", "IMG_001", MTIME, 7),
            "旅行_007"
        );
    }

    #[test]
    fn keeps_original_name() {
        assert_eq!(
            render_name("{原名}", "旅行", "IMG_001", MTIME, 1),
            "IMG_001"
        );
    }

    /// 与实现同源的期望日期（本地时区，避免 CI/本机时区差异）
    fn expected_date() -> String {
        chrono::DateTime::from_timestamp(MTIME / 1000, 0)
            .map(|d| d.with_timezone(&chrono::Local))
            .unwrap_or_default()
            .format("%Y%m%d")
            .to_string()
    }

    #[test]
    fn date_and_plain_seq() {
        assert_eq!(
            render_name("{日期}_{序号}", "", "x", MTIME, 12),
            format!("{}_12", expected_date())
        );
    }

    #[test]
    fn combo_template() {
        assert_eq!(
            render_name("{分库}-{日期}-{原名}-{序号:2}", "宠物", "P1001", MTIME, 3),
            format!("宠物-{}-P1001-03", expected_date())
        );
    }

    #[test]
    fn illegal_chars_sanitized() {
        assert_eq!(render_name("a/b\\c:d*e", "", "orig", MTIME, 1), "a_b_c_d_e");
    }

    #[test]
    fn empty_result_falls_back_to_original() {
        assert_eq!(render_name("", "", "orig", MTIME, 1), "orig");
    }

    #[test]
    fn collect_files_reports_missing_paths_instead_of_silently_dropping() {
        let missing = std::env::temp_dir()
            .join(format!("bagertea_missing_{}", uuid::Uuid::new_v4()))
            .to_string_lossy()
            .into_owned();
        let (files, warnings) = collect_files(&[missing], |_| {});
        assert!(files.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("路径不存在或不受支持"));
    }

    // This test creates an invalid-UTF-8 filesystem entry; macOS rejects that path with EILSEQ.
    #[cfg(target_os = "linux")]
    #[test]
    fn collect_files_skips_non_utf8_names_and_reports_warning_before_import() {
        use std::os::unix::ffi::OsStrExt;

        let dir = tempfile::tempdir().unwrap();
        let bad_name = std::ffi::OsStr::from_bytes(b"bad-\xff.jpg");
        std::fs::write(dir.path().join(bad_name), b"not really a jpeg").unwrap();
        let root = path::encode_native_path(dir.path()).unwrap();

        let (files, warnings) = collect_files(std::slice::from_ref(&root), |_| {});
        assert!(files.is_empty());
        assert_eq!(warnings.len(), 1);
        assert!(warnings[0].contains("不是有效 UTF-8"));

        let plan = super::inspect_paths(&[root]);
        assert!(plan.items.is_empty());
        assert_eq!(plan.warnings.len(), 1);
        assert!(plan.warnings[0].contains("不是有效 UTF-8"));
    }

    #[test]
    fn precheck_database_error_is_not_a_duplicate_result() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        let result = precheck(&conn, std::path::Path::new("x.jpg"), "hash");
        assert!(
            result.is_err(),
            "数据库结构错误必须向上返回，不能伪装为重复"
        );
    }

    #[test]
    fn inspect_classifies_previewable_limited_and_unsupported_images() {
        let dir = tempfile::tempdir().unwrap();
        let jpg = dir.path().join("ok.jpg");
        let raw = dir.path().join("preview.cr2");
        let bad = dir.path().join("broken.x3f");
        image::RgbImage::from_fn(256, 192, |x, y| {
            image::Rgb([x as u8, y as u8, (x.wrapping_add(y) % 255) as u8])
        })
        .save(&jpg)
        .unwrap();
        std::fs::copy(&jpg, &raw).unwrap();
        std::fs::write(&bad, b"not an image").unwrap();

        let paths = [
            path::encode_native_path(&jpg).unwrap(),
            path::encode_native_path(&raw).unwrap(),
            path::encode_native_path(&bad).unwrap(),
        ];
        let plan = inspect_paths(&paths);

        assert_eq!(plan.images, 3);
        assert_eq!(
            plan.items
                .iter()
                .find(|item| item.path.ends_with("ok.jpg"))
                .unwrap()
                .preview_status,
            ImportPreviewStatus::Ready
        );
        assert_eq!(
            plan.items
                .iter()
                .find(|item| item.path.ends_with("preview.cr2"))
                .unwrap()
                .preview_status,
            ImportPreviewStatus::Limited
        );
        assert_eq!(
            plan.items
                .iter()
                .find(|item| item.path.ends_with("broken.x3f"))
                .unwrap()
                .preview_status,
            ImportPreviewStatus::Unsupported
        );
    }

    #[test]
    fn import_preflight_rejects_unsupported_and_accepts_limited_previews() {
        let item = |path: &str, preview_status| super::ImportPlanItem {
            path: path.to_string(),
            kind: "image".to_string(),
            size: 1,
            preview_status,
            preview_message: None,
        };
        let blocked = super::ImportPlan {
            items: vec![item("/data/broken.x3f", ImportPreviewStatus::Unsupported)],
            images: 1,
            videos: 0,
            total_size: 1,
            warnings: Vec::new(),
        };
        let error = super::require_previewable(&blocked).unwrap_err();
        assert_eq!(error.code(), "UNSUPPORTED");
        assert!(error.to_string().contains("/data/broken.x3f"));

        let supported = super::ImportPlan {
            items: vec![item("/data/preview.cr2", ImportPreviewStatus::Limited)],
            images: 1,
            videos: 0,
            total_size: 1,
            warnings: Vec::new(),
        };
        assert!(super::require_previewable(&supported).is_ok());
    }
}
