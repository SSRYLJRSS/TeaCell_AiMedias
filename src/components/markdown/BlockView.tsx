/** 单个 Markdown 块的渲染单元。
 *
 * - 静态依赖只用 react-markdown + remark-gfm（GFM 表格/任务列表/删除线/自动链接）；
 * - 数学公式（remark-math + rehype-katex + katex 样式）**按需动态加载**：
 *   只有块里出现 `$…$` / `$$…$$` / `\(` / `\[` 时才 import，避免把 katex 打进首屏；
 * - 不启用 rehype-raw：模型输出的原始 HTML 一律按文本显示（安全红线）；
 * - 用 memo(text) 让流式期间未变化的块跳过重解析。
 */
import { memo, useEffect, useState, type ComponentProps } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import remarkGfm from "remark-gfm";
import CodePre from "./CodeBlock";

type MdProps = ComponentProps<typeof ReactMarkdown>;
type RemarkList = NonNullable<MdProps["remarkPlugins"]>;
type RehypeList = NonNullable<MdProps["rehypePlugins"]>;

const HAS_MATH = /(\$\$[\s\S]*?\$\$|\$[^$\n]+\$|\\\(|\\\[)/;

const COMPONENTS: Components = {
  pre: CodePre,
  a: ({ children, href, title }) => (
    <a href={href} title={title} target="_blank" rel="noreferrer noopener">
      {children}
    </a>
  ),
};

function BlockView({ text }: { text: string }) {
  const [math, setMath] = useState<{ remark: RemarkList; rehype: RehypeList } | null>(null);

  useEffect(() => {
    if (math || !HAS_MATH.test(text)) return;
    let cancelled = false;
    Promise.all([import("remark-math"), import("rehype-katex"), import("katex/dist/katex.min.css")])
      .then(([remarkMath, rehypeKatex]) => {
        if (cancelled) return;
        setMath({ remark: [remarkMath.default], rehype: [rehypeKatex.default] });
      })
      .catch(() => {
        /* 公式插件加载失败时保持纯文本渲染，不阻塞正文 */
      });
    return () => {
      cancelled = true;
    };
  }, [math, text]);

  return (
    <ReactMarkdown
      remarkPlugins={math ? [remarkGfm, ...math.remark] : [remarkGfm]}
      rehypePlugins={math ? math.rehype : undefined}
      components={COMPONENTS}
    >
      {text}
    </ReactMarkdown>
  );
}

export default memo(BlockView, (prev, next) => prev.text === next.text);
