//! 平台能力入口（三端复核 R0）：一个共享的静态能力计算层。
//!
//! 设计边界：
//! - 只计算**编译期/静态**能力，不访问数据库、网络、安装目录或外部进程。
//! - 不表示 FFmpeg、密钥环、AI 服务当前是否可用——运行时健康由对应服务各自报告。
//! - 前端所有页面从统一 store 读取，禁止各页面自行探测 `navigator.platform` 或直接 invoke
//!   决定后端能力。
//!
//! `managed_ollama` 是用户明确批准的唯一有意平台差异：仅 Windows 提供应用管理的 Ollama；
//! macOS/Linux 隐藏本地安装/进程/模型管理，引导用户自部署并填写兼容 API 地址。

use serde::Serialize;

/// 前端消费的平台能力 DTO。字段名 camelCase 与 TS `PlatformCapabilities` 对齐。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCapabilities {
    pub schema_version: u32,
    pub os: &'static str,
    pub arch: &'static str,
    /// 仅 Windows 提供应用管理的 Ollama（用户批准的唯一平台差异）。
    pub managed_ollama: bool,
    /// 兼容视频代理的首选变体；省略 IPC 参数时由后端按目标系统选择。
    pub preferred_video_proxy: &'static str,
    /// 当前 Tauri 配置在三端都关闭系统装饰并渲染自绘窗口控制，因此始终为 false。
    pub native_window_controls: bool,
    /// 主修饰键：macOS 为 meta（⌘），其余为 ctrl。
    pub primary_modifier: &'static str,
    /// 当前版本不支持跨系统素材库搬迁，保留 DTO 字段并始终返回 None。
    pub library_transfer_version: Option<u32>,
    /// 本平台是否提供 Agent Host（1.0 只支持 Windows x64）。纯增量字段：
    /// 为保持 R0 冻结的 schemaVersion=1 契约不变，它不参与版本号升级，
    /// 前端按可选字段消费；为 false 时界面必须禁用 Agent 入口并给出原因。
    pub agent_host: bool,
}

/// 编译期确定的操作系统名。
pub const fn current_os() -> &'static str {
    if cfg!(target_os = "windows") {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

/// 编译期确定的架构名。
pub const fn current_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    }
}

/// 仅 Windows 提供应用管理的 Ollama。
pub const fn managed_ollama_supported() -> bool {
    cfg!(target_os = "windows")
}

/// Linux 使用 VP8/WebM；Windows/macOS 使用 H.264/AAC MP4。
pub const fn preferred_video_proxy() -> &'static str {
    if cfg!(target_os = "linux") {
        "vp8_webm"
    } else {
        "h264_mp4"
    }
}

/// 计算当前构建目标的静态能力。纯函数、零副作用。
pub fn capabilities() -> PlatformCapabilities {
    let os = current_os();
    PlatformCapabilities {
        schema_version: 1,
        os,
        arch: current_arch(),
        managed_ollama: managed_ollama_supported(),
        preferred_video_proxy: preferred_video_proxy(),
        // tauri.conf.json 使用 decorations=false，不能把自绘标题栏误报成原生窗口按钮。
        native_window_controls: false,
        primary_modifier: if cfg!(target_os = "macos") {
            "meta"
        } else {
            "ctrl"
        },
        // 首阶段不承诺跨系统素材库搬迁。
        library_transfer_version: None,
        // 与 agent_runtime 的平台门保持一致，避免两处各写一份判断。
        agent_host: cfg!(all(target_os = "windows", target_arch = "x86_64")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_internally_consistent() {
        let c = capabilities();
        assert_eq!(c.schema_version, 1);
        // managed_ollama 当且仅当 Windows
        assert_eq!(c.managed_ollama, cfg!(target_os = "windows"));
        assert_eq!(c.preferred_video_proxy, preferred_video_proxy());
        assert_eq!(
            c.preferred_video_proxy,
            if cfg!(target_os = "linux") {
                "vp8_webm"
            } else {
                "h264_mp4"
            }
        );
        // 所有首发目标都使用 decorations=false + 自绘标题栏，不能声称有系统原生按钮。
        assert!(!c.native_window_controls);
        // Agent Host 只在 Windows x64 提供；其他平台必须报 false，供界面禁用入口。
        assert_eq!(
            c.agent_host,
            cfg!(all(target_os = "windows", target_arch = "x86_64"))
        );
        // 主修饰键与平台一致
        if cfg!(target_os = "macos") {
            assert_eq!(c.primary_modifier, "meta");
        } else {
            assert_eq!(c.primary_modifier, "ctrl");
        }
        // 首阶段不承诺跨系统素材库搬迁。
        assert_eq!(c.library_transfer_version, None);
        // os/arch 不为 unknown（在受支持首发目标上编译时）
        assert_ne!(c.os, "");
    }

    #[test]
    fn os_matches_managed_ollama() {
        // managed_ollama 与 current_os 一致：只有 windows 为 true
        assert_eq!(managed_ollama_supported(), current_os() == "windows");
    }
}
