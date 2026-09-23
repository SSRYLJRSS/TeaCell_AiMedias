/** 平台能力 IPC 封装；页面和组件不得直接 invoke。 */
import { invoke } from "./client";
import type { PlatformCapabilities } from "@/types/platform";

export function getPlatformCapabilities(): Promise<PlatformCapabilities> {
  return invoke<PlatformCapabilities>("get_platform_capabilities");
}
