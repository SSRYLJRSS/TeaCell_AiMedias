//! Ollama 一键配置命令（方案 A2）+ 一键安装（方案 A3）+ 下载源自选/测速（改造方案）：
//! 只做参数校验/转发与事件 emit，逻辑在 services/ollama_setup.rs 与 services/ollama_installer.rs

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use serde::Serialize;
use tauri::{AppHandle, Emitter, State};
use tauri_plugin_opener::OpenerExt;

use crate::commands::platform_cmd::require_managed_ollama_supported;
use crate::db::settings as db_settings;
use crate::error::{AppError, AppResult};
use crate::services::ollama_installer as installer;
use crate::services::ollama_runtime::{OllamaRuntimeSnapshot, ServiceOwnership};
use crate::services::ollama_setup::{self, GpuInfo, ModelRec, OllamaStatus, PullProgress};
use crate::state::AppState;

/// 存活检测（网络短任务，spawn_blocking 不堵主线程）
#[tauri::command]
pub async fn ollama_ping(base_url: String) -> AppResult<OllamaStatus> {
    tauri::async_runtime::spawn_blocking(move || ollama_setup::ping(&base_url))
        .await
        .map_err(|e| AppError::msg(format!("检测任务异常: {e}")))
}

/// 硬件探针 + 推荐结果
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HardwareReport {
    pub gpu: GpuInfo,
    pub recommendations: Vec<ModelRec>,
}

#[tauri::command]
pub async fn ollama_probe_hardware() -> AppResult<HardwareReport> {
    require_managed_ollama_supported()?;
    tauri::async_runtime::spawn_blocking(|| {
        let gpu = ollama_setup::probe_gpu();
        let recommendations = ollama_setup::recommend(gpu.vram_gb);
        HardwareReport {
            gpu,
            recommendations,
        }
    })
    .await
    .map_err(|e| AppError::msg(format!("硬件探测任务异常: {e}")))
}

/// 一键拉取：流式进度走 ollama://pull-progress 事件；不写 settings（前端收终态后保存）
#[tauri::command]
pub async fn ollama_pull(app: AppHandle, base_url: String, model: String) -> AppResult<()> {
    require_managed_ollama_supported()?;
    if model.trim().is_empty() {
        return Err(AppError::msg("未指定模型名"));
    }
    // 首期不做取消入口（机动项）：flag 保留以匹配 service 签名，中断重拉即续传
    let cancel = Arc::new(AtomicBool::new(false));
    tauri::async_runtime::spawn_blocking(move || {
        ollama_setup::pull(&base_url, &model, &cancel, |p: PullProgress| {
            let _ = app.emit("ollama://pull-progress", p);
        })
    })
    .await
    .map_err(|e| AppError::msg(format!("拉取任务异常: {e}")))?
}

/// 打开 Ollama 下载页（系统浏览器；插件已在 lib.rs 注册，Rust 直调免 capabilities）
#[tauri::command]
pub fn ollama_open_download_page(app: AppHandle) -> AppResult<()> {
    app.opener()
        .open_url("https://ollama.com/download", None::<&str>)
        .map_err(|e| AppError::msg(format!("打开下载页失败: {e}")))
}

// ---- 方案 A3：应用内一键安装 ----

/// 安装过程日志行（事件 ollama://install-log 的载荷；前端监控输出框展示）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallLogLine {
    /// 毫秒时间戳（unix）
    pub t: i64,
    /// info | warn | error
    pub level: String,
    pub msg: String,
}

/// 安装态聚合：探测已装 + 服务存活 + 安装包缓存路径（设置页「本地模型」向导卡片用）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallStatus {
    pub installed: bool,
    pub exe_path: Option<String>,
    pub version: Option<String>,
    pub running: bool,
    pub models: Vec<String>,
    /// 已缓存的安装包路径（存在才返回，供离线重装/清理展示）
    pub installer_path: Option<String>,
    /// 已缓存的安装包大小（字节；installer_path 为 None 时恒 0）
    pub installer_size: u64,
}

#[tauri::command]
pub async fn ollama_install_status() -> AppResult<InstallStatus> {
    require_managed_ollama_supported()?;
    tauri::async_runtime::spawn_blocking(|| {
        let d = installer::detect_installed();
        let st = ollama_setup::ping(installer::LOCAL_BASE_URL);
        let ip = installer::installer_path();
        let (installer_path, installer_size) = if ip.is_file() {
            if let Some(path) = ip.to_str() {
                let size = std::fs::metadata(&ip).map(|m| m.len()).unwrap_or(0);
                (Some(path.to_owned()), size)
            } else {
                (None, 0)
            }
        } else {
            (None, 0)
        };
        InstallStatus {
            installed: d.installed,
            exe_path: d.exe_path,
            version: d.version,
            running: st.running,
            models: st.models,
            installer_path,
            installer_size,
        }
    })
    .await
    .map_err(|e| AppError::msg(format!("安装态探测任务异常: {e}")))
}

/// Storage settings only need file metadata, not installation or runtime health.
#[tauri::command]
pub async fn ollama_installer_cache_info() -> AppResult<installer::InstallerCacheInfo> {
    require_managed_ollama_supported()?;
    tauri::async_runtime::spawn_blocking(|| {
        installer::installer_cache_info_at(&installer::installer_path())
    })
    .await
    .map_err(|error| AppError::msg(format!("缓存读取任务异常: {error}")))?
}

/// 从 settings 读取当前自定义源（installer::DownloadSource 形式）
fn read_custom_sources(state: &State<AppState>) -> AppResult<Vec<installer::DownloadSource>> {
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    let s = db_settings::get_settings(&conn)?;
    Ok(s.custom_download_sources
        .into_iter()
        .map(|c| installer::DownloadSource {
            id: c.id,
            label: c.label,
            url: c.url,
        })
        .collect())
}

/// 列出全部下载源：内置 3 + 自定义（preferred 下拉数据）
#[tauri::command]
pub fn ollama_list_sources(state: State<AppState>) -> AppResult<Vec<installer::DownloadSource>> {
    require_managed_ollama_supported()?;
    let custom = read_custom_sources(&state)?;
    Ok(installer::all_sources(&custom))
}

/// 并发测速：ids 为空/缺省 = 全部源；结果不持久化（前端内存缓存 5 分钟）
#[tauri::command]
pub async fn ollama_probe_sources(
    state: State<'_, AppState>,
    ids: Option<Vec<String>>,
) -> AppResult<Vec<installer::SourceProbe>> {
    require_managed_ollama_supported()?;
    let all = installer::all_sources(&read_custom_sources(&state)?);
    let targets: Vec<_> = match ids.as_ref().filter(|v| !v.is_empty()) {
        Some(list) => all.into_iter().filter(|s| list.contains(&s.id)).collect(),
        None => all,
    };
    if targets.is_empty() {
        return Err(AppError::msg("没有可测速的下载源"));
    }
    tauri::async_runtime::spawn_blocking(move || installer::probe_sources(&targets))
        .await
        .map_err(|e| AppError::msg(format!("测速任务异常: {e}")))
}

/// 一键安装（改造）：按 preferred_source_id 排序下载 → 静默安装 → 就绪复检；
/// preferred_source_id: "auto"（默认，内置顺序直接开下）| 源 id
#[tauri::command]
pub async fn ollama_download_install(
    app: AppHandle,
    state: State<'_, AppState>,
    preferred_source_id: String,
) -> AppResult<()> {
    require_managed_ollama_supported()?;
    let preferred = if preferred_source_id.is_empty() {
        "auto".to_string()
    } else {
        preferred_source_id
    };
    let custom = read_custom_sources(&state)?;
    tauri::async_runtime::spawn_blocking(move || {
        let dest = installer::installer_path();
        // 后端不做测速（auto 无缓存时按内置顺序直接开下）；probes 传空数组
        let ordered = installer::resolve_sources_ordered(&preferred, &[], &custom);
        // 安装日志事件：供前端监控输出框展示（ollama://install-log）
        let emit_log = |level: &str, msg: String| {
            let _ = app.emit(
                "ollama://install-log",
                crate::commands::ollama_cmd::InstallLogLine {
                    t: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_millis() as i64)
                        .unwrap_or(0),
                    level: level.to_string(),
                    msg,
                },
            );
        };
        emit_log("info", "开始一键安装：下载 → 静默安装 → 就绪复检".into());
        // 首期不做取消入口：flag 保留以匹配 service 签名，.part 支持断点续传
        let cancel = Arc::new(AtomicBool::new(false));
        installer::download(
            &dest,
            &ordered,
            &cancel,
            |p| {
                let _ = app.emit("ollama://install-progress", p);
            },
            |m| {
                // 行首 ✗ 视为 warn（失败/校验不过），其余 info
                let lvl = if m.starts_with('✗') { "warn" } else { "info" };
                emit_log(lvl, m.to_string());
            },
        )?;
        emit_log(
            "info",
            "安装包下载完成，开始静默安装（约 1–2 分钟，请勿关闭应用）…".into(),
        );
        let _ = app.emit(
            "ollama://install-progress",
            installer::InstallProgress {
                phase: "install".into(),
                downloaded: 0,
                total: 0,
                speed_bps: 0,
                source_idx: 0,
                source_id: String::new(),
            },
        );
        let code = installer::install_silent(&dest)?;
        emit_log(
            "info",
            format!("静默安装进程已退出（退出码 {code}），等待服务就绪…"),
        );
        let _ = app.emit(
            "ollama://install-progress",
            installer::InstallProgress {
                phase: "verify".into(),
                downloaded: 0,
                total: 0,
                speed_bps: 0,
                source_idx: 0,
                source_id: String::new(),
            },
        );
        if let Some(ver) = installer::wait_ready(installer::LOCAL_BASE_URL, Duration::from_secs(30))
        {
            emit_log(
                "info",
                format!("✓ Ollama 服务已就绪（版本 {ver}），安装完成"),
            );
            return Ok(());
        }
        if installer::exit_code_ok(code) {
            emit_log(
                "warn",
                "安装已完成，但服务未在 30 秒内就绪，可点「启动服务并复检」".into(),
            );
            Err(AppError::msg(
                "安装已完成，但 Ollama 服务尚未就绪：点「启动服务并复检」",
            ))
        } else {
            emit_log(
                "error",
                format!(
                    "安装失败（退出码 {code}）：可能被安全软件拦截，请手动运行安装包：{}",
                    dest.display()
                ),
            );
            Err(AppError::msg(format!(
                "安装失败（退出码 {code}）：可能被安全软件拦截，请手动运行安装包：{}",
                dest.display()
            )))
        }
    })
    .await
    .map_err(|e| AppError::msg(format!("安装任务异常: {e}")))?
}

/// 新增自定义下载源（即时落库，低风险低频写；成功返回新源 DTO）
#[tauri::command]
pub fn ollama_add_custom_source(
    state: State<AppState>,
    label: String,
    url: String,
) -> AppResult<installer::DownloadSource> {
    require_managed_ollama_supported()?;
    installer::validate_custom_source(&label, &url)?;
    let id = format!("custom-{}", uuid::Uuid::new_v4());
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    let mut s = db_settings::get_settings(&conn)?;
    installer::validate_custom_count(s.custom_download_sources.len())?;
    let src = db_settings::CustomSource {
        id: id.clone(),
        label: label.trim().to_string(),
        url: url.trim().to_string(),
    };
    db_settings::add_custom_source(&mut s, src.clone());
    db_settings::save_settings(&conn, &s)?;
    Ok(installer::DownloadSource {
        id: src.id,
        label: src.label,
        url: src.url,
    })
}

/// 删除自定义下载源（即时落库；id 无效则忽略不报错）
#[tauri::command]
pub fn ollama_remove_custom_source(state: State<AppState>, id: String) -> AppResult<()> {
    require_managed_ollama_supported()?;
    let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
    let mut s = db_settings::get_settings(&conn)?;
    db_settings::remove_custom_source(&mut s, &id);
    db_settings::save_settings(&conn, &s)?;
    Ok(())
}

/// 删除已缓存的 Ollama 安装包（释放空间；需要时重新一键下载）
#[tauri::command]
pub async fn ollama_remove_installer() -> AppResult<bool> {
    require_managed_ollama_supported()?;
    tauri::async_runtime::spawn_blocking(installer::remove_installer)
        .await
        .map_err(|e| AppError::msg(format!("删除任务异常: {e}")))?
}

/// 列出本地已装模型（含占用大小；「本地打标」模型管理面板数据源）
#[tauri::command]
pub async fn ollama_list_local_models(
    base_url: String,
) -> AppResult<Vec<ollama_setup::LocalModelInfo>> {
    require_managed_ollama_supported()?;
    tauri::async_runtime::spawn_blocking(move || ollama_setup::list_models(&base_url))
        .await
        .map_err(|e| AppError::msg(format!("模型列表任务异常: {e}")))?
}

/// 删除本地已下载模型（释放磁盘空间）；服务端错误（含模型不存在）原样透传
#[tauri::command]
pub async fn ollama_delete_model(base_url: String, model: String) -> AppResult<()> {
    require_managed_ollama_supported()?;
    tauri::async_runtime::spawn_blocking(move || ollama_setup::delete_model(&base_url, &model))
        .await
        .map_err(|e| AppError::msg(format!("删除任务异常: {e}")))?
}

/// 探测本地模型存储目录（OLLAMA_MODELS 优先，其次默认 ~/.ollama/models）
#[tauri::command]
pub fn ollama_model_dir() -> AppResult<String> {
    require_managed_ollama_supported()?;
    ollama_setup::model_dir()
}

/// 在系统文件管理器中打开本地模型存储目录
#[tauri::command]
pub fn ollama_open_model_dir(app: AppHandle) -> AppResult<()> {
    require_managed_ollama_supported()?;
    let dir = ollama_setup::model_dir()?;
    app.opener()
        .open_path(&dir, None::<&str>)
        .map_err(|e| AppError::msg(format!("打开文件夹失败: {e}")))
}

/// 已装但服务未跑：拉起 ollama serve 并复检就绪。
/// 模型下载代理（Settings.modelDownloadProxy）非空时注入 HTTPS_PROXY/HTTP_PROXY（加速项 A）
/// L2（§8.2）：启动前 ping——已有服务标记 External（不重启不改环境）；应用自启成功保存
/// Child/pid 为 AppOwned（防重复启动）；返回运行态快照。
#[tauri::command]
pub async fn ollama_start_service(state: State<'_, AppState>) -> AppResult<OllamaRuntimeSnapshot> {
    require_managed_ollama_supported()?;
    // 命令层读设置（拿代理），逻辑仍在 services（拉起+复检）
    let proxy = {
        let conn = state.db.lock().map_err(|_| AppError::msg("数据库锁中毒"))?;
        db_settings::get_settings(&conn)?.model_download_proxy
    };
    // 运行态在 spawn_blocking 外借用快照 clone（Child 不可跨线程直接持有但锁内可短操作）
    let runtime = std::sync::Arc::clone(&state.ollama_runtime);
    tauri::async_runtime::spawn_blocking(move || {
        // L2：已有服务标记 External，绝不重启/改环境（§8.2）
        if ollama_setup::ping(installer::LOCAL_BASE_URL).running {
            {
                let mut rt = runtime
                    .lock()
                    .map_err(|_| AppError::msg("Ollama 运行态锁中毒"))?;
                rt.mark_external();
            }
            tracing::info!("检测到已运行的 Ollama 服务（标记 External，应用不重启）");
            return Ok(runtime
                .lock()
                .map_err(|_| AppError::msg("Ollama 运行态锁中毒"))?
                .snapshot());
        }
        let exe = installer::detect_installed_executable()
            .ok_or_else(|| AppError::msg("未检测到已安装的 Ollama，请先一键安装"))?;
        let child = if proxy.trim().is_empty() {
            installer::start_service(&exe)?
        } else {
            tracing::info!("为 Ollama 注入模型下载代理: {}", proxy);
            installer::start_service_with_proxy(&exe, &proxy)?
        };
        // 保存 Child/pid 为 AppOwned（防重复启动）
        {
            let mut rt = runtime
                .lock()
                .map_err(|_| AppError::msg("Ollama 运行态锁中毒"))?;
            rt.register_app_owned(child);
        }
        if installer::wait_ready(installer::LOCAL_BASE_URL, Duration::from_secs(20)).is_some() {
            Ok(runtime
                .lock()
                .map_err(|_| AppError::msg("Ollama 运行态锁中毒"))?
                .snapshot())
        } else {
            // 启动但未就绪：保留 AppOwned 供用户明确停止，返回可读错误
            Err(AppError::msg(
                "Ollama 服务未在预期时间内就绪，请再点一次重试，或在服务管理停止后重新启动",
            ))
        }
    })
    .await
    .map_err(|e| AppError::msg(format!("启动服务任务异常: {e}")))?
}

/// 当前 Ollama 本地服务运行态（§8.5 高级信息 + L2 观测）
#[tauri::command]
pub async fn ollama_runtime_status(state: State<'_, AppState>) -> AppResult<OllamaRuntimeSnapshot> {
    require_managed_ollama_supported()?;
    let runtime = std::sync::Arc::clone(&state.ollama_runtime);
    tauri::async_runtime::spawn_blocking(move || {
        runtime
            .lock()
            .map_err(|_| AppError::msg("Ollama 运行态锁中毒"))
            .map(|rt| rt.snapshot())
    })
    .await
    .map_err(|e| AppError::msg(format!("运行态查询任务异常: {e}")))?
}

/// 停止 Ollama 本地服务（L2 §8.2）：仅停止 AppOwned；External 服务永不停止（§2 非目标第 9 条）。
/// 返回停止前 ownership 与是否执行了停止动作。
#[tauri::command]
pub async fn ollama_stop_service(state: State<'_, AppState>) -> AppResult<OllamaStopResult> {
    require_managed_ollama_supported()?;
    let runtime = std::sync::Arc::clone(&state.ollama_runtime);
    tauri::async_runtime::spawn_blocking(move || {
        let mut rt = runtime
            .lock()
            .map_err(|_| AppError::msg("Ollama 运行态锁中毒"))?;
        let before = rt.snapshot().ownership;
        let stopped = rt.stop_app_owned();
        let after = rt.snapshot();
        Ok(OllamaStopResult {
            before,
            stopped,
            after,
        })
    })
    .await
    .map_err(|e| AppError::msg(format!("停止服务任务异常: {e}")))?
}

/// ollama_stop_service 的返回（before/stopped/after）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OllamaStopResult {
    pub before: Option<ServiceOwnership>,
    pub stopped: bool,
    pub after: OllamaRuntimeSnapshot,
}
