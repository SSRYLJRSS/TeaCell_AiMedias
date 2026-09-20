//! 平台能力命令（三端复核 R0）：返回静态能力 DTO。
//!
//! 薄壳：不访问数据库、网络、安装目录或外部进程；仅转发 `services::platform::capabilities()`。

use crate::error::AppResult;
use crate::services::platform::{self, PlatformCapabilities};

/// 返回当前构建目标的静态平台能力。启动时前端加载一次并缓存到 platformStore。
#[tauri::command]
pub fn get_platform_capabilities() -> AppResult<PlatformCapabilities> {
    Ok(platform::capabilities())
}

/// R1 共享闸：托管 Ollama 只在 Windows 支持；其余平台返回 unsupported，
/// 文案指向在线服务配置。所有管理类命令在任何网络/文件/进程操作**之前**调用它。
pub fn require_managed_ollama_supported() -> AppResult<()> {
    if platform::managed_ollama_supported() {
        Ok(())
    } else {
        Err(crate::error::AppError::unsupported(
            "此平台不提供本地模型安装与管理。请自行安装并启动 Ollama 或其他兼容服务，再在「在线服务」填写地址和模型。",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_command_returns_schema_v1() {
        let c = get_platform_capabilities().unwrap();
        assert_eq!(c.schema_version, 1);
    }

    #[test]
    fn managed_ollama_gate_matches_platform() {
        let r = require_managed_ollama_supported();
        if cfg!(target_os = "windows") {
            assert!(r.is_ok(), "Windows 应支持托管 Ollama");
        } else {
            assert!(r.is_err(), "非 Windows 应返回 unsupported");
        }
    }
}
