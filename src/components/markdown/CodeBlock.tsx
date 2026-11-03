/** 代码块渲染：shiki 高亮（懒加载）+ 复制 + 长代码折叠 + mermaid 分派。
 *
 * - 首屏不加载 shiki（体积大）：只有真正出现带语言的代码块时才 import。
 * - 高亮结果通过 ref 写 innerHTML（shiki 生成的 HTML 已转义源码，安全）；
 *   未加载/高亮失败时退化为纯文本 <pre>，绝不阻塞渲染。
 * - `pre` 元素由本组件接管，从子级 <code> 的 className 解析语言。
 */
import { isValidElement, useEffect, useRef, useState, type ReactNode } from "react";
import clsx from "clsx";
import { Check, Copy } from "lucide-react";
import MermaidBlock from "./MermaidBlock";

const MAX_COLLAPSED_LINES = 20;

/** 常见别名 → shiki 语言名，减少「未知语言直接抛错」。 */
const LANG_ALIASES: Record<string, string> = {
  ts: "typescript",
  js: "javascript",
  jsx: "jsx",
  tsx: "tsx",
  sh: "bash",
  shell: "bash",
  zsh: "bash",
  py: "python",
  rs: "rust",
  yml: "yaml",
  md: "markdown",
  text: "text",
  plaintext: "text",
};

function toText(node: ReactNode): string {
  if (node === null || node === undefined || typeof node === "boolean") return "";
  if (typeof node === "string" || typeof node === "number") return String(node);
  if (Array.isArray(node)) return node.map(toText).join("");
  if (isValidElement(node)) {
    const props = node.props as { children?: ReactNode };
    return toText(props.children);
  }
  return "";
}

function extractCode(children: ReactNode): { language?: string; code: string } | null {
  const child = Array.isArray(children) ? children[0] : children;
  if (!isValidElement(child)) return null;
  const props = child.props as { className?: string; children?: ReactNode };
  const code = toText(props.children).replace(/\n$/, "");
  const match = /language-([\w-]+)/.exec(props.className ?? "");
  return { language: match ? match[1].toLowerCase() : undefined, code };
}

/** react-markdown 的 `pre` 渲染器：转交给代码块组件。 */
export default function CodePre({ children }: { children?: ReactNode }) {
  const info = extractCode(children);
  if (!info) {
    return <pre className="md-code__body md-code__plain">{children}</pre>;
  }
  if (info.language === "mermaid") return <MermaidBlock code={info.code} />;
  return <CodeBlock language={info.language} code={info.code} />;
}

function CodeBlock({ language, code }: { language?: string; code: string }) {
  const [html, setHtml] = useState<string | null>(null);
  const [failed, setFailed] = useState(false);
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const hostRef = useRef<HTMLDivElement | null>(null);
  const lineCount = code.split("\n").length;

  useEffect(() => {
    let cancelled = false;
    setHtml(null);
    setFailed(false);
    if (!language) return;
    const lang = LANG_ALIASES[language] ?? language;
    import("shiki")
      .then(async ({ codeToHtml }) => {
        try {
          const out = await codeToHtml(code, { lang, theme: "github-dark" });
          if (!cancelled) setHtml(out);
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
  }, [code, language]);

  useEffect(() => {
    if (html && hostRef.current) hostRef.current.innerHTML = html;
  }, [html]);

  const copy = async () => {
    try {
      await navigator.clipboard.writeText(code);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1200);
    } catch {
      /* 剪贴板不可用（无权限/无安全上下文）时静默失败，不打断阅读 */
    }
  };

  const collapsible = lineCount > MAX_COLLAPSED_LINES;
  const collapsed = collapsible && !expanded;

  return (
    <div className="md-code">
      <div className="md-code__bar">
        <span className="md-code__lang">{language ?? "text"}</span>
        <div className="md-code__actions">
          {collapsible && (
            <button type="button" className="md-code__btn" onClick={() => setExpanded((v) => !v)}>
              {expanded ? "收起" : `展开 ${lineCount} 行`}
            </button>
          )}
          <button type="button" className="md-code__btn" aria-label={copied ? "已复制" : "复制代码"} onClick={() => void copy()}>
            {copied ? <Check size={12} aria-hidden="true" /> : <Copy size={12} aria-hidden="true" />}
          </button>
        </div>
      </div>
      {html && !failed ? (
        <div ref={hostRef} className={clsx("md-code__body", collapsed && "md-code__body--collapsed")} />
      ) : (
        <pre className={clsx("md-code__body md-code__plain", collapsed && "md-code__body--collapsed")}>
          <code>{code}</code>
        </pre>
      )}
    </div>
  );
}
