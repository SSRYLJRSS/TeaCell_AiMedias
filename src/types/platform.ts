/** 平台能力类型（三端复核 R0）：与后端 services/platform.rs 的 PlatformCapabilities 对齐。 */

export type PlatformOs = "windows" | "macos" | "linux" | "unknown";
export type PlatformArch = "x86_64" | "aarch64" | "unknown";
export type PrimaryModifier = "ctrl" | "meta";

/**
 * 静态平台能力。只表示编译期确定的能力，不表示 FFmpeg/密钥环/AI 服务当前是否可用
 * （运行时健康由各自服务报告）。
 */
export interface PlatformCapabilities {
  schemaVersion: 1;
  os: PlatformOs;
  arch: PlatformArch;
  /** 仅 Windows 提供应用管理的 Ollama（用户批准的唯一平台差异）。 */
  managedOllama: boolean;
  /** macOS 使用原生窗口按钮。 */
  nativeWindowControls: boolean;
  /** 主修饰键：macOS 为 meta（⌘），其余 ctrl。 */
  primaryModifier: PrimaryModifier;
  /** 迁移协议版本；M4 验收前为 null。 */
  libraryTransferVersion: 1 | null;
}
