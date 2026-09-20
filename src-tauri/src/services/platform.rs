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
    /// macOS 使用原生窗口按钮（红黄绿）；Windows/Linux 沿用现有控制方式。
    pub native_window_controls: bool,
    /// 主修饰键：macOS 为 meta（⌘），其余为 ctrl。
    pub primary_modifier: &'static str,
    /// 迁移协议版本；只有 M4 验收完成后才置 Some(1)。当前为 None。
    pub library_transfer_version: Option<u32>,
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

/// 计算当前构建目标的静态能力。纯函数、零副作用。
pub fn capabilities() -> PlatformCapabilities {
    let os = current_os();
    PlatformCapabilities {
        schema_version: 1,
        os,
        arch: current_arch(),
        managed_ollama: managed_ollama_supported(),
        native_window_controls: cfg!(target_os = "macos"),
        primary_modifier: if cfg!(target_os = "macos") {
            "meta"
        } else {
            "ctrl"
        },
        // M4 验收前迁移能力不对外声明为可用。
        library_transfer_version: None,
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
        // 原生窗口按钮当且仅当 macOS
        assert_eq!(c.native_window_controls, cfg!(target_os = "macos"));
        // 主修饰键与平台一致
        if cfg!(target_os = "macos") {
            assert_eq!(c.primary_modifier, "meta");
        } else {
            assert_eq!(c.primary_modifier, "ctrl");
        }
        // 迁移协议 M4 前不可用
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
