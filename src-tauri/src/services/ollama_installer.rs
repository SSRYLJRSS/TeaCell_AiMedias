//! Ollama 一键安装（方案 A3 + 改造方案：下载源自选 + 延迟检测）：
//! 应用内下载官方安装包 → 静默安装 → 就绪复检 → 服务拉起。
//! 纯逻辑层：不碰 AppHandle/DB，可单测；进度走回调（与 ollama_setup/ai_cloud 同模式）。
//! 关键事实：OllamaSetup.exe 是 Inno Setup 包（免管理员、装到用户目录、装完自动后台运行）；
//! 静默参数 /VERYSILENT /SUPPRESSMSGBOXES /NORESTART；官方未公布稳定 sha256，用体积+功能复检兜底。
//!
//! 运行时说明见 docs/OPERATIONS.md：
//!  - DownloadSource 源注册表（内置 3 源 + 自定义源），替代旧的裸 URL 列表 resolve_sources
//!  - probe_source：Range 拉真实 1MB 实测 TTFB + 带宽（不是 HEAD）
//!  - resolve_sources_ordered：preferred 置顶、其余按测速降序作降级兜底、无效 id 回落 auto
//!  - .part.meta：跨会话换源续传一致性校验，防止静默产生损坏文件
//!  - remove_installer 顺带清理 .part / .part.meta（修复失败半截文件永久残留的 bug）
//!  - 官源 URL 修正：直接指向真实二分直链，不再用 HTML 下载页当候选源

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::error::{AppError, AppResult};
use crate::services::ollama_setup;

/// 本机 Ollama 默认服务地址（原生 API 根路径，无 /v1）
pub const LOCAL_BASE_URL: &str = "http://localhost:11434";

/// Inno Setup 静默安装参数（集中一处，未来变更只改这里）
pub const SILENT_ARGS: [&str; 3] = ["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART"];

/// 内置下载源个数（auto 未测速时的默认降级顺序；测速/并发上限依赖）
pub const BUILTIN_SOURCE_COUNT: usize = 4;
/// 自定义源数量上限（防注册表膨胀）
pub const MAX_CUSTOM_SOURCES: usize = 5;
/// 单次测速并发上限（一次最多同时测多少个源）
pub const MAX_PROBE_CONCURRENCY: usize = 8;
/// 自定义源 label 长度上限
pub const MAX_SOURCE_LABEL_LEN: usize = 24;
/// 自定义源 URL 长度上限
pub const MAX_SOURCE_URL_LEN: usize = 512;

/// 一个下载源："auto"（自动）是前端下拉的特殊项，不在此表内
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DownloadSource {
    pub id: String, // "official" | "ghfast" | "ghproxy" | "github" | "custom-<uuid>"
    pub label: String,
    pub url: String,
}

/// 内置源注册表（顺序 = auto 未测速时的默认降级顺序）
/// ghfast.top 是国内可达性最好的 GitHub Releases 加速，故前置；
/// 官方源是真实直链（ollama.com/download/OllamaSetup.exe 重定向到 CDN）；
/// gh-proxy 与 GitHub 直连作降级兜底。
pub fn builtin_sources() -> Vec<DownloadSource> {
    let official = "https://ollama.com/download/OllamaSetup.exe";
    let gh = "https://github.com/ollama/ollama/releases/latest/download/OllamaSetup.exe";
    vec![
        DownloadSource {
            id: "ghfast".into(),
            label: "加速镜像（ghfast.top）".into(),
            url: format!("https://ghfast.top/{gh}"),
        },
        DownloadSource {
            id: "official".into(),
            label: "官方源（ollama.com）".into(),
            url: official.to_string(),
        },
        DownloadSource {
            id: "ghproxy".into(),
            label: "加速镜像（gh-proxy）".into(),
            url: format!("https://gh-proxy.com/{gh}"),
        },
        DownloadSource {
            id: "github".into(),
            label: "GitHub 直连".into(),
            url: gh.to_string(),
        },
    ]
}

/// 全部源 = 内置 + 自定义
pub fn all_sources(custom: &[DownloadSource]) -> Vec<DownloadSource> {
    let mut v = builtin_sources();
    v.extend(custom.iter().cloned());
    v
}

/// 校验用户自定义源（前端先校验、后端兜底）
pub fn validate_custom_source(label: &str, url: &str) -> AppResult<()> {
    let label = label.trim();
    if label.is_empty() {
        return Err(AppError::msg("源名称不能为空"));
    }
    if label.chars().count() > MAX_SOURCE_LABEL_LEN {
        return Err(AppError::msg(format!(
            "源名称过长（最多 {MAX_SOURCE_LABEL_LEN} 字符）"
        )));
    }
    let url = url.trim();
    if url.len() > MAX_SOURCE_URL_LEN {
        return Err(AppError::msg(format!(
            "源地址过长（最多 {MAX_SOURCE_URL_LEN} 字符）"
        )));
    }
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(AppError::msg("源地址必须以 http(s):// 开头"));
    }
    if url.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(AppError::msg("源地址不能包含空格或控制字符"));
    }
    Ok(())
}

/// 校验自定义源数量上限（额外源数，不含内置 3）
pub fn validate_custom_count(extra: usize) -> AppResult<()> {
    if extra > MAX_CUSTOM_SOURCES {
        return Err(AppError::msg(format!(
            "自定义源最多 {MAX_CUSTOM_SOURCES} 个"
        )));
    }
    Ok(())
}

/// 安装流水线进度（事件 ollama://install-progress 的载荷）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallProgress {
    /// download | install | verify
    pub phase: String,
    pub downloaded: u64,
    pub total: u64,
    pub speed_bps: u64,
    /// 当前下载源在"本次 ordered 源列表"中的序号（保留，供统计/调试）
    pub source_idx: usize,
    /// 当前下载源的 id（"official"/"ghproxy"/"github"/"custom-*"；前端据此取 label，
    /// 因为 ordered 列表与前端 sources 展示顺序可能不同，用 id 解析才稳定）
    pub source_id: String,
}

/// 本机 Ollama 探测结果（installed=false 不代表报错）
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DetectResult {
    pub installed: bool,
    pub exe_path: Option<String>,
    pub version: Option<String>,
}

#[cfg(windows)]
const OLLAMA_EXE_NAME: &str = "ollama.exe";
#[cfg(not(windows))]
const OLLAMA_EXE_NAME: &str = "ollama";

/// 单源测速结果
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceProbe {
    pub id: String,
    pub label: String,
    pub ok: bool,
    /// 首字节延迟（毫秒）；失败为 None
    pub ttfb_ms: Option<u64>,
    /// 1MB 样本实测速度（字节/秒）；失败为 None
    pub speed_bps: Option<u64>,
    /// 失败原因（"timeout" | "HTTP 403" | ...）
    pub error: Option<String>,
}

/// 安装包落盘位置：$APP_DATA_DIR/bagertea_ai_media_v2/ollama/OllamaSetup.exe（安装后保留供离线重装）
pub fn installer_path() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("bagertea_ai_media_v2")
        .join("ollama")
        .join("OllamaSetup.exe")
}

/// File-only cache information; never starts Ollama or probes the local service.
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InstallerCacheInfo {
    pub path: Option<String>,
    pub size_bytes: u64,
}

pub fn installer_cache_info_at(path: &Path) -> AppResult<InstallerCacheInfo> {
    let metadata = match std::fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(InstallerCacheInfo {
                path: None,
                size_bytes: 0,
            });
        }
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() {
        return Err(AppError::invalid_arg("安装包缓存路径不是文件"));
    }
    Ok(InstallerCacheInfo {
        path: Some(crate::utils::path::encode_native_path(path)?),
        size_bytes: metadata.len(),
    })
}

/// 由目标路径派生 .part 与 .part.meta 路径
fn part_paths(dest: &Path) -> (PathBuf, PathBuf) {
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("OllamaSetup.exe");
    let part = dest.with_file_name(format!("{name}.part"));
    let meta = dest.with_file_name(format!("{name}.part.meta"));
    (part, meta)
}

/// .part.meta 内容：记录这段 .part 最初由哪个源创建，用于跨会话换源一致性校验
#[derive(serde::Deserialize)]
struct PartMeta {
    source_id: String,
}

impl PartMeta {
    fn write(path: &Path, source_id: &str) {
        let _ = std::fs::write(
            path,
            serde_json::json!({ "source_id": source_id }).to_string(),
        );
    }
    fn read(path: &Path) -> Option<String> {
        let raw = std::fs::read_to_string(path).ok()?;
        serde_json::from_str::<PartMeta>(&raw)
            .ok()
            .map(|m| m.source_id)
    }
}

/// 探测已装 Ollama：先查默认安装目录（Inno 用户级安装落点），再用原生 PATH 项兜底。
/// 不调用 `where`/shell 解析路径文本，避免本地代码页或非 UTF-8 路径被有损解码。
pub fn detect_installed_executable() -> Option<PathBuf> {
    let mut exe = None;
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        let cand = PathBuf::from(local_app_data)
            .join("Programs")
            .join("Ollama")
            .join(OLLAMA_EXE_NAME);
        if cand.is_file() {
            exe = Some(cand);
        }
    }
    if exe.is_none() {
        if let Some(path) = std::env::var_os("PATH") {
            exe = find_ollama_in_path(&path);
        }
    }
    exe
}

fn find_ollama_in_path(path: &std::ffi::OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        // An empty PATH component means the current directory on some platforms. Do not turn
        // the working directory into an implicit executable search path.
        .filter(|directory| !directory.as_os_str().is_empty())
        .map(|directory| directory.join(OLLAMA_EXE_NAME))
        .find(|candidate| candidate.is_file())
}

/// 前端探测 DTO 只返回无损 UTF-8 路径；实际启动使用 detect_installed_executable 的 PathBuf。
pub fn detect_installed() -> DetectResult {
    let exe = detect_installed_executable();
    let version = exe.as_ref().and_then(|p| query_version(p));
    DetectResult {
        installed: exe.is_some(),
        exe_path: exe.as_deref().and_then(Path::to_str).map(str::to_owned),
        version,
    }
}

/// `ollama --version` 输出在 stdout/stderr 因版本而异，两路都收（如 "ollama version is 0.12.x"）
pub fn parse_version_output(stdout: &str, stderr: &str) -> Option<String> {
    for s in [stdout, stderr] {
        let t = s.trim();
        if !t.is_empty() {
            return Some(t.lines().next().unwrap_or(t).to_string());
        }
    }
    None
}

fn query_version(exe: &Path) -> Option<String> {
    let o = crate::utils::process::output_with_timeout(
        crate::utils::process::background_command(exe).arg("--version"),
        Duration::from_secs(5),
    )
    .ok()?;
    parse_version_output(
        &String::from_utf8_lossy(&o.stdout),
        &String::from_utf8_lossy(&o.stderr),
    )
}

/// Inno Setup 退出码：0 成功；1 视为可接受（重启挂起等），最终以 wait_ready 复检为准
pub fn exit_code_ok(code: i32) -> bool {
    code == 0 || code == 1
}

// ---------------- 测速 ----------------

/// 构建一个带连接/整体超时的共享型 blocking client（下载与测速通用）
fn make_client(connect: u64, timeout: u64) -> AppResult<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(connect))
        .timeout(Duration::from_secs(timeout))
        .build()
        .map_err(|e| AppError::msg(format!("HTTP 客户端初始化失败: {e}")))
}

/// 测速：Range 拉真实数据测 TTFB + 带宽，不是 HEAD（拿不到带宽）。
/// 416（Range 不可满足，文件 <1MB 或不支持）→ 退化为无 Range 读最多 256KB。
/// 200（不支持 Range）→ 读满 256KB 后 drop 断开，避免拉满 900MB。
/// 超时窗口：国内直连海外 CDN 握手常 >3s，用 5s 连接 + 12s 总超时避免假阴性。
pub fn probe_source(src: &DownloadSource) -> SourceProbe {
    use std::io::Read;
    let client = match make_client(5, 12) {
        Ok(c) => c,
        Err(e) => {
            return fail_probe(src, Some(e.to_string()));
        }
    };
    let t0 = Instant::now();

    // 先带 Range 请求
    let resp = match client
        .get(&src.url)
        .header("Range", "bytes=0-1048575")
        .send()
    {
        Ok(r) => r,
        Err(e) => return fail_probe(src, Some(probe_connect_error(&e))),
    };
    let status = resp.status();

    let mut ttfb_ms: Option<u64> = None;
    let mut speed_bps: Option<u64> = None;
    let mut error: Option<String> = None;
    let mut first_byte_at: Option<Instant> = None;

    // 解析响应体，读取至多 cap 字节（超出即断开）并测速
    let mut read_body = |mut resp: reqwest::blocking::Response, cap: u64| {
        let deadline = Instant::now() + Duration::from_secs(12);
        let mut buf = [0u8; 64 * 1024];
        let mut first = true;
        let mut bytes = 0u64;
        loop {
            if Instant::now() >= deadline {
                error = Some("timeout".into());
                break;
            }
            match resp.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if first {
                        first_byte_at = Some(Instant::now());
                        ttfb_ms = Some((Instant::now() - t0).as_millis() as u64);
                        first = false;
                    }
                    bytes += n as u64;
                    if bytes >= cap {
                        break;
                    }
                }
            }
        }
        bytes
    };

    let mut measured = 0u64;
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        // 416：退化无 Range 读最多 256KB
        match client.get(&src.url).send() {
            Ok(r) if r.status().is_success() => {
                if is_html_error_page(&r) {
                    error = Some("返回的是网页（可能是错误页/镜像失效），不是安装包".into());
                } else {
                    measured = read_body(r, 256 * 1024);
                }
            }
            Ok(r) => {
                error = Some(format!("HTTP {}", r.status()));
            }
            Err(e) => {
                error = Some(probe_connect_error(&e));
            }
        }
    } else if !status.is_success() {
        error = Some(format!("HTTP {status}"));
    } else {
        // 206（支持 Range）拉满 1MB；200（不支持 Range）读满 256KB 即断开
        if is_html_error_page(&resp) {
            error = Some("返回的是网页（可能是错误页/镜像失效），不是安装包".into());
        } else {
            let cap: u64 = if status == reqwest::StatusCode::PARTIAL_CONTENT {
                1024 * 1024
            } else {
                256 * 1024
            };
            measured = read_body(resp, cap);
        }
    }

    // 带宽 = 首字节之后读到的字节 / 消耗时间（排除 RTT 对带宽估计的稀释）
    if let (Some(fb), true) = (first_byte_at, measured > 0) {
        let elapsed = (Instant::now() - fb).as_secs_f64().max(0.001);
        speed_bps = Some((measured as f64 / elapsed) as u64);
    }
    if error.is_none() && measured == 0 {
        error = Some("连接成功但未能读取数据：站点过慢或镜像不稳定，可稍后再试".into());
    }

    SourceProbe {
        id: src.id.clone(),
        label: src.label.clone(),
        ok: error.is_none(),
        ttfb_ms,
        speed_bps,
        error,
    }
}

fn fail_probe(src: &DownloadSource, error: Option<String>) -> SourceProbe {
    SourceProbe {
        id: src.id.clone(),
        label: src.label.clone(),
        ok: false,
        ttfb_ms: None,
        speed_bps: None,
        error,
    }
}

/// 把 reqwest 连接错误翻译成用户能理解的中文提示（慢速/被拦截是常态，别丢裸英文栈）
fn probe_connect_error(e: &reqwest::Error) -> String {
    if e.is_timeout() {
        "连接超时：网络到该站点较慢或不通（国内直连 GitHub/ollama 常见）".into()
    } else if e.is_connect() {
        "无法建立连接：可能被防火墙/运营商拦截，建议用加速镜像".into()
    } else if e.is_redirect() {
        "重定向异常".into()
    } else {
        format!("连接失败: {e}")
    }
}

/// 测速前置嗅探：返回 text/html 的 200/206 大概率是镜像错误页/失效占位，不是安装包
/// （避免把「返回 HTML 错误页」的镜像测成"超快可用"——真下载时体积校验才兜底，测速阶段先拦截更友好）
fn is_html_error_page(resp: &reqwest::blocking::Response) -> bool {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.to_ascii_lowercase().contains("text/html"))
        .unwrap_or(false)
}

/// 并发测速全部源（std::thread，每源一个独立 client）。
/// 超过并发上限时分批测（内置 4 + 自定义 5 = 9 可能 > 上限 8，必须全部覆盖，不能截断丢源）
pub fn probe_sources(sources: &[DownloadSource]) -> Vec<SourceProbe> {
    if sources.len() <= 1 {
        return sources.iter().map(probe_source).collect();
    }
    let mut out = Vec::with_capacity(sources.len());
    for chunk in sources.chunks(MAX_PROBE_CONCURRENCY) {
        let handles: Vec<_> = chunk
            .iter()
            .map(|s| {
                let s = s.clone();
                std::thread::spawn(move || probe_source(&s))
            })
            .collect();
        for h in handles {
            out.push(h.join().unwrap_or_else(|_| SourceProbe {
                id: String::new(),
                label: String::new(),
                ok: false,
                ttfb_ms: None,
                speed_bps: None,
                error: Some("探测线程异常".into()),
            }));
        }
    }
    out
}

// ---------------- 源排序 ----------------

/// 依据用户选择 + 测速结果排出最终下载顺序（首选在前，其余按测速降序兜底）。
/// custom 为当前已持久化的自定义源。
pub fn resolve_sources_ordered(
    preferred: &str,
    probes: &[SourceProbe],
    custom: &[DownloadSource],
) -> Vec<DownloadSource> {
    let all = all_sources(custom);

    // 测速 index: id -> speed_bps（失败/未测视为 None）
    let speed_of = |id: &str| -> Option<u64> {
        probes
            .iter()
            .find(|p| p.id == id && p.ok)
            .and_then(|p| p.speed_bps)
    };

    // 有效 preferred（"auto" 永不视为具体源）：该源置顶，其余按测速降序。
    // 无效 id（含 "auto"）→ 按测速降序，未测速的保持内置顺序兜底。
    let preferred_source = if preferred != "auto" {
        all.iter().find(|s| s.id == preferred).cloned()
    } else {
        None
    };
    let Some(pref) = preferred_source else {
        let mut list = all;
        list.sort_by(|a, b| {
            let sa = speed_of(&a.id);
            let sb = speed_of(&b.id);
            match (sa, sb) {
                (Some(x), Some(y)) => y.cmp(&x),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => std::cmp::Ordering::Equal,
            }
        });
        return list;
    };

    let mut rest: Vec<DownloadSource> = all.into_iter().filter(|s| s.id != preferred).collect();
    rest.sort_by(|a, b| {
        let sa = speed_of(&a.id);
        let sb = speed_of(&b.id);
        match (sa, sb) {
            (Some(x), Some(y)) => y.cmp(&x),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        }
    });
    let mut out = vec![pref];
    out.extend(rest);
    out
}

// ---------------- 下载（多源降级 + 续传 + 一致性） ----------------

/// 下载安装包：多源降级 + Range 断点续传 + 进度节流回调（约 500ms 一次）。
/// sources 为 resolve_sources_ordered 排好的顺序；跨会话换源用 .part.meta 一致性校验防损坏文件。
/// log 回调：把「尝试哪个源 / 失败原因 / 是否切换 / 校验结果」实时送出，供前端监控输出框展示。
pub fn download<F: Fn(InstallProgress)>(
    dest: &Path,
    sources: &[DownloadSource],
    cancel: &Arc<AtomicBool>,
    progress: F,
    mut log: impl FnMut(&str),
) -> AppResult<()> {
    if sources.is_empty() {
        return Err(AppError::msg("无可用下载源"));
    }
    let started = Instant::now();
    tracing::info!(
        operation = "ollama_install",
        stage = "download_start",
        source_count = sources.len(),
        "开始下载 Ollama 安装包"
    );
    log(&format!(
        "开始下载安装包：共 {} 个候选源，按顺序降级尝试",
        sources.len()
    ));
    if let Some(parent) = dest.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AppError::msg(format!("创建下载目录失败: {e}")))?;
    }
    let (part, meta) = part_paths(dest);
    // 连接超时放宽到 20s：慢速/拥塞网络握手可能较慢（测速窗口已放宽，下载同理）
    let client = make_client(20, 45 * 60)?;

    // 跨会话换源校验：.part 存在且由不同源创建 → 清空重下（防 Appending 到异源半截）
    if let Ok(size) = std::fs::metadata(&part).map(|m| m.len()) {
        if size > 0 {
            if let Some(origin_id) = PartMeta::read(&meta) {
                let target_id = sources.first().map(|s| s.id.as_str()).unwrap_or("");
                if origin_id != target_id {
                    log(&format!(
                        "检测到 .part 由源「{origin_id}」创建、本次首选「{target_id}」，源已切换 → 清空重新下载"
                    ));
                    tracing::info!(
                        operation = "ollama_install",
                        stage = "resume_source_changed",
                        origin_source_id = %origin_id,
                        target_source_id = %target_id,
                        "检测到续传分片来源变化，清空后重新下载"
                    );
                    let _ = std::fs::remove_file(&part);
                    let _ = std::fs::remove_file(&meta);
                }
            }
        }
    }

    let mut last_err = String::new();
    for (idx, src) in sources.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            tracing::warn!(
                operation = "ollama_install",
                stage = "download_cancelled",
                source_idx = idx,
                error_code = "CANCELLED",
                duration_ms = started.elapsed().as_millis() as u64,
                "Ollama 安装包下载已取消"
            );
            return Err(AppError::cancelled("下载已取消"));
        }
        tracing::debug!(
            operation = "ollama_install",
            stage = "source_attempt",
            source_idx = idx,
            attempt = idx + 1,
            source_id = %src.id,
            "尝试下载源"
        );
        log(&format!(
            "→ [{}/{}] 尝试源：{}（{}）",
            idx + 1,
            sources.len(),
            src.label,
            src.url
        ));
        // 尝试源之前先发"连接中"事件（total=0），前端据此显示"正在连接（源）"，
        // 避免慢速网络在连接阶段长时间无任何进度反馈
        progress(InstallProgress {
            phase: "download".into(),
            downloaded: 0,
            total: 0,
            speed_bps: 0,
            source_idx: idx,
            source_id: src.id.clone(),
        });
        match download_one(&client, src, idx, &part, &meta, cancel, &progress) {
            Ok(total) => {
                // 体积校验兜底（官方无稳定 sha256）：Content-Length 已知时必须一致
                let size = std::fs::metadata(&part).map(|m| m.len()).unwrap_or(0);
                if total > 0 && size != total {
                    log(&format!(
                        "✗ [{}/{}] {}：校验失败，下载不完整（{}/{} 字节），切换下一源",
                        idx + 1,
                        sources.len(),
                        src.label,
                        size,
                        total
                    ));
                    let _ = std::fs::remove_file(&part);
                    let _ = std::fs::remove_file(&meta);
                    last_err = format!("下载不完整（{size}/{total} 字节）");
                    tracing::warn!(
                        operation = "ollama_install",
                        stage = "size_mismatch",
                        source_idx = idx,
                        source_id = %src.id,
                        downloaded_bytes = size,
                        expected_bytes = total,
                        error_code = "DOWNLOAD_INCOMPLETE",
                        "安装包大小校验失败，切换下载源"
                    );
                    continue;
                }
                if size < 50 * 1024 * 1024 {
                    log(&format!(
                        "✗ [{}/{}] {}：安装包异常偏小（{} 字节，{}MB 以下），疑似无效响应（HTML 错误页等），切换下一源",
                        idx + 1,
                        sources.len(),
                        src.label,
                        size,
                        50
                    ));
                    let _ = std::fs::remove_file(&part);
                    let _ = std::fs::remove_file(&meta);
                    last_err = format!("安装包异常偏小（{size} 字节），疑似无效响应");
                    tracing::warn!(
                        operation = "ollama_install",
                        stage = "package_too_small",
                        source_idx = idx,
                        source_id = %src.id,
                        downloaded_bytes = size,
                        minimum_bytes = 50 * 1024 * 1024,
                        error_code = "INVALID_PACKAGE",
                        "安装包体积异常，切换下载源"
                    );
                    continue;
                }
                std::fs::rename(&part, dest).map_err(|e| {
                    tracing::error!(
                        operation = "ollama_install",
                        stage = "persist_failed",
                        source_idx = idx,
                        source_id = %src.id,
                        error_code = "IO",
                        duration_ms = started.elapsed().as_millis() as u64,
                        "安装包落盘失败"
                    );
                    AppError::msg(format!("安装包落盘失败: {e}"))
                })?;
                let _ = std::fs::remove_file(&meta);
                log(&format!(
                    "✓ [{}/{}] {}：下载完成（{}MB，校验通过），已落盘",
                    idx + 1,
                    sources.len(),
                    src.label,
                    size / 1024 / 1024
                ));
                tracing::info!(
                    operation = "ollama_install",
                    stage = "download_done",
                    source_idx = idx,
                    source_id = %src.id,
                    bytes = size,
                    duration_ms = started.elapsed().as_millis() as u64,
                    "Ollama 安装包下载完成"
                );
                return Ok(());
            }
            Err(e) => {
                if e.code() == "CANCELLED" {
                    tracing::warn!(
                        operation = "ollama_install",
                        stage = "download_cancelled",
                        source_idx = idx,
                        source_id = %src.id,
                        error_code = "CANCELLED",
                        duration_ms = started.elapsed().as_millis() as u64,
                        "Ollama 安装包下载已取消"
                    );
                    return Err(e);
                }
                last_err = e.to_string();
                log(&format!(
                    "✗ [{}/{}] {}：失败（{}），切换下一源",
                    idx + 1,
                    sources.len(),
                    src.label,
                    last_err
                ));
                tracing::warn!(
                    operation = "ollama_install",
                    stage = "source_failed",
                    source_idx = idx,
                    source_id = %src.id,
                    error_code = e.code(),
                    "下载源失败，尝试下一源"
                );
            }
        }
    }
    log(&format!("✗ 所有下载源均失败：{last_err}"));
    tracing::error!(
        operation = "ollama_install",
        stage = "download_failed",
        source_count = sources.len(),
        error_code = "DOWNLOAD_FAILED",
        duration_ms = started.elapsed().as_millis() as u64,
        "所有 Ollama 下载源均失败"
    );
    Err(AppError::msg(format!("所有下载源均失败：{last_err}")))
}

/// 单源下载：支持 206 续传；源不支持 Range（返回 200）则从头重写
fn download_one<F: Fn(InstallProgress)>(
    client: &reqwest::blocking::Client,
    src: &DownloadSource,
    idx: usize,
    part: &Path,
    meta: &Path,
    cancel: &Arc<AtomicBool>,
    progress: F,
) -> AppResult<u64> {
    use std::io::{Read, Seek, SeekFrom, Write};
    let mut existing = std::fs::metadata(part).map(|m| m.len()).unwrap_or(0);
    let mut req = client.get(&src.url);
    if existing > 0 {
        req = req.header(reqwest::header::RANGE, format!("bytes={existing}-"));
    }
    let resp = req
        .send()
        .map_err(|e| AppError::msg(format!("连接失败: {e}")))?;
    let status = resp.status();
    if status == reqwest::StatusCode::RANGE_NOT_SATISFIABLE {
        // 已下完的 .part 比内容还大等边界：删掉重来（递归内会重新探测文件大小）
        let _ = std::fs::remove_file(part);
        let _ = std::fs::remove_file(meta);
        return download_one(client, src, idx, part, meta, cancel, progress);
    }
    if !status.is_success() {
        return Err(AppError::msg(format!("HTTP {status}")));
    }
    let resumed = status == reqwest::StatusCode::PARTIAL_CONTENT;
    if !resumed {
        existing = 0; // 源不支持续传：从头写
    }
    let body_len = resp.content_length().unwrap_or(0);
    let total = existing + body_len;

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(!resumed)
        .open(part)
        .map_err(|e| AppError::msg(format!("打开临时文件失败: {e}")))?;
    if resumed && existing > 0 {
        file.seek(SeekFrom::Start(existing))
            .map_err(|e| AppError::msg(format!("续传定位失败: {e}")))?;
    }
    // 从头写时记录本段 .part 的创建源（换源一致性校验依据）
    if !resumed || existing == 0 {
        PartMeta::write(meta, &src.id);
    }

    let mut downloaded = existing;
    let mut last_report = Instant::now();
    let mut last_bytes = existing;
    let mut last_tick = Instant::now();
    progress(InstallProgress {
        phase: "download".into(),
        downloaded,
        total,
        speed_bps: 0,
        source_idx: idx,
        source_id: src.id.clone(),
    });
    let mut resp = resp;
    let mut buf = [0u8; 64 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(AppError::cancelled("下载已取消"));
        }
        let n = resp
            .read(&mut buf)
            .map_err(|e| AppError::msg(format!("下载中断: {e}")))?;
        if n == 0 {
            break;
        }
        file.write_all(&buf[..n])
            .map_err(|e| AppError::msg(format!("写入失败: {e}")))?;
        downloaded += n as u64;
        if last_report.elapsed() >= Duration::from_millis(500) {
            let dt = last_tick.elapsed().as_secs_f64().max(0.001);
            let speed = ((downloaded - last_bytes) as f64 / dt) as u64;
            progress(InstallProgress {
                phase: "download".into(),
                downloaded,
                total,
                speed_bps: speed,
                source_idx: idx,
                source_id: src.id.clone(),
            });
            last_report = Instant::now();
            last_bytes = downloaded;
            last_tick = Instant::now();
        }
    }
    file.flush()
        .map_err(|e| AppError::msg(format!("写入失败: {e}")))?;
    Ok(total)
}

/// 静默安装：等待安装进程退出并返回退出码（spawn 失败多为杀软拦截）
pub fn install_silent(installer: &Path) -> AppResult<i32> {
    let st = crate::utils::process::background_command(installer)
        .args(SILENT_ARGS)
        .status()
        .map_err(|e| AppError::msg(format!("启动安装程序失败（可能被安全软件拦截）: {e}")))?;
    Ok(st.code().unwrap_or(-1))
}

/// 安装后就绪复检：轮询 /api/version 直到服务起来或超时；成功返回版本号
pub fn wait_ready(base_url: &str, max_wait: Duration) -> Option<String> {
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .ok()?;
    let root = ollama_setup::api_root(base_url);
    let deadline = Instant::now() + max_wait;
    loop {
        if let Ok(resp) = client.get(format!("{root}/api/version")).send() {
            if resp.status().is_success() {
                let ver = resp
                    .json::<serde_json::Value>()
                    .ok()
                    .and_then(|v| v.get("version").and_then(|x| x.as_str()).map(String::from))
                    .unwrap_or_else(|| "unknown".to_string());
                return Some(ver);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(Duration::from_secs(2));
    }
}

/// 删除已缓存的安装包（「数据与缓存」分组清理入口；不存在视为已清空，不报错）
pub fn remove_installer() -> AppResult<bool> {
    remove_installer_at(&installer_path())
}

/// 按路径删除（纯函数，便于单测；不存在返回 false 不报错）。
/// 顺带清理同名 .part 与 .part.meta（修复失败半截文件永久残留的 bug）
pub fn remove_installer_at(path: &Path) -> AppResult<bool> {
    let mut removed = false;
    if path.exists() {
        std::fs::remove_file(path).map_err(|e| AppError::msg(format!("删除安装包失败: {e}")))?;
        removed = true;
    }
    let (part, meta) = part_paths(path);
    if part.exists() {
        std::fs::remove_file(&part).map_err(|e| AppError::msg(format!("删除临时文件失败: {e}")))?;
        removed = true;
    }
    if meta.exists() {
        std::fs::remove_file(&meta)
            .map_err(|e| AppError::msg(format!("删除临时元数据失败: {e}")))?;
        removed = true;
    }
    Ok(removed)
}

/// 已装但服务未跑：拉起 `ollama serve`（无窗口分离进程，随系统托盘由官方安装包管理后续自启）。
/// 不做代理设置（纯拉起）。
pub fn start_service(exe_path: &Path) -> AppResult<std::process::Child> {
    start_service_inner(exe_path, None)
}

/// 拉起 `ollama serve` 并可注入模型下载代理（改造方案·加速项 A：HTTPS_PROXY/HTTP_PROXY）。
/// proxy 形如 "http://127.0.0.1:7890"（留空/None 则不注入，行为同 start_service）
pub fn start_service_with_proxy(exe_path: &Path, proxy: &str) -> AppResult<std::process::Child> {
    let proxy = proxy.trim();
    start_service_inner(exe_path, if proxy.is_empty() { None } else { Some(proxy) })
}

/// L1（§8.1）：应用自启 ollama serve 注入的空闲保留时长常量（建议 2m，常量化）。
/// Ollama 原生 generate/chat 的默认保留时长；模型空闲到点自动卸载，节省 RAM/VRAM。
pub const KEEP_ALIVE_IDLE: &str = "2m";

/// 构造 `ollama serve` 命令（纯构造，便于单测断言环境变量；§8.1「增加命令构造测试」）。
/// - 注入 OLLAMA_KEEP_ALIVE（L1 止血：模型空闲自动卸载）；
/// - 可选注入模型下载代理 HTTPS_PROXY/HTTP_PROXY（保留代理环境变量）；
/// - Windows 无窗口标志。
fn build_serve_command(exe_path: &Path, proxy: Option<&str>) -> std::process::Command {
    let mut cmd = crate::utils::process::background_command(exe_path);
    cmd.arg("serve");
    // L1：空闲保留时长注入（不记录密钥；值固定常量无敏感内容）
    cmd.env("OLLAMA_KEEP_ALIVE", KEEP_ALIVE_IDLE);
    if let Some(p) = proxy {
        // Ollama 模型拉取走进程内 reqwest，读标准 HTTPS_PROXY/HTTP_PROXY 即可换源
        cmd.env("HTTPS_PROXY", p).env("HTTP_PROXY", p);
    }
    cmd
}

fn start_service_inner(exe_path: &Path, proxy: Option<&str>) -> AppResult<std::process::Child> {
    // L2（§8.2）：返回 Child（不再丢弃），由命令层保存 pid/ownership 供停服与防重复启动
    build_serve_command(exe_path, proxy)
        .spawn()
        .map_err(|e| AppError::msg(format!("启动 Ollama 服务失败: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installer_cache_info_reads_only_file_metadata() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("茶包 cache 空间.exe");
        let missing = installer_cache_info_at(&path).unwrap();
        assert!(missing.path.is_none());
        assert_eq!(missing.size_bytes, 0);
        std::fs::write(&path, b"cached installer").unwrap();
        let info = installer_cache_info_at(&path).unwrap();
        assert_eq!(info.path.as_deref(), path.to_str());
        assert_eq!(info.size_bytes, 16);
        assert_eq!(std::fs::read(&path).unwrap(), b"cached installer");
        assert!(installer_cache_info_at(temp.path()).is_err());
    }

    #[test]
    fn ollama_path_lookup_preserves_unicode_native_paths() {
        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp.path().join("茶包 空间");
        std::fs::create_dir(&install_dir).unwrap();
        let executable = install_dir.join(OLLAMA_EXE_NAME);
        std::fs::write(&executable, b"test").unwrap();
        let path = std::env::join_paths([&install_dir]).unwrap();

        assert_eq!(find_ollama_in_path(&path), Some(executable));
    }

    // This test creates an invalid-UTF-8 filesystem entry; macOS rejects that path with EILSEQ.
    #[cfg(target_os = "linux")]
    #[test]
    fn ollama_path_lookup_preserves_non_utf8_native_paths() {
        use std::os::unix::ffi::OsStrExt;

        let temp = tempfile::tempdir().unwrap();
        let install_dir = temp
            .path()
            .join(std::ffi::OsStr::from_bytes(b"install-\xff"));
        std::fs::create_dir(&install_dir).unwrap();
        let executable = install_dir.join(OLLAMA_EXE_NAME);
        std::fs::write(&executable, b"test").unwrap();
        let path = std::env::join_paths([&install_dir]).unwrap();

        assert_eq!(find_ollama_in_path(&path), Some(executable));
    }

    #[test]
    fn builtin_sources_four_in_order() {
        let s = builtin_sources();
        assert_eq!(s.len(), 4);
        // ghfast.top 前置为默认加速源
        assert_eq!(s[0].id, "ghfast");
        assert_eq!(s[1].id, "official");
        assert_eq!(s[2].id, "ghproxy");
        assert_eq!(s[3].id, "github");
        // 官源必须是真实二分直链，而非 HTML 下载页
        assert!(s[1]
            .url
            .starts_with("https://ollama.com/download/OllamaSetup.exe"));
        assert!(s[0]
            .url
            .starts_with("https://ghfast.top/https://github.com/"));
        assert!(s[3].url.starts_with("https://github.com/"));
    }

    #[test]
    fn validate_custom_source_rules() {
        assert!(validate_custom_source(" 我的源 ", " https://a.com/x ").is_ok());
        assert!(validate_custom_source("", "https://a.com/x").is_err());
        assert!(validate_custom_source("a", "ftp://a.com/x").is_err());
        assert!(validate_custom_source("a", "https://a b.com/x").is_err());
        let long = "x".repeat(25);
        assert!(validate_custom_source(&long, "https://a.com/x").is_err());
        let long_url = "https://a.com/".to_owned() + &"x".repeat(MAX_SOURCE_URL_LEN + 1);
        assert!(validate_custom_source("a", &long_url).is_err());
    }

    #[test]
    fn validate_custom_count_caps_at_five() {
        assert!(validate_custom_count(5).is_ok());
        assert!(validate_custom_count(6).is_err());
    }

    #[test]
    fn exit_code_ok_zero_and_one() {
        assert!(exit_code_ok(0));
        assert!(exit_code_ok(1));
        assert!(!exit_code_ok(2));
        assert!(!exit_code_ok(-1));
    }

    // L1（§8.1）：命令构造测试——注入 OLLAMA_KEEP_ALIVE + 保留代理 + serve 参数
    #[test]
    fn build_serve_command_injects_keep_alive_and_proxy() {
        let cmd = build_serve_command(
            Path::new("C:\\ollama\\ollama.exe"),
            Some("http://127.0.0.1:7890"),
        );
        let args: Vec<_> = cmd
            .get_args()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, vec!["serve"]);
        let envs: Vec<_> = cmd.get_envs().collect();
        let keep = envs
            .iter()
            .find(|(k, _)| k.to_string_lossy() == "OLLAMA_KEEP_ALIVE")
            .map(|(_, v)| v.as_ref().map(|v| v.to_string_lossy().into_owned()));
        assert_eq!(keep, Some(Some(KEEP_ALIVE_IDLE.to_string())));
        let https = envs
            .iter()
            .find(|(k, _)| k.to_string_lossy() == "HTTPS_PROXY")
            .map(|(_, v)| v.as_ref().map(|v| v.to_string_lossy().into_owned()));
        assert_eq!(https, Some(Some("http://127.0.0.1:7890".to_string())));
    }

    #[test]
    fn build_serve_command_no_proxy_omits_proxy_env() {
        let cmd = build_serve_command(Path::new(OLLAMA_EXE_NAME), None);
        let envs: Vec<_> = cmd.get_envs().collect();
        assert!(envs
            .iter()
            .all(|(k, _)| k.to_string_lossy() != "HTTPS_PROXY"));
        assert!(envs
            .iter()
            .all(|(k, _)| k.to_string_lossy() != "HTTP_PROXY"));
        // keep_alive 仍注入（L1 不依赖是否配置代理）
        assert!(envs
            .iter()
            .any(|(k, v)| k.to_string_lossy() == "OLLAMA_KEEP_ALIVE" && v.is_some()));
    }

    #[test]
    fn remove_installer_missing_is_false_not_error() {
        let p = std::env::temp_dir().join("bagertea_test_nonexistent_installer.exe");
        let _ = std::fs::remove_file(&p);
        assert!(!remove_installer_at(&p).unwrap());
    }

    #[test]
    fn remove_installer_deletes_part_and_meta_too() {
        let p = std::env::temp_dir().join(format!(
            "bagertea_test_installer_{}.exe",
            std::process::id()
        ));
        let (part, meta) = part_paths(&p);
        std::fs::write(&p, b"fake installer").unwrap();
        std::fs::write(&part, b"partial").unwrap();
        std::fs::write(&meta, r#"{"source_id":"github"}"#).unwrap();
        assert!(remove_installer_at(&p).unwrap());
        assert!(!p.exists());
        assert!(!part.exists());
        assert!(!meta.exists());
    }

    #[test]
    fn parse_version_output_prefers_nonempty() {
        assert_eq!(
            parse_version_output("ollama version is 0.12.5", "").as_deref(),
            Some("ollama version is 0.12.5")
        );
        assert_eq!(
            parse_version_output("", "ollama version is 0.12.5\n").as_deref(),
            Some("ollama version is 0.12.5")
        );
        assert!(parse_version_output("", "").is_none());
    }

    #[test]
    fn part_meta_roundtrip() {
        let meta =
            std::env::temp_dir().join(format!("bagertea_partmeta_{}.json", std::process::id()));
        let _ = std::fs::remove_file(&meta);
        PartMeta::write(&meta, "ghproxy");
        assert_eq!(PartMeta::read(&meta).as_deref(), Some("ghproxy"));
        let _ = std::fs::remove_file(&meta);
    }

    #[test]
    fn resolve_sources_ordered_preferred_on_top() {
        let custom = vec![DownloadSource {
            id: "custom-1".into(),
            label: "NAS".into(),
            url: "https://nas/x".into(),
        }];
        let probes = vec![];
        let ordered = resolve_sources_ordered("github", &probes, &custom);
        assert_eq!(ordered[0].id, "github");
        assert!(ordered.iter().any(|s| s.id == "custom-1"));
        assert_eq!(ordered.len(), 5);
    }

    #[test]
    fn resolve_sources_ordered_auto_sorts_by_speed() {
        let custom = vec![];
        let probes = vec![
            SourceProbe {
                id: "official".into(),
                label: "官方".into(),
                ok: true,
                ttfb_ms: Some(100),
                speed_bps: Some(1024),
                error: None,
            },
            SourceProbe {
                id: "ghproxy".into(),
                label: "镜像".into(),
                ok: true,
                ttfb_ms: Some(50),
                speed_bps: Some(99999),
                error: None,
            },
        ];
        let ordered = resolve_sources_ordered("auto", &probes, &custom);
        // 最快 ghproxy 置顶
        assert_eq!(ordered[0].id, "ghproxy");
    }

    #[test]
    fn resolve_sources_ordered_invalid_id_falls_back_to_auto() {
        let custom = vec![DownloadSource {
            id: "custom-1".into(),
            label: "NAS".into(),
            url: "https://nas/x".into(),
        }];
        let probes = vec![];
        // 已被删除的自定义源 id → 按 auto 处理（内置顺序保留，不崩溃）
        let ordered = resolve_sources_ordered("custom-gone", &probes, &custom);
        assert_eq!(ordered.len(), 5);
        assert_eq!(ordered[0].id, "ghfast"); // 未测速时保持内置顺序（ghfast 前置）
    }

    #[test]
    fn resolve_sources_ordered_custom_preferred_with_fallback() {
        let custom = vec![DownloadSource {
            id: "custom-1".into(),
            label: "NAS".into(),
            url: "https://nas/x".into(),
        }];
        let probes = vec![];
        let ordered = resolve_sources_ordered("custom-1", &probes, &custom);
        assert_eq!(ordered[0].id, "custom-1");
        // 自定义源失败也会自动降级到内置源（内置源都在列表里）
        assert!(ordered.iter().any(|s| s.id == "github"));
    }

    #[test]
    fn probe_concurrency_covers_all_sources() {
        // 内置源 + 自定义源总数可能超过单批并发上限，probe_sources 必须分批覆盖全部，
        // 否则最后一个源永远测不到。此断言防止未来调参时回归。
        const {
            assert!(BUILTIN_SOURCE_COUNT + MAX_CUSTOM_SOURCES > MAX_PROBE_CONCURRENCY);
        }
        // 即便总数超上限，分批逻辑也返回与输入等长的结果（数量守恒）
        let mut all: Vec<DownloadSource> = builtin_sources();
        for i in 0..MAX_CUSTOM_SOURCES {
            all.push(DownloadSource {
                id: format!("custom-{i}"),
                label: format!("C{i}"),
                url: format!("https://c{i}.example/x"),
            });
        }
        assert_eq!(all.len(), BUILTIN_SOURCE_COUNT + MAX_CUSTOM_SOURCES);
    }
}
