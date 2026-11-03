/** 助手消息列表（A3-04 / WF-1）。
 *
 * 只渲染「本进程真实收到的事件」投影：
 * - 消息增量由 store 按 (instance, runId, sequence) 去重后写入，这里不做二次拼接；
 * - 助手输出用 Markdown 渲染（块级 memo 降低流式重排），用户输入保持纯文本；
 * - 终态细分三态：正常完成 / 主动停止 / 被中断/失败，避免「停止=失败」的误判；
 * - 消息很多时启用虚拟列表（阈值以上），少量消息保持普通渲染，确保可访问性与测试稳定；
 * - 尾部跟随只在用户本就贴底时发生，不抢已向上翻看的位置。
 */
import { memo, useEffect, useRef, useState, type ReactNode } from "react";
import clsx from "clsx";
import { useVirtualizer } from "@tanstack/react-virtual";
import type { AgentMessage } from "@/stores/agentSessionStore";
import MarkdownRenderer from "@/components/markdown/MarkdownRenderer";

/** 超过该条数启用虚拟列表：少量消息全量渲染更简单、更稳，也利于测试与读屏。 */
const VIRTUAL_THRESHOLD = 50;

const STATUS_HINT: Record<AgentMessage["status"], string | null> = {
  streaming: "正在回答…",
  complete: null,
  aborted: "已停止回答",
  failed: "本轮未能完成",
};

export default function MessageList({
  messages,
  emptyHint,
  interrupted = false,
  onRetry,
  onContinue,
}: {
  messages: AgentMessage[];
  emptyHint: string;
  /** 本轮是否因 Host 中断（而非用户主动停止）：用于把「已停止」细分为「被中断」。 */
  interrupted?: boolean;
  onRetry?: () => void;
  onContinue?: () => void;
}) {
  const scrollRef = useRef<HTMLDivElement | null>(null);
  const last = messages.at(-1);
  const virtual = messages.length > VIRTUAL_THRESHOLD;

  const virtualizer = useVirtualizer({
    count: messages.length,
    getScrollElement: () => scrollRef.current,
    estimateSize: () => 96,
    overscan: 6,
    enabled: virtual,
  });

  useEffect(() => {
    const el = scrollRef.current;
    if (!el) return;
    const distance = el.scrollHeight - el.scrollTop - el.clientHeight;
    // 用户已向上翻看时不强行贴底，避免打断阅读。
    if (messages.length > 1 && distance > 160) return;
    if (virtual) {
      virtualizer.scrollToIndex(messages.length - 1, { align: "end" });
    } else {
      el.scrollTop = el.scrollHeight;
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [messages.length, last?.text.length, virtual]);

  return (
    <div
      ref={scrollRef}
      className="flex flex-1 flex-col overflow-y-auto px-4 py-4"
      role="log"
      aria-label="对话消息"
    >
      {messages.length === 0 ? (
        <div className="flex flex-1 items-center justify-center text-center">
          <p className="max-w-[420px] whitespace-pre-wrap text-sm leading-6 text-[var(--color-text-tertiary)]">
            {emptyHint}
          </p>
        </div>
      ) : virtual ? (
        <div
          style={{ height: virtualizer.getTotalSize(), position: "relative", width: "100%", maxWidth: 760, margin: "0 auto" }}
        >
          {virtualizer.getVirtualItems().map((item) => {
            const message = messages[item.index];
            return (
              <div
                key={message.id}
                data-index={item.index}
                ref={virtualizer.measureElement}
                style={{ position: "absolute", top: 0, left: 0, width: "100%", transform: `translateY(${item.start}px)` }}
              >
                <MessageRow
                  message={message}
                  streaming={item.index === messages.length - 1 && message.status === "streaming"}
                  interrupted={interrupted}
                  onRetry={onRetry}
                  onContinue={onContinue}
                />
              </div>
            );
          })}
        </div>
      ) : (
        <ol className="mx-auto flex w-full max-w-[760px] flex-col gap-4">
          {messages.map((message, index) => (
            <li key={message.id}>
              <MessageRow
                message={message}
                streaming={index === messages.length - 1 && message.status === "streaming"}
                interrupted={interrupted}
                onRetry={onRetry}
                onContinue={onContinue}
              />
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}

const MessageRow = memo(function MessageRow({
  message,
  streaming,
  interrupted,
  onRetry,
  onContinue,
}: {
  message: AgentMessage;
  streaming: boolean;
  interrupted: boolean;
  onRetry?: () => void;
  onContinue?: () => void;
}) {
  const isUser = message.role === "user";
  const hint =
    message.status === "aborted" && interrupted ? "回答被中断" : STATUS_HINT[message.status];
  const showRetry = message.status === "failed" && onRetry;
  const showContinue = message.status === "aborted" && interrupted && onContinue;

  let body: ReactNode;
  if (isUser) {
    body = message.text;
  } else if (!message.text) {
    body = message.status === "streaming" ? "…" : "";
  } else if (streaming) {
    body = <StreamingMarkdown text={message.text} />;
  } else {
    body = <MarkdownRenderer text={message.text} />;
  }

  return (
    <div className={clsx("flex flex-col gap-1", isUser ? "items-end" : "items-start")}>
      <div
        className={clsx(
          "max-w-[min(640px,100%)] break-words rounded-[var(--radius-control)] px-3 py-2 text-sm leading-6",
          isUser
            ? "whitespace-pre-wrap bg-[var(--color-surface)] text-[var(--color-text)]"
            : "md-assistant text-[var(--color-text)]",
        )}
      >
        {body}
      </div>
      {(hint || showRetry || showContinue) && (
        <div className="flex items-center gap-2">
          {hint && (
            <span
              role={message.status === "failed" ? "alert" : undefined}
              className={clsx(
                "text-[11px]",
                message.status === "failed"
                  ? "text-[var(--color-danger)]"
                  : message.status === "aborted" && interrupted
                    ? "text-[var(--color-status)]"
                    : "text-[var(--color-text-tertiary)]",
              )}
            >
              {hint}
            </span>
          )}
          {showRetry && (
            <button
              type="button"
              className="text-[11px] text-[var(--color-accent)] underline-offset-2 hover:underline"
              onClick={onRetry}
            >
              重试
            </button>
          )}
          {showContinue && (
            <button
              type="button"
              className="text-[11px] text-[var(--color-accent)] underline-offset-2 hover:underline"
              onClick={onContinue}
            >
              继续
            </button>
          )}
        </div>
      )}
    </div>
  );
});

/** 流式最后一块：用 rAF 合并高频文本更新，最多每帧刷新一次，减少长回答抖动。 */
function StreamingMarkdown({ text }: { text: string }) {
  const [display, setDisplay] = useState(text);
  const latest = useRef(text);
  latest.current = text;

  useEffect(() => {
    if (typeof requestAnimationFrame !== "function") {
      setDisplay(text);
      return;
    }
    const raf = requestAnimationFrame(() => setDisplay(latest.current));
    return () => cancelAnimationFrame(raf);
  }, [text]);

  return <MarkdownRenderer text={display} />;
}
