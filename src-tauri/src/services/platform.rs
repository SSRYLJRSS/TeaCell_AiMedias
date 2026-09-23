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
/// 只保留当前实际会消费的字段：不含未使用的窗口控制/修饰键/迁移能力。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCapabilities {
    pub schema_version: u32,
    pub os: &'static str,
    pub arch: &'static str,
    /// 仅 Windows 提供应用管理的 Ollama（用户批准的唯一平台差异）。
    pub managed_ollama: bool,
    /// 首选视频兼容代理变体：Windows/macOS 用 H.264 MP4，Linux 用 VP8 WebM。
    /// 前端所有 ensure/status/cancel 调用从同一 selector 取 variant，不允许组件硬编码。
    pub preferred_video_proxy: &'static str,
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

/// 首选视频兼容代理变体：Linux 缺少 H.264 专利许可，回退到 VP8/WebM；其余用 H.264 MP4。
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
        // 视频代理变体与平台一致：Linux WebM，其余 MP4
        if cfg!(target_os = "linux") {
            assert_eq!(c.preferred_video_proxy, "vp8_webm");
        } else {
            assert_eq!(c.preferred_video_proxy, "h264_mp4");
        }
        // os/arch 不为 unknown（在受支持首发目标上编译时）
        assert_ne!(c.os, "");
    }

    #[test]
    fn os_matches_managed_ollama() {
        // managed_ollama 与 current_os 一致：只有 windows 为 true
        assert_eq!(managed_ollama_supported(), current_os() == "windows");
    }
}
