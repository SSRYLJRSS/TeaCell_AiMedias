//! 视频服务：ffprobe 元数据 + ffmpeg 抽帧。
//! 发布构建只允许使用应用相邻 sidecar；仅开发构建可以回退用户 PATH。
//!
//! 指导书 阶段 2 §7.2：ffprobe stdout 必须 `piped` 并读取（历史 bug 是 `.stdout(Stdio::null())` 后再解析空 buffer，
//! 导致所有视频元数据探测静默失败）；stderr 保留到错误摘要；探测有真实墙钟超时并返回可识别错误。

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;

/// ffmpeg/ffprobe 子进程总超时：损坏/超大文件可能让裸 status() 永久挂起（批次卡死），
/// 统一加墙防止无界等待；超时后 kill + wait 并返回 `ProbeError::Timeout`。
const FFMPEG_TIMEOUT: Duration = Duration::from_secs(30);

/// 输出格式随平台代理策略显式选择，避免把 Linux 目标的 H.264 能力当作必需依赖。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProxyVariant {
    H264Mp4,
    Vp8Webm,
}

impl ProxyVariant {
    pub fn parse(value: &str) -> crate::error::AppResult<Self> {
        match value {
            "h264_mp4" => Ok(Self::H264Mp4),
            "vp8_webm" => Ok(Self::Vp8Webm),
            _ => Err(crate::error::AppError::invalid_arg(format!(
                "非法视频代理变体：{value}"
            ))),
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::H264Mp4 => "h264_mp4",
            Self::Vp8Webm => "vp8_webm",
        }
    }

    pub const fn file_extension(self) -> &'static str {
        match self {
            Self::H264Mp4 => "mp4",
            Self::Vp8Webm => "webm",
        }
    }
}

fn proxy_codec_args(variant: ProxyVariant) -> &'static [&'static str] {
    match variant {
        ProxyVariant::H264Mp4 => &[
            "-c:v",
            "libx264",
            "-preset",
            "fast",
            "-crf",
            "23",
            "-pix_fmt",
            "yuv420p",
            "-c:a",
            "aac",
            "-b:a",
            "128k",
            "-movflags",
            "+faststart",
        ],
        ProxyVariant::Vp8Webm => &[
            "-c:v",
            "libvpx",
            "-crf",
            "30",
            "-b:v",
            "1M",
            "-c:a",
            "libvorbis",
            "-b:a",
            "128k",
        ],
    }
}

/// ffprobe 可用性探测的短期缓存有效期（指导书 §7.2：PATH 探测不能每次素材请求都无界阻塞）
const BINARY_CACHE_TTL: Duration = Duration::from_secs(60);

/// ffmpeg/ffprobe 可用性结果缓存（短 TTL）。静态全局，进程内共享。
static BINARY_CACHE: Mutex<Option<(String, Instant, bool)>> = Mutex::new(None);

/// Tauri 将 sidecar 放到主可执行文件旁边；这里返回该相邻路径，供同步服务直接管理子进程。
fn sidecar_path(executable: &Path, name: &str) -> Option<std::path::PathBuf> {
    let directory = executable.parent()?;
    #[cfg(target_os = "windows")]
    let path = {
        let mut filename = std::ffi::OsString::from(name);
        filename.push(".exe");
        directory.join(filename)
    };
    #[cfg(not(target_os = "windows"))]
    let path = directory.join(name);

    path.is_file().then_some(path)
}

fn resolve_binary_with(
    name: &str,
    executable: Option<&Path>,
    allow_path_fallback: bool,
    mut is_available: impl FnMut(&Path) -> bool,
) -> Option<PathBuf> {
    let bundled = executable.and_then(|executable| sidecar_path(executable, name));
    if let Some(path) = bundled {
        if is_available(&path) {
            return Some(path);
        }
        // A packaged sidecar is the release contract. Do not silently substitute an arbitrary
        // system binary when the package is incomplete or the sidecar cannot run.
        if !allow_path_fallback {
            return None;
        }
    }

    if !allow_path_fallback {
        return None;
    }

    let path = PathBuf::from(name);
    is_available(&path).then_some(path)
}

fn executable_responds_to_version(path: &Path) -> bool {
    crate::utils::process::output_with_timeout(
        crate::utils::process::background_command(path).arg("-version"),
        Duration::from_secs(5),
    )
    .is_ok_and(|output| output.status.success())
}

fn resolve_binary(name: &str) -> Option<PathBuf> {
    let executable = std::env::current_exe().ok();
    resolve_binary_with(
        name,
        executable.as_deref(),
        cfg!(debug_assertions),
        executable_responds_to_version,
    )
}

/// 返回 FFmpeg 工具身份指纹，不读取整个可执行文件。
///
/// 指纹由已解析的实际路径、文件大小、修改时间和 `-version` 首行组成；获取过程位于
/// 调用方的阻塞线程，且必须在数据库锁之外。它用于代理缓存失效，不是供应链签名。
pub fn ffmpeg_tool_fingerprint() -> crate::error::AppResult<String> {
    use sha2::{Digest, Sha256};
    use std::time::UNIX_EPOCH;

    let path = resolve_binary("ffmpeg").ok_or_else(|| {
        crate::error::AppError::unsupported("FFmpeg sidecar 不可用（仅开发构建允许从 PATH 回退）")
    })?;
    let output = crate::utils::process::output_with_timeout(
        crate::utils::process::background_command(&path).arg("-version"),
        Duration::from_secs(5),
    )
    .map_err(|e| crate::error::AppError::msg(format!("读取 FFmpeg 版本失败: {e}")))?;
    if !output.status.success() {
        return Err(crate::error::AppError::unsupported(
            "FFmpeg sidecar 存在但无法通过版本探测",
        ));
    }
    let version_line = String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    let metadata = std::fs::metadata(&path)?;
    let modified_ns = metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();

    let mut hasher = Sha256::new();
    hasher.update(b"ffmpeg-tool-identity-v1\0");
    hasher.update(path.to_string_lossy().as_bytes());
    hasher.update([0]);
    hasher.update(metadata.len().to_le_bytes());
    hasher.update(modified_ns.to_le_bytes());
    hasher.update(version_line.as_bytes());
    Ok(format!("ffmpeg-meta-v1:{:x}", hasher.finalize()))
}

/// 带短 TTL 缓存的 ffprobe 可用性探测：避免每次素材请求都 spawn `ffprobe -version`。
pub fn ffprobe_available() -> bool {
    binary_available("ffprobe")
}

/// 带短 TTL 缓存的 ffmpeg 可用性探测。
pub fn ffmpeg_available() -> bool {
    binary_available("ffmpeg")
}

fn binary_available(name: &str) -> bool {
    if let Ok(cache) = BINARY_CACHE.lock() {
        if let Some((cached_name, at, val)) = &*cache {
            if cached_name == name && at.elapsed() < BINARY_CACHE_TTL {
                return *val;
            }
        }
    }
    let val = resolve_binary(name).is_some();
    if let Ok(mut cache) = BINARY_CACHE.lock() {
        *cache = Some((name.to_string(), Instant::now(), val));
    }
    val
}

/// ffprobe 探测错误：不同失败原因必须可区分（指导书 §7.2）。
#[derive(Debug, thiserror::Error)]
pub enum ProbeError {
    #[error("ffprobe 不可用")]
    BinaryMissing,
    #[error("启动 ffprobe 失败: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("ffprobe 探测超时（>{FFMPEG_TIMEOUT:?}），已终止")]
    Timeout,
    #[error("ffprobe 退出码非 0（code={code}）：{stderr}")]
    NonZeroExit { code: i32, stderr: String },
    #[error("ffprobe 输出解析失败: {0}")]
    Parse(#[source] serde_json::Error),
}

#[derive(Debug, Clone, Default)]
pub struct VideoMeta {
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub duration_ms: Option<i64>,
    pub video_codec: Option<String>,
    pub audio_codec: Option<String>,
    // 指导书 §7.3 结构化字段（对高频筛选列结构化，长尾进 raw_json）
    pub container_format: Option<String>,
    pub video_profile: Option<String>,
    pub pixel_format: Option<String>,
    pub bit_depth: Option<i64>,
    pub frame_rate: Option<f64>,
    pub video_bit_rate: Option<i64>,
    pub color_range: Option<String>,
    pub color_space: Option<String>,
    pub color_transfer: Option<String>,
    pub color_primaries: Option<String>,
    pub rotation: Option<i64>,
    pub audio_sample_rate: Option<i64>,
    pub audio_channels: Option<i64>,
    pub audio_layout: Option<String>,
    /// 原始 ffprobe JSON 保留（未来补字段无需重读文件）
    pub raw_json: Option<String>,
    /// GPS 定位（format.tags 的 ISO 6709 location 解析，带符号十进制度，北纬东经为正）
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
    /// 拍摄时间（format.tags.creation_time，ISO 8601 UTC → epoch 毫秒，与图片 taken_at 口径一致）
    pub taken_at: Option<i64>,
}

#[derive(Deserialize)]
struct FfprobeOutput {
    streams: Option<Vec<FfprobeStream>>,
    format: Option<FfprobeFormat>,
}

#[derive(Deserialize)]
struct FfprobeStream {
    codec_type: Option<String>,
    codec_name: Option<String>,
    profile: Option<String>,
    width: Option<i64>,
    height: Option<i64>,
    pix_fmt: Option<String>,
    bit_depth: Option<i64>,
    #[serde(rename = "avg_frame_rate")]
    avg_frame_rate: Option<String>,
    #[serde(rename = "r_frame_rate")]
    r_frame_rate: Option<String>,
    #[serde(rename = "bit_rate")]
    bit_rate: Option<String>,
    #[serde(rename = "color_range")]
    color_range: Option<String>,
    #[serde(rename = "color_space")]
    color_space: Option<String>,
    #[serde(rename = "color_transfer")]
    color_transfer: Option<String>,
    #[serde(rename = "color_primaries")]
    color_primaries: Option<String>,
    #[serde(rename = "sample_rate")]
    sample_rate: Option<String>,
    channels: Option<i64>,
    /// 声道布局（如 stereo/5.1）——FB6 需求六：audio_layout 的正确来源（此前误用 tags.language/title）
    #[serde(rename = "channel_layout")]
    channel_layout: Option<String>,
    tags: Option<std::collections::HashMap<String, serde_json::Value>>,
    side_data_list: Option<Vec<FfprobeSideData>>,
}

#[derive(Deserialize)]
struct FfprobeSideData {
    #[serde(rename = "side_data_type")]
    side_data_type: Option<String>,
    rotation: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct FfprobeFormat {
    #[serde(rename = "format_name")]
    format_name: Option<String>,
    duration: Option<String>,
    #[serde(rename = "bit_rate")]
    bit_rate: Option<String>,
    /// 容器级标签：location / com.apple.quicktime.location（ISO 6709）、creation_time（ISO 8601 UTC）
    tags: Option<std::collections::HashMap<String, serde_json::Value>>,
}

/// 把 `2/1`、`30000/1001` 之类的分数解析为 f64；非分数/无效返回 None。
fn parse_rate(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() || s == "0/0" || s == "N/A" {
        return None;
    }
    if let Some((num, den)) = s.split_once('/') {
        let n: f64 = num.trim().parse().ok()?;
        let d: f64 = den.trim().parse().ok()?;
        if d == 0.0 {
            return None;
        }
        Some(n / d)
    } else {
        s.parse::<f64>().ok()
    }
}

/// 旋转值：ffprobe 在 side_data.displaymatrix 里是数字（90/-90），在 tags.rotate 里是字符串（"90"），
/// 需同时接受数字与数字字符串并归一化到 0..360。
fn as_rotation(v: &serde_json::Value) -> Option<i64> {
    let n = if let Some(n) = v.as_i64() {
        n as f64
    } else if let Some(n) = v.as_f64() {
        n
    } else {
        v.as_str()?.trim().parse::<f64>().ok()?
    };
    Some((n.rem_euclid(360.0)) as i64)
}

/// ISO 6709 单个坐标分量 → 十进制度。`dd_len` 是度的位数（纬 2 / 经 3）：
///  - 带小数点 → 十进制度形式（+30.2500）；
///  - 纯数字按位数判定：dd 位 = 度，dd+2 = 度分，dd+4 = 度分秒。
fn parse_iso6709_component(s: &str, dd_len: usize) -> Option<f64> {
    let (sign, rest) = match s.chars().next()? {
        '+' => (1.0, &s[1..]),
        '-' => (-1.0, &s[1..]),
        _ => return None,
    };
    if rest.is_empty() || !rest.chars().all(|c| c.is_ascii_digit() || c == '.') {
        return None;
    }
    let v = if rest.contains('.') {
        rest.parse::<f64>().ok()?
    } else {
        match rest.len() {
            n if n == dd_len => rest.parse::<f64>().ok()?,
            n if n == dd_len + 2 => {
                let deg: f64 = rest[..dd_len].parse().ok()?;
                let minutes: f64 = rest[dd_len..].parse().ok()?;
                if minutes >= 60.0 {
                    return None;
                }
                deg + minutes / 60.0
            }
            n if n == dd_len + 4 => {
                let deg: f64 = rest[..dd_len].parse().ok()?;
                let minutes: f64 = rest[dd_len..dd_len + 2].parse().ok()?;
                let seconds: f64 = rest[dd_len + 2..].parse().ok()?;
                if minutes >= 60.0 || seconds >= 60.0 {
                    return None;
                }
                deg + minutes / 60.0 + seconds / 3600.0
            }
            _ => return None,
        }
    };
    if !v.is_finite() {
        return None;
    }
    Some(sign * v)
}

/// ISO 6709 定位串（手机视频常见，形如 `+30.2500+120.1670/`，可含高度尾段）→
/// (纬度, 经度) 带符号十进制度。格式异常一律 None，绝不 panic。
pub fn parse_iso6709_location(s: &str) -> Option<(f64, f64)> {
    let s = s.trim().trim_end_matches('/');
    if s.len() < 2 {
        return None;
    }
    // 纬度之后的第一个 +/- 是经度起始（位置 0 的符号属于纬度）
    let split = s[1..].find(['+', '-']).map(|i| i + 1)?;
    let (lat_s, rest) = s.split_at(split);
    // rest 可能还带高度段（+30.25+120.16+10.5）：只取前两段，高度丢弃。
    // 经度结束于其后第一个符号字符（若有）。
    let lon_end = rest[1..]
        .find(['+', '-'])
        .map(|i| i + 1)
        .unwrap_or(rest.len());
    let lon_s = &rest[..lon_end];
    let lat = parse_iso6709_component(lat_s, 2)?;
    let lon = parse_iso6709_component(lon_s, 3)?;
    if lat.abs() > 90.0 || lon.abs() > 180.0 {
        return None;
    }
    Some((lat, lon))
}

/// ffprobe format.tags.creation_time（ISO 8601 UTC，如 2024-03-15T14:30:00.123456Z）
/// → epoch 毫秒（绝对时刻，与图片 taken_at 同口径；本地时区换算在展示层发生）。
pub fn parse_creation_time_ms(s: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(s.trim())
        .ok()
        .map(|dt| dt.timestamp_millis())
}

/// 从 ffprobe JSON 字节流解析出 `VideoMeta`（导出的纯函数，便于 fixture 单测）。
/// - 主视频流：codec/profile/pixel/bit depth/分辨率/帧率/码率/色彩/旋转。
/// - 第一条音频轨：codec/采样率/声道/声道布局。
/// - 容器：format_name + duration + bit_rate。
pub fn parse_ffprobe_json(bytes: &[u8]) -> Result<VideoMeta, ProbeError> {
    let parsed: FfprobeOutput = serde_json::from_slice(bytes).map_err(ProbeError::Parse)?;
    let mut meta = VideoMeta::default();
    if let Some(streams) = &parsed.streams {
        let mut video_seen = false;
        for s in streams {
            match s.codec_type.as_deref() {
                Some("video") if !video_seen => {
                    video_seen = true;
                    meta.video_codec = s.codec_name.clone();
                    meta.video_profile = s.profile.clone();
                    meta.width = s.width;
                    meta.height = s.height;
                    meta.pixel_format = s.pix_fmt.clone();
                    meta.bit_depth = s.bit_depth;
                    meta.frame_rate = s
                        .avg_frame_rate
                        .as_deref()
                        .and_then(parse_rate)
                        .or_else(|| s.r_frame_rate.as_deref().and_then(parse_rate));
                    meta.video_bit_rate = s.bit_rate.as_deref().and_then(|b| b.parse::<i64>().ok());
                    meta.color_range = s.color_range.clone();
                    meta.color_space = s.color_space.clone();
                    meta.color_transfer = s.color_transfer.clone();
                    meta.color_primaries = s.color_primaries.clone();
                    // 旋转：优先 side_data（displaymatrix），退化到 tags.rotate
                    if let Some(sd) = &s.side_data_list {
                        for d in sd {
                            if d.side_data_type.as_deref() == Some("Display Matrix") {
                                if let Some(rot) = &d.rotation {
                                    meta.rotation = as_rotation(rot);
                                }
                            }
                        }
                    }
                    if meta.rotation.is_none() {
                        if let Some(r) = s.tags.as_ref().and_then(|t| t.get("rotate")) {
                            meta.rotation = as_rotation(r);
                        }
                    }
                }
                Some("audio")
                    // 只取第一条音轨作为主音轨摘要（多音轨完整信息保留在 raw_json）
                    if meta.audio_codec.is_none() => {
                        meta.audio_codec = s.codec_name.clone();
                        meta.audio_sample_rate =
                            s.sample_rate.as_deref().and_then(|v| v.parse::<i64>().ok());
                        meta.audio_channels = s.channels;
                        // FB6 需求六：声道布局来自 ffprobe 的 channel_layout（stereo/5.1 等）；
                        // 缺失时保持 None（UI 显示「未提供」），不再误把 language/title 当布局。
                        meta.audio_layout = s.channel_layout.clone();
                    }
                _ => {}
            }
        }
    }
    if let Some(f) = &parsed.format {
        meta.container_format = f.format_name.clone();
        if let Some(d) = &f.duration {
            meta.duration_ms = d.parse::<f64>().ok().map(|v| (v * 1000.0) as i64);
        }
        if meta.video_bit_rate.is_none() {
            meta.video_bit_rate = f.bit_rate.as_deref().and_then(|b| b.parse::<i64>().ok());
        }
        // GPS 定位与拍摄时间：容器级 tags。com.apple.quicktime.location 是 QuickTime 原生字段，
        // 优先于通用 location（两者同源，前者存在时更可靠）；均为 ISO 6709，格式异常丢弃。
        if let Some(tags) = &f.tags {
            let loc = tags
                .get("com.apple.quicktime.location")
                .or_else(|| tags.get("location"))
                .and_then(|v| v.as_str())
                .and_then(parse_iso6709_location);
            if let Some((lat, lon)) = loc {
                meta.latitude = Some(lat);
                meta.longitude = Some(lon);
            }
            meta.taken_at = tags
                .get("creation_time")
                .and_then(|v| v.as_str())
                .and_then(parse_creation_time_ms);
        }
    }
    if let Ok(text) = std::str::from_utf8(bytes) {
        meta.raw_json = Some(text.to_string());
    }
    Ok(meta)
}

/// ffprobe 提取元数据（指导书 §7.2 修复版）：
///  - stdout 必须 `piped` 并读取（历史 bug 是 `Stdio::null()`，导致解析空 buffer 而静默失败）；
///  - stderr 保留到错误摘要，不无条件丢弃；
///  - 真实墙钟超时，超时 kill + wait，返回 `ProbeError::Timeout`；
///  - 二进制缺失/启动失败/退出码非 0/解析失败/超时各有可识别错误。
pub fn probe(path: &Path) -> Result<VideoMeta, ProbeError> {
    // 需要解析 stdout，所以 ffprobe 必须存在
    if !ffprobe_available() {
        return Err(ProbeError::BinaryMissing);
    }
    let ffprobe = resolve_binary("ffprobe").ok_or(ProbeError::BinaryMissing)?;

    let mut child = crate::utils::process::background_command(ffprobe)
        .args([
            "-v",
            "error",
            "-print_format",
            "json",
            "-show_format",
            "-show_streams",
            "-show_programs",
            "-show_chapters",
        ])
        .arg(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(ProbeError::Spawn)?;

    // 用线程并行读取 stdout/stderr（避免输出阻塞子进程导致无法退出）
    let mut stdout_pipe = child.stdout.take();
    let mut stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut o) = stdout_pipe.take() {
            let _ = o.read_to_end(&mut buf);
        }
        buf
    });
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut e) = stderr_pipe.take() {
            let _ = e.read_to_end(&mut buf);
        }
        buf
    });

    // 真实墙钟超时：轮询 try_wait，超时 kill + wait
    let deadline = Instant::now() + FFMPEG_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    // 先 join 读线程避免资源泄漏
                    let _ = stdout_reader.join();
                    let _ = stderr_reader.join();
                    tracing::warn!(
                        "ffprobe 探测超时（>{:?}），已终止：{}",
                        FFMPEG_TIMEOUT,
                        path.display()
                    );
                    return Err(ProbeError::Timeout);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => {
                let _ = stdout_reader.join();
                let _ = stderr_reader.join();
                return Err(ProbeError::Spawn(std::io::Error::other(
                    "等待 ffprobe 失败",
                )));
            }
        }
    };

    let stdout = stdout_reader.join().unwrap_or_default();
    let stderr_bytes = stderr_reader.join().unwrap_or_default();

    let status =
        status.ok_or_else(|| ProbeError::Spawn(std::io::Error::other("ffprobe 未返回状态")))?;
    if !status.success() {
        let stderr = String::from_utf8_lossy(&stderr_bytes).trim().to_string();
        return Err(ProbeError::NonZeroExit {
            code: status.code().unwrap_or(-1),
            stderr,
        });
    }

    // 指导书：stdout 被丢弃/为空时不得静默通过
    if stdout.is_empty() {
        return Err(ProbeError::NonZeroExit {
            code: status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&stderr_bytes).trim().to_string(),
        });
    }

    parse_ffprobe_json(&stdout)
}

/// ffmpeg 抽帧（缩放到指定边长，输出 webp/jpg 由 out 扩展名决定）。
/// 带 30s 超时：超时强制 kill 子进程返回 false，杜绝损坏/超大视频让批次无限挂起
pub fn extract_frame(path: &Path, time_ms: i64, out: &Path, size: u32) -> bool {
    let Some(ffmpeg) = resolve_binary("ffmpeg") else {
        return false;
    };
    let secs = format!("{:.3}", time_ms as f64 / 1000.0);
    let mut child = match crate::utils::process::background_command(ffmpeg)
        .args(["-y", "-ss", &secs, "-i"])
        .arg(path)
        .args([
            "-frames:v",
            "1",
            "-vf",
            &format!("scale={size}:{size}:force_original_aspect_ratio=decrease"),
            "-loglevel",
            "error",
        ])
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return false,
    };
    // 带超时的等待：进程超时未退出 → kill，避免抽帧卡死批次
    let deadline = Instant::now() + FFMPEG_TIMEOUT;
    let ok = loop {
        match child.try_wait() {
            Ok(Some(st)) => break st.success(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    tracing::warn!(
                        "ffmpeg 抽帧超时（>{:?}），已终止：{}",
                        FFMPEG_TIMEOUT,
                        path.display()
                    );
                    break false;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break false,
        }
    };
    ok && out.exists()
}

/// 向后兼容入口：转码为 H.264/AAC MP4。
pub fn transcode_to_h264(
    src: &Path,
    out: &Path,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> crate::error::AppResult<()> {
    transcode_variant(src, out, ProxyVariant::H264Mp4, cancel)
}

/// 按显式容器/编码器策略生成视频兼容代理。
pub fn transcode_variant(
    src: &Path,
    out: &Path,
    variant: ProxyVariant,
    cancel: Option<&std::sync::atomic::AtomicBool>,
) -> crate::error::AppResult<()> {
    let Some(ffmpeg) = resolve_binary("ffmpeg") else {
        return Err(crate::error::AppError::msg("ffmpeg 不可用"));
    };
    let mut child = crate::utils::process::background_command(ffmpeg)
        .args(["-y", "-i"])
        .arg(src)
        .args(proxy_codec_args(variant))
        .arg(out)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| crate::error::AppError::msg(format!("启动 ffmpeg 失败: {e}")))?;

    // stderr 读线程，避免管道缓冲阻塞子进程
    let mut stderr_pipe = child.stderr.take();
    let stderr_reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut e) = stderr_pipe.take() {
            let _ = e.read_to_end(&mut buf);
        }
        buf
    });

    let deadline = Instant::now() + FFMPEG_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(st)) => break Some(st),
            Ok(None) => {
                if Instant::now() >= deadline || cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
                    let _ = child.kill();
                    let _ = child.wait();
                    let _ = stderr_reader.join();
                    return Err(crate::error::AppError::msg("转码超时或已取消"));
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break None,
        }
    };
    let stderr = String::from_utf8_lossy(&stderr_reader.join().unwrap_or_default())
        .trim()
        .to_string();
    let Some(st) = status else {
        return Err(crate::error::AppError::msg("转码进程异常"));
    };
    if !st.success() {
        return Err(crate::error::AppError::msg(format!(
            "转码失败（退出码 {:?}）：{}",
            st.code(),
            stderr
        )));
    }
    Ok(())
}

/// P3-02：视频 AI 打标抽帧——取每段中点帧（1280px webp）到指定目录；
/// 返回实际抽出的帧路径（时长未知时退化为 2s/7s/12s，跳过第 0 秒）；ffmpeg 不可用或全部失败返回空
///
/// FB2-07（§13.5）：帧位取每段中点。原公式 d*i/n 的第一帧恒为第 0ms（相机视频开头常是黑场/自动曝光未稳定），
/// 取中点避开片头黑场与片尾字幕/淡出 —— n=3 → 16.7% / 50% / 83%。抽帧走全局解码并发闸，避免批量打标拉满 CPU。
pub fn extract_keyframes(
    path: &Path,
    duration_ms: Option<i64>,
    dir: &Path,
    n: usize,
) -> Vec<std::path::PathBuf> {
    let n = n.max(1);
    // FB2-07：帧位取每段中点（n==1 时自然给出 d*0.5，与原 vec![d/2] 分支行为一致，已合并删除）
    let times: Vec<i64> = match duration_ms {
        Some(d) if d > 0 => (0..n)
            .map(|i| ((d as f64) * (i as f64 + 0.5) / (n as f64)) as i64)
            .collect(),
        // 时长未知：退化为固定间隔，跳过第 0 秒
        _ => (0..n).map(|i| 2000 + i as i64 * 5000).collect(),
    };
    let mut out = Vec::new();
    for (i, t) in times.iter().enumerate() {
        let frame = dir.join(format!("kframe_{i}.webp"));
        // FB2-07：每帧申请一次全局解码/抽帧并发闸 permit（acquire 返回 RAII guard，作用域内持有）。
        // 绝不能 `let _ = acquire()` —— 那会立即 drop，permit 白拿；也不能提到循环外（会降低整段吞吐）。
        let _permit = crate::services::imaging::acquire();
        if extract_frame(path, *t, &frame, 1280) {
            out.push(frame);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn json(s: &str) -> Vec<u8> {
        s.as_bytes().to_vec()
    }

    #[test]
    fn proxy_variants_select_matching_format_and_encoders() {
        let mp4 = ProxyVariant::parse("h264_mp4").unwrap();
        assert_eq!(mp4.file_extension(), "mp4");
        assert!(proxy_codec_args(mp4).contains(&"libx264"));
        assert!(proxy_codec_args(mp4).contains(&"aac"));

        let webm = ProxyVariant::parse("vp8_webm").unwrap();
        assert_eq!(webm.file_extension(), "webm");
        assert!(proxy_codec_args(webm).contains(&"libvpx"));
        assert!(proxy_codec_args(webm).contains(&"libvorbis"));
        assert!(ProxyVariant::parse("mp4").is_err());
    }

    #[test]
    fn packaged_sidecar_is_resolved_beside_the_app_executable() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("bagertea");
        let sidecar = dir.path().join(if cfg!(target_os = "windows") {
            "ffprobe.exe"
        } else {
            "ffprobe"
        });
        fs::write(&sidecar, b"test binary placeholder").unwrap();

        assert_eq!(sidecar_path(&app, "ffprobe"), Some(sidecar));
    }

    #[test]
    fn resolver_prefers_a_working_packaged_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("bagertea");
        let sidecar = dir.path().join(if cfg!(target_os = "windows") {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        });
        fs::write(&sidecar, b"sidecar").unwrap();
        let mut tested = Vec::new();

        let resolved = resolve_binary_with("ffmpeg", Some(&app), true, |candidate| {
            tested.push(candidate.to_path_buf());
            true
        });

        assert_eq!(resolved, Some(sidecar.clone()));
        assert_eq!(tested, vec![sidecar]);
    }

    #[test]
    fn release_resolver_never_uses_path_when_sidecar_is_missing_or_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("bagertea");
        let mut tested = Vec::new();

        let missing = resolve_binary_with("ffmpeg", Some(&app), false, |candidate| {
            tested.push(candidate.to_path_buf());
            true
        });
        assert_eq!(missing, None);
        assert!(tested.is_empty(), "sidecar 缺失时不能探测 PATH");

        let sidecar = dir.path().join(if cfg!(target_os = "windows") {
            "ffmpeg.exe"
        } else {
            "ffmpeg"
        });
        fs::write(&sidecar, b"not executable").unwrap();
        let invalid = resolve_binary_with("ffmpeg", Some(&app), false, |candidate| {
            tested.push(candidate.to_path_buf());
            false
        });
        assert_eq!(invalid, None);
        assert_eq!(tested, vec![sidecar], "坏 sidecar 后不能回退 PATH");
    }

    #[test]
    fn development_resolver_falls_back_to_path_after_bad_sidecar() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("bagertea");
        let sidecar = dir.path().join(if cfg!(target_os = "windows") {
            "ffprobe.exe"
        } else {
            "ffprobe"
        });
        fs::write(&sidecar, b"bad sidecar").unwrap();
        let mut tested = Vec::new();

        let resolved = resolve_binary_with("ffprobe", Some(&app), true, |candidate| {
            tested.push(candidate.to_path_buf());
            candidate == Path::new("ffprobe")
        });

        assert_eq!(resolved, Some(PathBuf::from("ffprobe")));
        assert_eq!(tested, vec![sidecar, PathBuf::from("ffprobe")]);
    }

    #[test]
    fn parses_h264_with_duration_and_color() {
        let j = r#"
        {
          "streams": [
            {"index":0,"codec_type":"video","codec_name":"h264","profile":"High",
             "width":1920,"height":1080,"pix_fmt":"yuv420p","avg_frame_rate":"30000/1001",
             "bit_rate":"5000000","color_range":"tv","color_space":"bt709",
             "color_transfer":"bt709","color_primaries":"bt709",
             "tags":{"rotate":"90"}},
            {"index":1,"codec_type":"audio","codec_name":"aac","sample_rate":"44100","channels":2,
             "tags":{"language":"eng","title":"Main"}}
          ],
          "format": {"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"12.345","bit_rate":"5100000"}
        }"#;
        let meta = parse_ffprobe_json(&json(j)).unwrap();
        assert_eq!(meta.video_codec.as_deref(), Some("h264"));
        assert_eq!(meta.video_profile.as_deref(), Some("High"));
        assert_eq!(meta.width, Some(1920));
        assert_eq!(meta.height, Some(1080));
        assert_eq!(meta.pixel_format.as_deref(), Some("yuv420p"));
        assert_eq!(meta.duration_ms, Some(12345));
        assert_eq!(meta.frame_rate, Some(30000.0 / 1001.0));
        assert_eq!(meta.video_bit_rate, Some(5000000));
        assert_eq!(meta.color_space.as_deref(), Some("bt709"));
        assert_eq!(meta.rotation, Some(90));
        assert_eq!(meta.audio_codec.as_deref(), Some("aac"));
        assert_eq!(
            meta.container_format.as_deref(),
            Some("mov,mp4,m4a,3gp,3g2,mj2")
        );
        assert!(meta.raw_json.is_some());
    }

    #[test]
    fn parses_hevc_main10_with_bit_depth_no_rot() {
        let j = r#"
        {
          "streams": [
            {"index":0,"codec_type":"video","codec_name":"hevc","profile":"Main 10",
             "width":3840,"height":2160,"pix_fmt":"yuv420p10le","bit_depth":10,
             "avg_frame_rate":"24000/1001","bit_rate":"12000000"}
          ],
          "format": {"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"600.5"}
        }"#;
        let meta = parse_ffprobe_json(&json(j)).unwrap();
        assert_eq!(meta.video_codec.as_deref(), Some("hevc"));
        assert_eq!(meta.video_profile.as_deref(), Some("Main 10"));
        assert_eq!(meta.bit_depth, Some(10));
        assert_eq!(meta.pixel_format.as_deref(), Some("yuv420p10le"));
        assert_eq!(meta.duration_ms, Some(600500));
        assert_eq!(meta.audio_codec, None);
        assert_eq!(meta.rotation, None);
    }

    #[test]
    fn parses_multi_audio_keeps_first_track() {
        let j = r#"
        {
          "streams": [
            {"index":0,"codec_type":"video","codec_name":"vp9","width":1280,"height":720},
            {"index":1,"codec_type":"audio","codec_name":"opus","channels":2},
            {"index":2,"codec_type":"audio","codec_name":"aac","channels":6}
          ],
          "format": {"format_name":"matroska,webm","duration":"30","bit_rate":"800000"}
        }"#;
        let meta = parse_ffprobe_json(&json(j)).unwrap();
        assert_eq!(meta.audio_codec.as_deref(), Some("opus")); // 第一条音轨
        assert_eq!(meta.container_format.as_deref(), Some("matroska,webm"));
        assert_eq!(meta.video_codec.as_deref(), Some("vp9"));
    }

    #[test]
    fn handles_vfr_rate_zero_denominator() {
        let j = r#"
        {
          "streams": [
            {"index":0,"codec_type":"video","codec_name":"h264","avg_frame_rate":"0/0","r_frame_rate":"25/1"}
          ],
          "format": {"format_name":"matroska,webm","duration":"10.0"}
        }"#;
        let meta = parse_ffprobe_json(&json(j)).unwrap();
        // 0/0 应被丢弃，退化到 r_frame_rate=25
        assert_eq!(meta.frame_rate, Some(25.0));
    }

    #[test]
    fn audio_layout_comes_from_channel_layout_field() {
        // FB6 需求六：声道布局取 ffprobe 的 channel_layout（stereo/5.1），不再误用 language/title
        let j = r#"
        {
          "streams": [
            {"index":0,"codec_type":"video","codec_name":"h264","width":640,"height":360},
            {"index":1,"codec_type":"audio","codec_name":"aac","sample_rate":"48000","channels":2,
             "channel_layout":"stereo","tags":{"language":"eng","title":"Main"}}
          ],
          "format": {"format_name":"mov,mp4,m4a"}
        }"#;
        let meta = parse_ffprobe_json(&json(j)).unwrap();
        assert_eq!(meta.audio_layout.as_deref(), Some("stereo"));
        assert_eq!(meta.audio_channels, Some(2));
        assert_eq!(meta.audio_sample_rate, Some(48000));

        // 缺失 channel_layout → None（UI 显示「未提供」，不伪造）
        let j2 = r#"
        {
          "streams": [
            {"index":0,"codec_type":"video","codec_name":"h264"},
            {"index":1,"codec_type":"audio","codec_name":"mp3","channels":2,
             "tags":{"language":"jpn"}}
          ],
          "format": {"format_name":"mp3"}
        }"#;
        let meta2 = parse_ffprobe_json(&json(j2)).unwrap();
        assert_eq!(meta2.audio_layout, None);
    }

    #[test]
    fn corrupted_json_returns_parse_error() {
        let err = parse_ffprobe_json(&json("{ not valid json "));
        assert!(matches!(err, Err(ProbeError::Parse(_))));
    }

    // ── GPS 定位（ISO 6709）与拍摄时间 ──

    #[test]
    fn iso6709_decimal_degrees_parses() {
        let (lat, lon) = parse_iso6709_location("+30.2500+120.1670/").unwrap();
        assert!((lat - 30.25).abs() < 1e-9);
        assert!((lon - 120.167).abs() < 1e-9);
        // 南纬西经为负；无尾斜杠也合法
        let (lat, lon) = parse_iso6709_location("-33.8688-070.6693").unwrap();
        assert!((lat + 33.8688).abs() < 1e-9);
        assert!((lon + 70.6693).abs() < 1e-9);
    }

    #[test]
    fn iso6709_with_altitude_and_packed_dms() {
        // 高度尾段丢弃，只取前两段
        let (lat, lon) = parse_iso6709_location("+30.2500+120.1670+15.500/").unwrap();
        assert!((lat - 30.25).abs() < 1e-9);
        assert!((lon - 120.167).abs() < 1e-9);
        // 打包度分秒：+301500 = 30°15'00" = 30.25；+1201030 = 120°10'30"
        let (lat, lon) = parse_iso6709_location("+301500+1201030/").unwrap();
        assert!((lat - 30.25).abs() < 1e-9);
        assert!((lon - (120.0 + 10.0 / 60.0 + 30.0 / 3600.0)).abs() < 1e-9);
        // 打包度分：+3015+12010 = 30.25° 120.1667°
        let (lat, lon) = parse_iso6709_location("+3015+12010/").unwrap();
        assert!((lat - 30.25).abs() < 1e-9);
        assert!((lon - (120.0 + 10.0 / 60.0)).abs() < 1e-9);
    }

    #[test]
    fn iso6709_malformed_is_none() {
        assert!(parse_iso6709_location("").is_none());
        assert!(parse_iso6709_location("/").is_none());
        assert!(parse_iso6709_location("+").is_none());
        assert!(parse_iso6709_location("abc").is_none());
        assert!(parse_iso6709_location("+30.25").is_none()); // 缺经度段
        assert!(parse_iso6709_location("+91.0000+120.1670/").is_none()); // 纬度越界
        assert!(parse_iso6709_location("+30.2500+181.1670/").is_none()); // 经度越界
        assert!(parse_iso6709_location("+3075+12010/").is_none()); // 分 ≥60 非法
    }

    #[test]
    fn creation_time_utc_to_epoch_millis() {
        // 2024-03-15T06:30:00Z = 1710484200000 ms（UTC 绝对时刻，本地时区展示在展示层换算）
        let ms = parse_creation_time_ms("2024-03-15T06:30:00Z").unwrap();
        assert_eq!(ms, 1710484200000);
        // 带微秒小数（ffprobe 常见 6 位小数）与带偏移量形式均能解析，时刻与 UTC 形式一致/等价
        let ms2 = parse_creation_time_ms("2024-03-15T06:30:00.123456Z").unwrap();
        assert_eq!(ms2, 1710484200123);
        let ms3 = parse_creation_time_ms("2024-03-15T14:30:00+08:00").unwrap();
        assert_eq!(ms3, 1710484200000, "带时区偏移须换算为同一绝对时刻");
        // 非法格式容错
        assert!(parse_creation_time_ms("not-a-date").is_none());
        assert!(parse_creation_time_ms("").is_none());
    }

    #[test]
    fn parses_format_tags_location_and_creation_time() {
        let j = r#"
        {
          "streams": [{"index":0,"codec_type":"video","codec_name":"hevc","width":1920,"height":1080}],
          "format": {"format_name":"mov,mp4,m4a,3gp,3g2,mj2","duration":"5.0",
            "tags": {
              "major_brand":"qt  ",
              "creation_time":"2024-03-15T06:30:00.000000Z",
              "com.apple.quicktime.location":"+30.2500+120.1670/",
              "location":"+30.2500+120.1670/"
            }}
        }"#;
        let meta = parse_ffprobe_json(&json(j)).unwrap();
        assert_eq!(meta.latitude, Some(30.25));
        assert!((meta.longitude.unwrap() - 120.167).abs() < 1e-9);
        assert_eq!(meta.taken_at, Some(1710484200000));
    }

    #[test]
    fn format_tags_location_fallback_and_bad_values() {
        // 只有通用 location（无 quicktime 字段）也能解析；creation_time 非法时 taken_at 保持 None，
        // location 非法时经纬度保持 None，互不影响、不报错。
        let j = r#"
        {
          "streams": [{"index":0,"codec_type":"video","codec_name":"h264"}],
          "format": {"format_name":"mov,mp4",
            "tags": {"creation_time":"garbage", "location":"+39.9042+116.4074/"}}
        }"#;
        let meta = parse_ffprobe_json(&json(j)).unwrap();
        assert!((meta.latitude.unwrap() - 39.9042).abs() < 1e-9);
        assert!((meta.longitude.unwrap() - 116.4074).abs() < 1e-9);
        assert_eq!(meta.taken_at, None);

        let j2 = r#"
        {
          "streams": [{"index":0,"codec_type":"video","codec_name":"h264"}],
          "format": {"format_name":"mov,mp4",
            "tags": {"creation_time":"2024-03-15T06:30:00Z", "location":"not-a-location"}}
        }"#;
        let meta2 = parse_ffprobe_json(&json(j2)).unwrap();
        assert_eq!(meta2.latitude, None);
        assert_eq!(meta2.longitude, None);
        assert_eq!(meta2.taken_at, Some(1710484200000));

        // 无 tags 的普通视频：三个字段均 None，向后兼容不报错（复用既有 fixture）
        let j3 = r#"
        {
          "streams": [{"index":0,"codec_type":"video","codec_name":"h264"}],
          "format": {"format_name":"matroska,webm","duration":"1"}
        }"#;
        let meta3 = parse_ffprobe_json(&json(j3)).unwrap();
        assert_eq!(meta3.latitude, None);
        assert_eq!(meta3.longitude, None);
        assert_eq!(meta3.taken_at, None);
    }
}
