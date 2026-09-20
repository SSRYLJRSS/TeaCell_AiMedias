/**
 * 路径显示工具（三端复核 X-18）：从完整路径取"文件名"用于展示。
 *
 * 缺陷背景：各处用 `path.split(/[\\/]/).pop()` 把反斜杠也当分隔符。这在 Windows 正确，
 * 但在 Unix，反斜杠是**合法文件名字符**——`/home/a\b.jpg` 的文件名是 `a\b.jpg`，
 * 按 `[\\/]` 拆会错误截成 `b.jpg`。分隔符必须按平台判定，不能一刀切。
 *
 * 平台 OS 从 platformStore 读取（启动后静态）；store 未就绪时按"仅 `/`"这一最安全、
 * 跨平台不误伤的规则处理（Windows 路径同时含盘符与反斜杠，未就绪的极短暂窗口内
 * 顶多少截一层目录，绝不把合法文件名截断）。
 */
import { usePlatformStore } from "@/stores/platformStore";

/** 当前平台的路径分隔符正则：Windows 认 `\` 和 `/`，其余只认 `/`。 */
function separatorRegex(): RegExp {
  const os = usePlatformStore.getState().capabilities?.os;
  return os === "windows" ? /[\\/]/ : /\//;
}

/**
 * 取路径最后一段作为展示用文件名。空段（尾部分隔符）回退到原字符串。
 * 不做规范化、不访问文件系统，纯字符串处理。
 */
export function displayBasename(path: string): string {
  if (!path) return path;
  const parts = path.split(separatorRegex());
  const last = parts[parts.length - 1];
  return last && last.length > 0 ? last : path;
}
