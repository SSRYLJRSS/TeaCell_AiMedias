/** 平台能力类型（三端复核 R0）：与后端 services/platform.rs 的 PlatformCapabilities 对齐。 */

export type PlatformOs = "windows" | "macos" | "linux" | "unknown";
export type PlatformArch = "x86_64" | "aarch64" | "unknown";
export type PrimaryModifier = "ctrl" | "meta";
export type VideoProxyVariant = "h264_mp4" | "vp8_webm";

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
  /** 省略视频代理变体时由后端按平台选择；UI 不硬编码编码格式。 */
  preferredVideoProxy: VideoProxyVariant;
  /** Tauri 使用系统装饰和原生窗口按钮时为 true；当前三端统一为自绘标题栏。 */
  nativeWindowControls: boolean;
  /** 主修饰键：macOS 为 meta（⌘），其余 ctrl。 */
  primaryModifier: PrimaryModifier;
  /** 当前版本不支持跨系统素材库搬迁；保留协议字段并始终为 null。 */
  libraryTransferVersion: 1 | null;
  /**
   * 本平台是否提供 Agent Host（当前仅 Windows x64）。纯增量可选字段：
   * 为保持冻结的 schemaVersion=1 契约不做版本升级，缺失按 false 处理，
   * 此时界面必须禁用 Agent 入口并说明原因。
   */
  agentHost?: boolean;
}
