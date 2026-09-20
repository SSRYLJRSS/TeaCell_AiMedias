/** 平台能力命令封装（对应 commands/platform_cmd.rs，三端复核 R0）。 */
import { invoke } from "./client";
import type { PlatformCapabilities } from "@/types/platform";

/** 读取当前构建目标的静态平台能力。启动时加载一次，缓存到 platformStore。 */
export function getPlatformCapabilities(): Promise<PlatformCapabilities> {
  return invoke<PlatformCapabilities>("get_platform_capabilities");
}
