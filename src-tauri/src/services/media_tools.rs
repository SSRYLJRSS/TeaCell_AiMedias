//! 媒体工具解析（三端复核 §8）：ffmpeg / ffprobe 的**唯一**解析入口。
//!
//! 设计边界：
//! - 返回工具的**绝对路径 + 版本 + 来源**（sidecar / PATH），不再让各处各自 `Command::new("ffmpeg")`；
//! - 发布包优先使用随包 sidecar（`current_exe` 同目录下的 `ffmpeg`/`ffprobe`），
//!   即便系统 PATH 为空也能探测视频与生成代理；**仅开发模式**允许 PATH fallback；
//! - ffmpeg 与 ffprobe **分开缓存**（此前单槽 name 缓存会被交替探测互相覆盖）；
//!   缓存键包含工具绝对路径与版本，升级/换源后自动失效；
//! - 纯 std::process 探测，不访问数据库、网络。
//!
//! 与 `services/video.rs` 的关系：video.rs 的 ffprobe/ffmpeg 调用一律经本模块拿绝对路径，
//! 不再直接依赖 PATH 名称。

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// 版本探测总超时（P1-04）：sidecar 损坏 / 被安全软件挂起 / 网络盘阻塞 / 恶意替代文件不退出时，
/// 探测线程不得永久卡死。3s 足够任何正常 ffmpeg/ffprobe 打印 `-version` 首行。
const PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// 版本输出读取上限（P1-04）：只需首行即可，限制内存防止恶意工具输出超大内容。
const PROBE_OUTPUT_CAP: usize = 64 * 1024;

/// 探测轮询间隔：try_wait 忙等步长。
const PROBE_POLL: Duration = Duration::from_millis(20);

/// 工具解析结果：绝对路径 + 版本首行 + 来源 + 文件指纹。
#[derive(Debug, Clone)]
pub struct ResolvedTool {
    /// 可执行文件的绝对路径（sidecar 命中时是包内路径；PATH 命中时是 `which` 解析的绝对路径）。
    pub path: PathBuf,
    /// `-version` 首行（供构建日志与缓存键；解析失败为空串）。
    pub version: String,
    /// 来源：`sidecar`（随包）| `path`（系统 PATH，仅开发模式）。
    pub source: ToolSource,
    /// 文件指纹（size + mtime 毫秒）：工具被替换 / mtime 或 size 变化时缓存自动失效。
    pub fingerprint: ToolFingerprint,
}

/// 工具文件指纹：不读全文件，只取 size + 高精度 mtime；替换/升级后自动变化。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ToolFingerprint {
    pub size: u64,
    /// 修改时间（毫秒；无法取得时为 0）。
    pub mtime_ms: i64,
}

/// 版本探测失败原因（P1-04）：区分「进程失败/不存在」与「超时被终止」。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeFailure {
    /// spawn 失败、退出码非 0、或无有效输出。
    Unavailable,
    /// 超过 PROBE_TIMEOUT 被 kill/wait 回收。
    Timeout,
}

/// 读取文件指纹（size + mtime 毫秒）。任一失败降级为 0，不阻断探测。
fn file_fingerprint(path: &std::path::Path) -> ToolFingerprint {
    let Ok(md) = std::fs::metadata(path) else {
        return ToolFingerprint::default();
    };
    let mtime_ms = md
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);
    ToolFingerprint {
        size: md.len(),
        mtime_ms,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSource {
    /// 随应用分发的 sidecar（生产首选）。
    Sidecar,
    /// 系统 PATH（仅开发模式允许）。
    Path,
}

impl ToolSource {
    pub fn as_str(self) -> &'static str {
        match self {
            ToolSource::Sidecar => "sidecar",
            ToolSource::Path => "path",
        }
    }
}

/// 媒体工具（ffmpeg / ffprobe）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaTool {
    Ffmpeg,
    Ffprobe,
}

impl MediaTool {
    /// 无扩展名的基础可执行名（Windows 由 `exe_name` 补 `.exe`）。
    fn base_name(self) -> &'static str {
        match self {
            MediaTool::Ffmpeg => "ffmpeg",
            MediaTool::Ffprobe => "ffprobe",
        }
    }

    /// 平台可执行名（Windows 追加 `.exe`）。
    fn exe_name(self) -> String {
        if cfg!(target_os = "windows") {
            format!("{}.exe", self.base_name())
        } else {
            self.base_name().to_string()
        }
    }
}

/// 解析缓存 TTL：与旧 video.rs 一致（PATH/文件系统探测不必每次请求都跑）。
const RESOLVE_TTL: Duration = Duration::from_secs(60);

/// ffmpeg / ffprobe **各自独立**的解析缓存（此前单槽 name 缓存会被交替探测互相覆盖）。
static FFMPEG_CACHE: Mutex<Option<(Instant, Option<ResolvedTool>)>> = Mutex::new(None);
static FFPROBE_CACHE: Mutex<Option<(Instant, Option<ResolvedTool>)>> = Mutex::new(None);

fn cache_for(tool: MediaTool) -> &'static Mutex<Option<(Instant, Option<ResolvedTool>)>> {
    match tool {
        MediaTool::Ffmpeg => &FFMPEG_CACHE,
        MediaTool::Ffprobe => &FFPROBE_CACHE,
    }
}

/// sidecar 候选路径：`current_exe` 同目录下的平台可执行名。
/// Tauri `externalBin` 打包后 sidecar 被重命名为去掉 target-triple 的名字并置于 exe 同目录。
fn sidecar_candidate(tool: MediaTool) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?;
    let cand = dir.join(tool.exe_name());
    cand.is_file().then_some(cand)
}

/// 运行 `<path> -version` 取首行版本，带硬超时（P1-04）。
///
/// 用 `spawn()` 而非 `output()`：`output()` 无超时，恶意/损坏/被挂起的工具会永久卡住调用线程。
/// 这里：spawn → 独立线程排空 stdout（限 `PROBE_OUTPUT_CAP`，防管道阻塞与超大输出）→
/// 主线程 try_wait 轮询到 `PROBE_TIMEOUT` → 超时则 kill + wait 回收子进程，绝不留僵尸。
/// stderr 直接丢弃（null），不占管道。
fn probe_version(path: &std::path::Path) -> Result<String, ProbeFailure> {
    let mut cmd = Command::new(path);
    cmd.arg("-version");
    run_probe_with_timeout(cmd)
}

/// 探测执行核（可注入 `Command` 供测试用便携假工具）：spawn + 限量排空 + 硬超时 kill/wait。
fn run_probe_with_timeout(mut cmd: Command) -> Result<String, ProbeFailure> {
    let mut child = match cmd
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(_) => return Err(ProbeFailure::Unavailable),
    };

    // 独立线程排空 stdout（限量读取）：避免子进程写满管道后阻塞、也避免超大输出撑爆内存。
    let stdout = child.stdout.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut out) = stdout {
            let mut chunk = [0u8; 8 * 1024];
            while buf.len() < PROBE_OUTPUT_CAP {
                match out.read(&mut chunk) {
                    Ok(0) => break,
                    Ok(n) => {
                        let room = PROBE_OUTPUT_CAP - buf.len();
                        buf.extend_from_slice(&chunk[..n.min(room)]);
                    }
                    Err(_) => break,
                }
            }
            // 读满上限后继续把管道读空（丢弃），让子进程不会因写阻塞而不退出。
            let mut sink = [0u8; 8 * 1024];
            while let Ok(n) = out.read(&mut sink) {
                if n == 0 {
                    break;
                }
            }
        }
        buf
    });

    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let buf = reader.join().unwrap_or_default();
                if !status.success() {
                    return Err(ProbeFailure::Unavailable);
                }
                let text = String::from_utf8_lossy(&buf);
                let first = text.lines().next().unwrap_or("").trim().to_string();
                return Ok(first);
            }
            Ok(None) => {
                if Instant::now() >= deadline {
                    // 超时：终止并回收子进程，绝不留僵尸。
                    let _ = child.kill();
                    let _ = child.wait();
                    // 不 join 排空线程：kill 关闭直接子进程 stdout 后它会自然退出；
                    // 若存在仍持有管道的孙进程（测试用 cmd→ping），join 会阻塞到孙进程结束，
                    // 故此处分离该线程，让探测函数在超时上限内立即返回。
                    drop(reader);
                    return Err(ProbeFailure::Timeout);
                }
                std::thread::sleep(PROBE_POLL);
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                drop(reader);
                return Err(ProbeFailure::Unavailable);
            }
        }
    }
}

/// 在系统 PATH 中查找可执行文件的绝对路径（仅开发模式使用）。
/// 不依赖外部 `which`：手动遍历 PATH，跨平台自带扩展名匹配。
fn which_in_path(tool: MediaTool) -> Option<PathBuf> {
    let exe_name = tool.exe_name();
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let cand = dir.join(&exe_name);
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// 是否为开发模式（debug 构建）。生产（release）不允许 PATH fallback。
const fn is_dev_build() -> bool {
    cfg!(debug_assertions)
}

/// 解析工具（带 TTL 缓存）。优先 sidecar；开发模式允许 PATH fallback。
/// 返回 None 表示当前不可用（生产且无 sidecar，或开发且 PATH 无）。
pub fn resolve(tool: MediaTool) -> Option<ResolvedTool> {
    // 命中未过期缓存直接返回（含"不可用"的负缓存，避免每次都跑 -version）。
    if let Ok(cache) = cache_for(tool).lock() {
        if let Some((at, val)) = &*cache {
            if at.elapsed() < RESOLVE_TTL {
                return val.clone();
            }
        }
    }
    let resolved = resolve_uncached(tool);
    if let Ok(mut cache) = cache_for(tool).lock() {
        *cache = Some((Instant::now(), resolved.clone()));
    }
    resolved
}

fn resolve_uncached(tool: MediaTool) -> Option<ResolvedTool> {
    // ① sidecar 优先（生产与开发都先看包内）。
    if let Some(path) = sidecar_candidate(tool) {
        if let Ok(version) = probe_version(&path) {
            let fingerprint = file_fingerprint(&path);
            return Some(ResolvedTool {
                path,
                version,
                source: ToolSource::Sidecar,
                fingerprint,
            });
        }
    }
    // ② 仅开发模式允许 PATH fallback；生产必须靠 sidecar（PATH 为空也要能用）。
    if is_dev_build() {
        if let Some(path) = which_in_path(tool) {
            if let Ok(version) = probe_version(&path) {
                let fingerprint = file_fingerprint(&path);
                return Some(ResolvedTool {
                    path,
                    version,
                    source: ToolSource::Path,
                    fingerprint,
                });
            }
        }
    }
    None
}

/// 工具指纹字符串（P1-05 视频代理缓存用）：`source|version|size|mtime_ms`。
/// 工具升级 / 被替换 / mtime 或 size 变化后该串必变，供代理缓存判定 stale。
/// 不可用时返回 None（调用方据此决定是否重新生成或拒绝）。
pub fn tool_fingerprint(tool: MediaTool) -> Option<String> {
    resolve(tool).map(|t| {
        format!(
            "{}|{}|{}|{}",
            t.source.as_str(),
            t.version,
            t.fingerprint.size,
            t.fingerprint.mtime_ms
        )
    })
}

/// 便捷：拿到 ffmpeg 绝对路径（不可用返回 None）。
pub fn ffmpeg_path() -> Option<PathBuf> {
    resolve(MediaTool::Ffmpeg).map(|t| t.path)
}

/// 便捷：拿到 ffprobe 绝对路径（不可用返回 None）。
pub fn ffprobe_path() -> Option<PathBuf> {
    resolve(MediaTool::Ffprobe).map(|t| t.path)
}

/// 可用性：ffmpeg 是否可解析。
pub fn ffmpeg_available() -> bool {
    resolve(MediaTool::Ffmpeg).is_some()
}

/// 可用性：ffprobe 是否可解析。
pub fn ffprobe_available() -> bool {
    resolve(MediaTool::Ffprobe).is_some()
}

/// 构建/诊断日志用的一行摘要（工具、来源、版本、路径）。
pub fn describe(tool: MediaTool) -> String {
    match resolve(tool) {
        Some(t) => format!(
            "{}: source={} version=\"{}\" path={}",
            tool.base_name(),
            t.source.as_str(),
            t.version,
            t.path.display()
        ),
        None => format!("{}: unavailable", tool.base_name()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_name_has_platform_extension() {
        let name = MediaTool::Ffmpeg.exe_name();
        if cfg!(target_os = "windows") {
            assert_eq!(name, "ffmpeg.exe");
        } else {
            assert_eq!(name, "ffmpeg");
        }
    }

    #[test]
    fn tool_source_str_is_stable() {
        assert_eq!(ToolSource::Sidecar.as_str(), "sidecar");
        assert_eq!(ToolSource::Path.as_str(), "path");
    }

    /// ffmpeg 与 ffprobe 使用**不同**缓存槽：交替解析不得互相覆盖。
    /// 无论环境是否装了工具，两次解析各自命中自己的缓存、结果类型一致（都为 Option）。
    #[test]
    fn ffmpeg_and_ffprobe_use_separate_caches() {
        let a1 = resolve(MediaTool::Ffmpeg).is_some();
        let b1 = resolve(MediaTool::Ffprobe).is_some();
        // 再次解析：应与首次一致（缓存未被对方覆盖）。
        let a2 = resolve(MediaTool::Ffmpeg).is_some();
        let b2 = resolve(MediaTool::Ffprobe).is_some();
        assert_eq!(a1, a2, "ffmpeg 解析结果不得被 ffprobe 覆盖");
        assert_eq!(b1, b2, "ffprobe 解析结果不得被 ffmpeg 覆盖");
    }

    #[test]
    fn describe_never_panics() {
        let _ = describe(MediaTool::Ffmpeg);
        let _ = describe(MediaTool::Ffprobe);
    }

    // ---- P1-04：版本探测硬超时（用便携命令注入 run_probe_with_timeout）----

    /// 便携「立即输出并退出」命令：Windows 用 cmd /c echo，Unix 用 sh -c echo。
    fn cmd_echo(line: &str) -> Command {
        if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", &format!("echo {line}")]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", &format!("echo {line}")]);
            c
        }
    }

    /// 便携「长时间运行不退出」命令：远超 PROBE_TIMEOUT。
    fn cmd_hang() -> Command {
        if cfg!(target_os = "windows") {
            // ping 本机 30 次 ≈ 30s，稳定阻塞，无需交互输入。
            let mut c = Command::new("cmd");
            c.args(["/C", "ping -n 30 127.0.0.1 > NUL"]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "sleep 30"]);
            c
        }
    }

    /// 便携「退出码非 0」命令。
    fn cmd_fail() -> Command {
        if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", "exit 3"]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "exit 3"]);
            c
        }
    }

    #[test]
    fn probe_succeeds_and_returns_first_line() {
        let out = run_probe_with_timeout(cmd_echo("ffmpeg version 6.1")).unwrap();
        assert!(out.contains("ffmpeg version 6.1"), "首行应含版本: {out}");
    }

    #[test]
    fn probe_nonzero_exit_is_unavailable() {
        assert_eq!(
            run_probe_with_timeout(cmd_fail()),
            Err(ProbeFailure::Unavailable)
        );
    }

    #[test]
    fn probe_missing_binary_is_unavailable() {
        let cmd = Command::new("definitely_not_a_real_tool_xyz_123");
        assert_eq!(run_probe_with_timeout(cmd), Err(ProbeFailure::Unavailable));
    }

    /// 关键回归：持续运行的工具必须在 PROBE_TIMEOUT 内被 kill，测试整体不得超时挂死。
    #[test]
    fn probe_hanging_tool_times_out_and_is_killed() {
        let start = Instant::now();
        let r = run_probe_with_timeout(cmd_hang());
        let elapsed = start.elapsed();
        assert_eq!(r, Err(ProbeFailure::Timeout), "长跑工具应超时");
        assert!(
            elapsed < PROBE_TIMEOUT + Duration::from_secs(5),
            "必须在超时上限附近返回，实测 {elapsed:?}"
        );
    }

    #[test]
    fn probe_output_is_capped() {
        // 输出远超 cap（Unix 用 yes 截断；Windows 用循环 echo 近似）。
        let cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            // 输出多行大内容后退出（for /L 生成 ~5000 行）。
            c.args([
                "/C",
                "for /L %i in (1,1,5000) do @echo AAAAAAAAAAAAAAAAAAAA",
            ]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", "yes AAAAAAAAAAAAAAAAAAAA | head -c 5000000"]);
            c
        };
        // 不断言具体内容，只断言不 panic、不 OOM、能在超时内返回。
        let start = Instant::now();
        let _ = run_probe_with_timeout(cmd);
        assert!(start.elapsed() < PROBE_TIMEOUT + Duration::from_secs(5));
    }
}
