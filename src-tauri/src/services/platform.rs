//! 平台能力入口。
//!
//! 这里只返回编译目标决定的静态能力，不探测文件系统、网络、凭据或外部进程。
//! 运行时健康由各自服务负责；前端通过一个统一 DTO 消费平台差异。

use serde::Serialize;

/// 前端消费的平台能力 DTO。字段名与 `src/types/platform.ts` 对齐。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlatformCapabilities {
    pub schema_version: u32,
    pub os: &'static str,
    pub arch: &'static str,
    /// 仅 Windows 提供应用托管的 Ollama 安装、启动和模型管理。
    pub managed_ollama: bool,
    /// 兼容代理默认变体：Linux 使用 VP8/WebM，其余目标使用 H.264/MP4。
    pub preferred_video_proxy: &'static str,
}

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

pub const fn current_arch() -> &'static str {
    if cfg!(target_arch = "x86_64") {
        "x86_64"
    } else if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "unknown"
    }
}

pub const fn managed_ollama_supported() -> bool {
    cfg!(target_os = "windows")
}

pub const fn preferred_video_proxy() -> &'static str {
    if cfg!(target_os = "linux") {
        "vp8_webm"
    } else {
        "h264_mp4"
    }
}

/// 纯函数：不访问任何 OS 资源。
pub fn capabilities() -> PlatformCapabilities {
    PlatformCapabilities {
        schema_version: 1,
        os: current_os(),
        arch: current_arch(),
        managed_ollama: managed_ollama_supported(),
        preferred_video_proxy: preferred_video_proxy(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_are_consistent_with_target() {
        let c = capabilities();
        assert_eq!(c.schema_version, 1);
        assert_eq!(c.managed_ollama, cfg!(target_os = "windows"));
        assert_eq!(
            c.preferred_video_proxy,
            if cfg!(target_os = "linux") {
                "vp8_webm"
            } else {
                "h264_mp4"
            }
        );
        assert_ne!(c.os, "");
        assert_ne!(c.arch, "");
    }
}
