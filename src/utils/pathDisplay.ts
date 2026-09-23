/**
 * 按后端平台能力取展示文件名。Unix 下反斜杠是合法文件名字符，不能统一用 /[\\/]/ 拆分。
 * `os` 可显式传入以便纯函数测试；省略时读取启动缓存。
 */
import { usePlatformStore } from "@/stores/platformStore";
import type { PlatformCapabilities } from "@/types/platform";

type OsName = PlatformCapabilities["os"] | undefined;

export function displayBasename(path: string, os?: OsName): string {
  if (!path) return path;
  const resolvedOs = os ?? usePlatformStore.getState().capabilities?.os;
  const parts = path.split(resolvedOs === "windows" ? /[\\/]/ : /\//);
  const last = parts[parts.length - 1];
  return last ? last : path;
}

export function useDisplayBasename(path: string): string {
  const os = usePlatformStore((state) => state.capabilities?.os);
  return displayBasename(path, os);
}
