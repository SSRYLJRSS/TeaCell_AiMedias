/** 把一段 Markdown 拆成「块」，供流式渲染复用。
 *
 * 目标：流式增量时只有最后一块在变，其余块可以按文本 memo 跳过重解析，
 * 避免整篇文档每个 delta 都重新 parse + 重排（这是长回答卡顿/闪烁的主因之一）。
 *
 * 规则：按空行分段，但围栏代码块（``` / ~~~）内部不拆，保证表格与代码块完整。
 * 只做「渲染分块」，不改原文语义；不完整代码块（流式中途）会被当成一块继续渲染。
 */
export function splitBlocks(text: string): string[] {
  if (!text) return [];
  const lines = text.split(/\r?\n/);
  const blocks: string[] = [];
  let buf: string[] = [];
  let fence: string | null = null;

  const flush = () => {
    if (buf.length === 0) return;
    const joined = buf.join("\n");
    if (joined.trim() !== "") blocks.push(joined);
    buf = [];
  };

  for (const line of lines) {
    const fenceMatch = line.match(/^\s{0,3}(`{3,}|~{3,})/);
    if (fence !== null) {
      buf.push(line);
      // 只有同字符、长度足够的闭合围栏才结束代码块，避免 ``` 里的 ``` 误判。
      if (fenceMatch && line.trimStart().startsWith(fence)) {
        fence = null;
        flush();
      }
      continue;
    }
    if (fenceMatch) {
      flush();
      fence = fenceMatch[1];
      buf.push(line);
      continue;
    }
    if (line.trim() === "") {
      flush();
      continue;
    }
    buf.push(line);
  }
  flush();
  return blocks;
}
