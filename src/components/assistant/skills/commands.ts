/** `/` 命令面板的纯逻辑（WP2）。
 *
 * 只放数据形状与匹配/解析函数，不碰 React，方便单测。面板本身的键盘/渲染在
 * `CommandPalette.tsx`，调用方（Composer）负责把 items 的 id 映射到真实动作。
 *
 * 触发规则（互审已定）：只有当光标处「行首 token」形如 `/query`，且 query 为空
 * 或能命中某条命令/技能时才展开——避免把 `/home`、`/etc` 这类路径误当成命令。
 */

/**
 * `/` 面板只列**技能项**。权限、模型切换、清空/继续/停止等动作已从面板移除：
 * 它们各自有自己的胶囊或按钮，混进技能面板会让「/ 召唤技能」语义不清。
 */
export type PaletteItemKind = "skill";

export interface PaletteItem {
  id: string;
  kind: PaletteItemKind;
  /** 命令名列（面板首列主文本）。 */
  title: string;
  /** 右侧灰色说明。 */
  description: string;
  /** 参与匹配的额外关键词（服务名、技能英文 id 等）。 */
  keywords: string[];
  disabled?: boolean;
}

/** 从光标前文本里解析出行首 `/query` token（不含前导 `/`）。 */
export function slashToken(
  text: string,
  caret: number,
): { query: string; start: number; end: number } | null {
  if (caret < 0 || caret > text.length) return null;
  const before = text.slice(0, caret);
  // 仅匹配「行首或换行后」的一个 `/`，后面到光标处不含空白或第二个 `/`。
  const match = /(?:^|\n)\/([^\s/]*)$/.exec(before);
  if (!match) return null;
  const query = match[1];
  return { query, start: before.length - query.length - 1, end: caret };
}

function matches(item: PaletteItem, rawQuery: string): boolean {
  const q = rawQuery.toLowerCase();
  if (!q) return true;
  const hay = [item.title, item.description, ...item.keywords].join(" ").toLowerCase();
  // 先做子串匹配（名称/描述/关键词任意位置），这是最直观的过滤。
  if (hay.includes(q)) return true;
  // 再允许「跳字」子序列，但**只在标题上**做——避免把英文 id 里的字母
  // 拼出 `/etc`、`/home` 之类误命中（路径不能误当命令）。
  let i = 0;
  const title = item.title.toLowerCase();
  for (const ch of title) {
    if (ch === q[i]) i += 1;
    if (i === q.length) return true;
  }
  return false;
}

/** 过滤命令项；query 为空返回全部（截断到上限）。 */
export function filterPaletteItems(items: PaletteItem[], rawQuery: string, limit = 40): PaletteItem[] {
  return items.filter((item) => matches(item, rawQuery)).slice(0, limit);
}

/**
 * 是否应展开面板：token 必须存在；query 为空时展示全部；否则必须命中至少一项。
 * 这正是 `/home` 不误触的关键——它解析出 token 但命中不到任何命令。
 */
export function shouldOpenPalette(items: PaletteItem[], query: string): boolean {
  if (!query) return items.length > 0;
  return items.some((item) => matches(item, query));
}
