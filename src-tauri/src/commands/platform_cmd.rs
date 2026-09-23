//! 平台能力 IPC 薄壳。

use crate::error::AppResult;
use crate::services::platform::{self, PlatformCapabilities};

#[tauri::command]
pub fn get_platform_capabilities() -> AppResult<PlatformCapabilities> {
    Ok(platform::capabilities())
}

/// 在任何安装、启动、模型目录或外部进程操作之前调用。
pub fn require_managed_ollama_supported() -> AppResult<()> {
    if platform::managed_ollama_supported() {
        Ok(())
    } else {
        Err(crate::error::AppError::unsupported(
            "此平台不提供应用托管的 Ollama。请自行安装 Ollama 或其他兼容服务，再在设置中填写地址和模型。",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capability_command_returns_schema_v1() {
        assert_eq!(get_platform_capabilities().unwrap().schema_version, 1);
    }

    #[test]
    fn ollama_gate_matches_target() {
        assert_eq!(
            require_managed_ollama_supported().is_ok(),
            cfg!(target_os = "windows")
        );
    }
}
