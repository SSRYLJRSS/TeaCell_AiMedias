/** 窄窗布局判定（A3-07/A3-09 共用）。
 *
 * 只读浏览器视口宽度，不探测操作系统；SSR/测试环境无 `matchMedia` 时按「不窄」处理，
 * 让桌面默认布局保持不变。使用方必须把返回值当纯展示开关，不得由它决定业务权限。
 */
import { useEffect, useState } from "react";

export function useMediaQuery(query: string): boolean {
  const [matches, setMatches] = useState(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return false;
    return window.matchMedia(query).matches;
  });

  useEffect(() => {
    if (typeof window === "undefined" || typeof window.matchMedia !== "function") return;
    const list = window.matchMedia(query);
    const onChange = (event: MediaQueryListEvent) => setMatches(event.matches);
    setMatches(list.matches);
    // 旧 WebView 只有 addListener；两种都挂上，重复调用 setState 是幂等的。
    if (typeof list.addEventListener === "function") {
      list.addEventListener("change", onChange);
      return () => list.removeEventListener("change", onChange);
    }
    list.addListener(onChange);
    return () => list.removeListener(onChange);
  }, [query]);

  return matches;
}

/** 助手页在窄窗（< 1100px）收起左侧历史，把宽度让给对话与右侧工作台。 */
export const NARROW_ASSISTANT_QUERY = "(max-width: 1099px)";
