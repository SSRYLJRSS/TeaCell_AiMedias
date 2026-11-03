/** 助手 Markdown 渲染入口：把文本拆块后逐块渲染，块级 memo 降低流式重排成本。
 *
 * 用法：`<MarkdownRenderer text={message.text} />`，外层不需要再套 `.md-body`。
 */
import { memo, useMemo } from "react";
import { splitBlocks } from "./splitBlocks";
import BlockView from "./BlockView";

function MarkdownRenderer({ text }: { text: string }) {
  const blocks = useMemo(() => splitBlocks(text), [text]);
  return (
    <div className="md-body">
      {blocks.map((block, index) => (
        <BlockView key={index} text={block} />
      ))}
    </div>
  );
}

export default memo(MarkdownRenderer, (prev, next) => prev.text === next.text);
