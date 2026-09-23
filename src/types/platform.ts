/** 平台能力类型（三端复核 R0）：与后端 services/platform.rs 的 PlatformCapabilities 对齐。 */

export type PlatformOs = "windows" | "macos" | "linux" | "unknown";
export type PlatformArch = "x86_64" | "aarch64" | "unknown";
export type PreferredVideoProxy = "h264_mp4" | "vp8_webm";

/**
 * 静态平台能力。只表示编译期确定的能力，不表示 FFmpeg/密钥环/AI 服务当前是否可用
 * （运行时健康由各自服务报告）。只保留当前实际消费的字段。
 */
export interface PlatformCapabilities {
  schemaVersion: 1;
  os: PlatformOs;
  arch: PlatformArch;
  /** 仅 Windows 提供应用管理的 Ollama（用户批准的唯一平台差异）。 */
  managedOllama: boolean;
  /** 首选视频兼容代理变体：Windows/macOS 用 H.264 MP4，Linux 用 VP8 WebM。 */
  preferredVideoProxy: PreferredVideoProxy;
}
