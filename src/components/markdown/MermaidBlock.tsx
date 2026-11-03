/** Mermaid 图表块：懒加载 mermaid，渲染失败回退到源码。
 *
 * CSP 约束：外部页面没有 worker-src，不能用 blob worker，因此这里走
 * `mermaid.render` 的主线程路径（mermaid v11+ 默认即可），失败时保留源码，
 * 不静默吞掉内容。`securityLevel: "strict"` 禁止图表内嵌任意 HTML。
 */
import { useEffect, useId, useRef, useState } from "react";

export default function MermaidBlock({ code }: { code: string }) {
  const rawId = useId();
  const renderId = `mermaid-${rawId.replace(/[^a-zA-Z0-9_-]/g, "")}`;
  const hostRef = useRef<HTMLDivElement | null>(null);
  const [svg, setSvg] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setSvg(null);
    setFailed(false);
    import("mermaid")
      .then(async (mod) => {
        const mermaid = mod.default;
        try {
          mermaid.initialize({ startOnLoad: false, securityLevel: "strict", theme: "dark" });
          const { svg: out } = await mermaid.render(renderId, code);
          if (!cancelled) setSvg(out);
        } catch {
          if (!cancelled) setFailed(true);
        }
      })
      .catch(() => {
        if (!cancelled) setFailed(true);
      });
    return () => {
      cancelled = true;
    };
  }, [code, renderId]);

  useEffect(() => {
    if (svg && hostRef.current) hostRef.current.innerHTML = svg;
  }, [svg]);

  if (failed) {
    return (
      <div className="md-code">
        <div className="md-code__bar">
          <span className="md-code__lang">mermaid · 无法渲染，已显示源码</span>
        </div>
        <pre className="md-code__body md-code__plain">
          <code>{code}</code>
        </pre>
      </div>
    );
  }

  if (!svg) {
    return <div className="md-mermaid md-mermaid--loading">正在渲染图表…</div>;
  }

  return <div className="md-mermaid" ref={hostRef} />;
}
