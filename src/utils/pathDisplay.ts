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

/**
 * 去掉 Windows 扩展长度前缀，供界面显示。
 *
 * 总仓根目录在后端存的是 `canonicalize` 结果，Windows 上必然带 `\\?\` 前缀
 * （后端 `utils::path::display_native_path` 专职去掉它）。直接原样显示，用户会看到
 * `\\?\F:\all` 这种像乱码的路径。这里只处理显示，不用于打开文件。
 */
export function formatPathForDisplay(path: string | null | undefined): string {
  const text = (path ?? "").trim();
  if (!text) return "";
  if (text.startsWith("\\\\?\\UNC\\")) return `\\\\${text.slice(8)}`;
  if (text.startsWith("\\\\?\\") || text.startsWith("//?/")) return text.slice(4);
  return text;
}

/** 归一化到可比较形式：去前缀、统一分隔符、去尾部分隔符、Windows 忽略大小写。 */
function normalizeForCompare(path: string): string {
  let text = formatPathForDisplay(path).replace(/\//g, "\\");
  while (text.length > 3 && text.endsWith("\\")) text = text.slice(0, -1);
  // 只有 Windows 忽略大小写；Unix 上 /A 与 /a 是两个目录。
  return usePlatformStore.getState().capabilities?.os === "windows" ? text.toLowerCase() : text;
}

/**
 * 两段路径是否指向同一位置。空路径恒不相等（「未配置」不等于任何目录）。
 *
 * 用于「用户是否真的换了目录」的判断：用选择器重选同一目录得到 `F:\all`，
 * 而库里存的是 `\\?\F:\all`，字符串不同但语义相同。
 */
export function isSamePath(a: string | null | undefined, b: string | null | undefined): boolean {
  const left = normalizeForCompare(a ?? "");
  const right = normalizeForCompare(b ?? "");
  return left.length > 0 && left === right;
}
