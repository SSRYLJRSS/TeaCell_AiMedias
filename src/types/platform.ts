/** 后端静态平台能力 DTO（与 services/platform.rs 对齐）。 */

export type PlatformOs = "windows" | "macos" | "linux" | "unknown";
export type PlatformArch = "x86_64" | "aarch64" | "unknown";
export type PreferredVideoProxy = "h264_mp4" | "vp8_webm";

export interface PlatformCapabilities {
  schemaVersion: 1;
  os: PlatformOs;
  arch: PlatformArch;
  managedOllama: boolean;
  preferredVideoProxy: PreferredVideoProxy;
}
